/**
 * Builds the PM OS team-service agent on Deep Agents.
 *
 * - Model: an open-weight model behind the PM OS gateway (OpenAI-compatible endpoint).
 * - Content: only what the agent needs is mounted, read-only: `/content/skills/` and
 *   `/content/master/` (the team `AGENTS.md` and this PM's own). The context library is NOT
 *   mounted: it is reachable only through the tool server's `context_search`/`context_read`,
 *   which enforce the skill's data-class ceiling. Everything else the agent writes goes to
 *   per-run scratch state.
 * - Skills: `/content/skills/` (SKILL.md, loaded on demand).
 * - Master context: the team `AGENTS.md`, then the PM's personal one if present (CTX-3).
 * - Approvals: a pause before every write tool in `content/tools.yaml` (OUT-4), unless
 *   `interruptOnWrites` is off. The bot worker turns it off: its writes go to the tool server,
 *   which returns `pending_approval` and waits for the PM's Slack card instead.
 */
import { copyFileSync, cpSync, existsSync, mkdirSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import type { BaseChatModel } from "@langchain/core/language_models/chat_models";
import type { StructuredToolInterface } from "@langchain/core/tools";
import type { BaseCheckpointSaver } from "@langchain/langgraph";
import { ChatOpenAI } from "@langchain/openai";
import {
  CompositeBackend,
  FilesystemBackend,
  StateBackend,
  createDeepAgent,
} from "deepagents";
import { buildInterruptOn, loadToolRegistry } from "./registry.js";

/** Virtual mount point of the content repo inside the agent's filesystem. */
export const CONTENT_MOUNT = "/content/";

/**
 * Gateway model aliases. The gateway maps each role to a concrete open-weight model, so the
 * bake-off can swap models without touching skills or the harness.
 */
export const MODEL_ALIASES = {
  default: "pmos/default",
  extraction: "pmos/extraction",
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

export function createPmosAgent(options: PmosAgentOptions) {
  const contentRoot = resolve(options.contentRoot);
  const registry = loadToolRegistry(join(contentRoot, "tools.yaml"));
  const masterRoot = stageMasterContext(contentRoot, options.user, options.stagingRoot);
  const skillsRoot = options.skills
    ? stageSkills(contentRoot, options.skills, options.stagingRoot)
    : join(contentRoot, "skills");

  return createDeepAgent({
    name: "pmos",
    model: options.model,
    systemPrompt: options.systemPrompt,
    tools: options.tools ?? [],
    checkpointer: options.checkpointer,
    backend: (config) =>
      new CompositeBackend(new StateBackend(config), {
        [`${CONTENT_MOUNT}skills/`]: new FilesystemBackend({
          rootDir: skillsRoot,
          virtualMode: true,
        }),
        [`${CONTENT_MOUNT}master/`]: new FilesystemBackend({
          rootDir: masterRoot,
          virtualMode: true,
        }),
      }),
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
