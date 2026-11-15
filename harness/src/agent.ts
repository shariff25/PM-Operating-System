/**
 * Builds the PM OS team-service agent on Deep Agents.
 *
 * - Model: an open-weight model behind the PM OS gateway (OpenAI-compatible endpoint).
 * - Content: only what the agent needs is mounted, read-only: `/content/skills/`,
 *   `/content/master/` (the team `AGENTS.md` and this PM's own), and the templates,
 *   reviewer rubrics and playbooks the skills read. The context library is NOT
 *   mounted: it is reachable only through the tool server's `context_search`/`context_read`,
 *   which enforce the skill's data-class ceiling. Everything else the agent writes goes to
 *   per-run scratch state.
 * - Skills: `/content/skills/` (SKILL.md, loaded on demand).
 * - Master context: the team `AGENTS.md`, then the PM's personal one if present (CTX-3).
 * - Review panel: a role with the `review` skill gets one sub-agent per reviewer rubric in
 *   `content/reviewers/`, with the session's read tools only (`docs/pm-craft-spec.md` §4).
 * - Approvals: a pause before every write tool in `content/tools.yaml` (OUT-4), unless
 *   `interruptOnWrites` is off. The bot worker turns it off: its writes go to the tool server,
 *   which returns `pending_approval` and waits for the PM's Slack card instead.
 */
