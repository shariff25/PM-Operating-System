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
 * - Approvals: a pause before every write tool in `content/tools.yaml` (OUT-4).
 */
import { copyFileSync, existsSync, mkdirSync, mkdtempSync } from "node:fs";
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
}

export function gatewayModel({
  gatewayUrl,
  role = "default",
  apiKey = "pmos-sandbox-placeholder",
}: GatewayModelOptions): BaseChatModel {
  return new ChatOpenAI({
    model: MODEL_ALIASES[role],
    apiKey,
    temperature: 0,
    configuration: { baseURL: gatewayUrl },
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
export function stageMasterContext(contentRoot: string, user?: string): string {
  const staged = mkdtempSync(join(tmpdir(), "pmos-master-"));
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

export function createPmosAgent(options: PmosAgentOptions) {
  const contentRoot = resolve(options.contentRoot);
  const registry = loadToolRegistry(join(contentRoot, "tools.yaml"));
  const masterRoot = stageMasterContext(contentRoot, options.user);

  return createDeepAgent({
    name: "pmos",
    model: options.model,
    tools: options.tools ?? [],
    checkpointer: options.checkpointer,
    backend: (config) =>
      new CompositeBackend(new StateBackend(config), {
        [`${CONTENT_MOUNT}skills/`]: new FilesystemBackend({
          rootDir: join(contentRoot, "skills"),
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
    interruptOn: buildInterruptOn(registry),
  });
}
