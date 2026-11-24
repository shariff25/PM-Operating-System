/**
 * The worker's side of the bot runtime API (`pmos-bots`, docs/bot-runtime-spec.md §3):
 * claim, heartbeat, events, trace, complete and fail, with the `worker` client token.
 */
import { z } from "zod";

const TaskSchema = z.object({
  id: z.string().min(1),
  bot: z.string().min(1),
  requested_by: z.string().min(1),
  text: z.string(),
  priority: z.string().default("interactive"),
  source: z.string().optional(),
});

/** A role as `pmos_contract::bots::Role` serializes it; only what the worker reads. */
const RoleSchema = z.object({
  name: z.string().min(1),
  // The Rust struct serializes a list; accept the ROLE.md space-separated form as well.
  skills: z
    .union([z.array(z.string()), z.string()])
    .transform((s) => (typeof s === "string" ? s.split(/\s+/).filter(Boolean) : s)),
  model: z.string().default("default"),
  instructions: z.string().default(""),
});

/** A bot as `pmos_contract::bots::Bot` serializes it; only what the worker reads. */
const BotSchema = z.object({
  name: z.string().min(1),
  scope: z.enum(["team", "personal"]).optional(),
  owner: z.string().optional(),
  description: z.string().optional(),
  job: z.string().default(""),
});

/** One earlier turn of the task's conversation (docs/bot-runtime-spec.md §3, Conversations). */
const TurnSchema = z.object({
  text: z.string(),
  result: z.string().nullish(),
  error: z.string().nullish(),
  status: z.string(),
  at: z.string().optional(),
});

export const ClaimSchema = z.object({
  task: TaskSchema,
  bot: BotSchema,
  role: RoleSchema,
  tools: z.array(z.string()).default([]),
  session_token: z.string().min(1),
  lease_until: z.string().optional(),
  /** The level to work at (docs/staff-principal-spec.md §1); an older runtime sends none. */
  level: z.enum(["senior", "staff", "principal"]).default("senior"),
  /**
   * The earlier turns of this conversation (the same Slack thread or web conversation, the
   * same PM and bot), oldest first. Empty for a first message; an older runtime sends none.
   */
  history: z.array(TurnSchema).default([]),
  /**
   * The highest data class those earlier turns were built from. The tool server already
   * started this task's session at it, so the worker can neither raise nor lower what the
   * audience check sees; it only tells the model. Null without history; an older runtime
   * sends none. An unknown value reads as "restricted".
   */
  history_sensitivity: z
    .enum(["public", "internal", "restricted"])
    .nullish()
    .catch("restricted"),
  /**
   * Under `PMOS_MODEL_SOURCE=hosted` (slice H12, r6/r7 V11): the gateway's effective data-class
   * ceiling. With one below `restricted`, a master-context file above it stops the task
   * (`checkMasterContext`). Absent under any other source and from an older runtime; an
   * unknown value reads as `public`, the lowest (fail closed).
   */
  max_data_class: z.enum(["public", "internal", "restricted"]).nullish().catch("public"),
  /**
   * The marketplace bot (persona) this task runs as (docs/bot-marketplace-spec.md §6). Absent
   * (or null) for Nudge and from an older runtime. With one, `role` is the persona's role.
   * `display_name` and `title` are catalog text: the worker checks their shape again before
   * putting them in the system prompt (`personaLine`).
   */
  persona: z
    .object({
      key: z.string().min(1),
      display_name: z.string().min(1),
      title: z.string().min(1),
    })
    .nullish(),
  /**
   * What the runtime says about the gateway for this task: the model role and the headers it
   * expects on every call, among them `X-Pmos-Model-Source`, the source the task was first
   * claimed under (slice H4). A claim without that source runs no model call
   * (`taskModelSource`).
   */
  gateway: z
    .object({
      model: z.string().optional(),
      headers: z.record(z.string(), z.unknown()).default({}),
    })
    .optional(),
});

export type Claim = z.infer<typeof ClaimSchema>;
export type Turn = Claim["history"][number];
export type ClaimedTask = Claim["task"];

export interface ChecklistItem {
  item: string;
  done: boolean;
}

export type TaskEvent =
  | { kind: "status"; text: string }
  | { kind: "milestone"; text: string }
  | { kind: "checklist"; checklist: ChecklistItem[] };

export type TraceKind = "prompt" | "model_reply" | "tool_call" | "tool_result" | "policy_denial";

export interface TraceEntry {
  kind: TraceKind;
  body: unknown;
}

/** An HTTP error from the runtime, with its status, so callers can tell 404 from 503. */
export class RuntimeError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "RuntimeError";
  }
}

export interface RuntimeClientOptions {
  /** Base URL of `pmos-bots`, e.g. `http://127.0.0.1:8750` (paths start with `/v1`). */
  baseUrl: string;
  /** The `worker` client token from `PMOS_BOTS_TOKENS`. */
  token: string;
  fetch?: typeof fetch;
}

export class RuntimeClient {
  private readonly baseUrl: string;
  private readonly fetchImpl: typeof fetch;

  constructor(private readonly options: RuntimeClientOptions) {
    this.baseUrl = options.baseUrl.replace(/\/+$/, "");
    this.fetchImpl = options.fetch ?? fetch;
  }

  private async post(path: string, body: unknown, signal?: AbortSignal): Promise<Response> {
    const response = await this.fetchImpl(`${this.baseUrl}${path}`, {
      method: "POST",
      headers: {
        authorization: `Bearer ${this.options.token}`,
        "content-type": "application/json",
      },
      body: JSON.stringify(body),
      signal,
    });
    if (!response.ok) {
      const detail = (await response.text().catch(() => "")).slice(0, 300);
      throw new RuntimeError(
        response.status,
        `POST ${path}: ${response.status}${detail ? ` ${detail}` : ""}`,
      );
    }
    return response;
  }

  private taskPath(id: string, action: string): string {
    return `/v1/worker/tasks/${encodeURIComponent(id)}/${action}`;
  }

  /**
   * Long-polls for a task. Resolves `null` on `204` (nothing queued), else the response body,
   * unvalidated: parse it with `ClaimSchema`, so an unreadable claim can still be failed by id.
   */
  async claim(
    request: { worker: string; bots: string[]; wait_ms: number },
    signal?: AbortSignal,
  ): Promise<unknown> {
    const response = await this.post("/v1/worker/claim", request, signal);
    if (response.status === 204) {
      return null;
    }
    return (await response.json()) as unknown;
  }

  async heartbeat(id: string): Promise<void> {
    await this.post(this.taskPath(id, "heartbeat"), {});
  }

  async event(id: string, event: TaskEvent): Promise<void> {
    await this.post(this.taskPath(id, "events"), event);
  }

  async trace(id: string, entries: TraceEntry[]): Promise<void> {
    await this.post(this.taskPath(id, "trace"), { entries });
  }

  async complete(
    id: string,
    body: {
      result: string;
      approvals: string[];
      /** Which labelled models answered the task (slice MP7), when the gateway said. */
      models_used?: { label: string; model_sha256?: string; calls: number }[];
      /** A call was answered by a fallback model. */
      fallback_used?: boolean;
    },
  ): Promise<void> {
    await this.post(this.taskPath(id, "complete"), body);
  }

  async fail(id: string, reason: string): Promise<void> {
    await this.post(this.taskPath(id, "fail"), { reason });
  }
}