import {
  copyFileSync,
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  statSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import type { BaseChatModel } from "@langchain/core/language_models/chat_models";
import type { StructuredTool, StructuredToolInterface } from "@langchain/core/tools";
import type { BaseCheckpointSaver } from "@langchain/langgraph";
import { ChatOpenAI } from "@langchain/openai";
import {
  CompositeBackend,
  FilesystemBackend,
  StateBackend,
  type SubAgent,
  createDeepAgent,
} from "deepagents";
import { parse as parseYaml } from "yaml";
import { buildInterruptOn, harnessToolNames, loadToolRegistry } from "./registry.js";

/** Virtual mount point of the content repo inside the agent's filesystem. */
export const CONTENT_MOUNT = "/content/";

/**
 * Gateway model aliases. The gateway maps each role to a concrete open-weight model, so the
 * bake-off can swap models without touching skills or the harness.
 */
export const MODEL_ALIASES = {
  default: "pmos/default",
  extraction: "pmos/extraction",
  /** The PM assistant's drafting and critique. */
  reasoning: "pmos/reasoning",
} as const;
export type ModelRole = keyof typeof MODEL_ALIASES;

export interface GatewayModelOptions {
  /** Base URL of the gateway's OpenAI-compatible API, e.g. `https://gateway.pmos.internal/v1`. */
  gatewayUrl: string;
  role?: ModelRole;
  /**
   * The gateway authenticates the sandbox, not this key. OpenShell swaps the placeholder for
   * the session credential at the network boundary.
   */
  apiKey?: string;
  /**
   * Headers sent on every call, such as the gateway's `X-Pmos-User`, `X-Pmos-Run` and
   * `X-Pmos-Priority` (docs/gateway.md).
   */
  headers?: Record<string, string>;
}

export function gatewayModel({
  gatewayUrl,
  role = "default",
  apiKey = "pmos-sandbox-placeholder",
  headers,
}: GatewayModelOptions): BaseChatModel {
  return new ChatOpenAI({
    model: MODEL_ALIASES[role],
    apiKey,
    temperature: 0,
    configuration: { baseURL: gatewayUrl, defaultHeaders: headers },
  });
}

export interface PmosAgentOptions {
  /** Path to the content repo on disk (mounted read-only in the sandbox). */
  contentRoot: string;
  /** The chat model, usually `gatewayModel(...)`. */
  model: BaseChatModel;
  /** The signed-in PM's handle; loads `master/people/<user>/AGENTS.md` if it exists. */
  user?: string;
  /** Tools from the tool server, named with `harnessToolName`. */
  tools?: StructuredToolInterface[];
  checkpointer?: BaseCheckpointSaver;
  /** Standing instructions for the agent, such as a bot's role instructions and job. */
  systemPrompt?: string;
  /**
   * Only these skills are mounted under `/content/skills/` (a bot's role skills). Omit to
   * mount every skill in the content repo.
   */
  skills?: readonly string[];
  /**
   * Pause before every write tool for an in-harness approval (default `true`). Off in the
   * bot worker, where the tool server turns each write into a `pending_approval` item.
   */
  interruptOnWrites?: boolean;
  /**
   * Directory to stage mounted content in (default: the system temp directory). The worker
   * passes a per-task directory and removes it when the task ends.
   */
  stagingRoot?: string;
}

function checkHandle(user: string): void {
  if (!/^[a-z0-9][a-z0-9._-]*$/.test(user)) {
    throw new Error(`invalid user handle: ${user}`);
  }
}

export function memorySources(contentRoot: string, user?: string): string[] {
  const sources = [`${CONTENT_MOUNT}master/AGENTS.md`];
  if (user) {
    checkHandle(user);
    if (existsSync(join(contentRoot, "master", "people", user, "AGENTS.md"))) {
      sources.push(`${CONTENT_MOUNT}master/people/${user}/AGENTS.md`);
    }
  }
  return sources;
}

/**
 * Copy only the master context this PM may see (the team file and their own personal file)
 * into a fresh directory, so other PMs' personal files are never mounted. The sandbox is
 * per session, so the directory goes away with it.
 */
export function stageMasterContext(
  contentRoot: string,
  user?: string,
  stagingRoot: string = tmpdir(),
): string {
  const staged = mkdtempSync(join(stagingRoot, "pmos-master-"));
  copyFileSync(join(contentRoot, "master", "AGENTS.md"), join(staged, "AGENTS.md"));
  if (user) {
    checkHandle(user);
    const personal = join(contentRoot, "master", "people", user, "AGENTS.md");
    if (existsSync(personal)) {
      mkdirSync(join(staged, "people", user), { recursive: true });
      copyFileSync(personal, join(staged, "people", user, "AGENTS.md"));
    }
  }
  return staged;
}

/**
 * Copy only the named skills into a fresh directory, so a bot sees its role's skills and no
 * others. Throws on a name that isn't a plain skill name or has no `SKILL.md`.
 */
export function stageSkills(
  contentRoot: string,
  skills: readonly string[],
  stagingRoot: string = tmpdir(),
): string {
  const staged = mkdtempSync(join(stagingRoot, "pmos-skills-"));
  for (const skill of skills) {
    if (!/^[a-z0-9][a-z0-9-]*$/.test(skill)) {
      throw new Error(`invalid skill name: ${skill}`);
    }
    const source = join(contentRoot, "skills", skill);
    if (!existsSync(join(source, "SKILL.md"))) {
      throw new Error(`skill ${skill} not found in the content repo`);
    }
    cpSync(source, join(staged, skill), { recursive: true });
  }
  return staged;
}

/**
 * Content folders mounted read-only as they are, each only if it exists in the content repo:
 * the templates skills draft from, the reviewer rubrics and the playbooks.
 */
export const READ_ONLY_CONTENT_DIRS = ["templates", "reviewers", "playbooks"] as const;

/** The skill that runs the review panel; a role with it gets the reviewer sub-agents. */
export const REVIEW_SKILL = "review";

/** Split a markdown file into its YAML front matter and body. */
function splitFrontMatter(text: string, path: string): { meta: unknown; body: string } {
  const match = /^---\r?\n([\s\S]*?)\r?\n---[ \t]*(?:\r?\n|$)/.exec(text);
  if (!match) {
    throw new Error(`${path} has no front matter`);
  }
  return { meta: parseYaml(match[1] ?? ""), body: text.slice(match[0].length) };
}

/**
 * One review sub-agent per rubric in `content/reviewers/` (`README.md` aside): named by the
 * file stem, described by its `lens`, prompted with the rubric body, and given only
 * `readTools`, so no reviewer can write, draft or approve. Empty when there is no
 * reviewers folder.
 */
export function reviewerSubagents(
  contentRoot: string,
  readTools: readonly StructuredToolInterface[],
): SubAgent[] {
  const dir = join(contentRoot, "reviewers");
  if (!existsSync(dir)) {
    return [];
  }
  return readdirSync(dir)
    .filter((file) => file.endsWith(".md") && file !== "README.md")
    .sort()
    .map((file) => {
      const path = join(dir, file);
      const { meta, body } = splitFrontMatter(readFileSync(path, "utf8"), path);
      const lens = (meta as { lens?: unknown } | null)?.lens;
      if (typeof lens !== "string" || lens.trim() === "") {
        throw new Error(`reviewer ${path} has no lens`);
      }
      return {
        name: basename(file, ".md"),
        description: lens.trim(),
        systemPrompt: body.trim(),
        // LangChain's tool interface and class are the same at runtime.
        tools: [...readTools] as StructuredTool[],
      };
    });
}

export function createPmosAgent(options: PmosAgentOptions) {
  const contentRoot = resolve(options.contentRoot);
  const registry = loadToolRegistry(join(contentRoot, "tools.yaml"));
  const masterRoot = stageMasterContext(contentRoot, options.user, options.stagingRoot);
  const skillsRoot = options.skills
    ? stageSkills(contentRoot, options.skills, options.stagingRoot)
    : join(contentRoot, "skills");
  const tools = options.tools ?? [];

  const mounts: Record<string, FilesystemBackend> = {
    [`${CONTENT_MOUNT}skills/`]: new FilesystemBackend({ rootDir: skillsRoot, virtualMode: true }),
    [`${CONTENT_MOUNT}master/`]: new FilesystemBackend({ rootDir: masterRoot, virtualMode: true }),
  };
  for (const dir of READ_ONLY_CONTENT_DIRS) {
    const rootDir = join(contentRoot, dir);
    if (existsSync(rootDir) && statSync(rootDir).isDirectory()) {
      mounts[`${CONTENT_MOUNT}${dir}/`] = new FilesystemBackend({ rootDir, virtualMode: true });
    }
  }

  // The review panel: only for a role whose skills include `review`, and with the read
  // tools only (kind `read` in tools.yaml).
  const kinds = harnessToolNames(registry);
  const readTools = tools.filter((t) => kinds.get(t.name)?.kind === "read");
  // Without `options.skills` every skill is mounted, `review` included if the repo has it.
  const hasReview = options.skills
    ? options.skills.includes(REVIEW_SKILL)
    : existsSync(join(contentRoot, "skills", REVIEW_SKILL, "SKILL.md"));
  const subagents = hasReview ? reviewerSubagents(contentRoot, readTools) : [];

  return createDeepAgent({
    name: "pmos",
    model: options.model,
    systemPrompt: options.systemPrompt,
    tools,
    subagents,
    checkpointer: options.checkpointer,
    backend: (config) => new CompositeBackend(new StateBackend(config), mounts),
    // The content repo is read-only to the agent; OpenShell's Landlock policy enforces the
    // same thing again at the OS level.
    permissions: [
      { operations: ["write"], paths: [`${CONTENT_MOUNT}**`], mode: "deny" },
    ],
    skills: [`${CONTENT_MOUNT}skills/`],
    memory: memorySources(contentRoot, options.user),
    interruptOn: (options.interruptOnWrites ?? true) ? buildInterruptOn(registry) : undefined,
  });
}
