/**
 * The harness worker (docs/bot-runtime-spec.md §4): one long-running process per bot
 * sandbox. It claims the bot's tasks from `pmos-bots`, runs each one on a Deep Agent with the
 * tool server's MCP tools and the gateway model, reports progress and the trace as it goes,
 * and completes or fails the task.
 *
 * - Writes are never paused in the harness. They reach the tool server, which answers
 *   `pending_approval`; the worker hands those approval ids to the runtime on completion.
 * - One task's error never stops the loop: the task fails with a reason a PM can read.
 * - `stop()` (SIGTERM) stops claiming and gives the current task a grace period to finish.
 *   A task still running after that is left alone, not failed: its lease runs out and the
 *   runtime retries it (D2: resume after a restart, or fail with a stated reason).
 */
import { mkdtempSync, rmSync } from "node:fs";
import { hostname, tmpdir } from "node:os";
import { join } from "node:path";
import type { BaseChatModel } from "@langchain/core/language_models/chat_models";
import { AIMessage, type BaseMessage, HumanMessage } from "@langchain/core/messages";
import {
  MODEL_ALIASES,
  MissingSkillError,
  type ModelRole,
  ModelSourceGuard,
  createPmosAgent,
  gatewayModel,
} from "./agent.js";
import { connectMcpTools, type McpToolSet } from "./mcp-tools.js";
import { FIRST_MILESTONE, Progress, TaskTracer } from "./progress.js";
import { harnessToolNames, loadToolRegistry, type ToolKind } from "./registry.js";
import {
  type Claim,
  ClaimSchema,
  RuntimeClient,
  RuntimeError,
  type TaskEvent,
  type TraceEntry,
  type Turn,
} from "./runtime-client.js";

export type LogLevel = "info" | "warn" | "error";
export type Logger = (level: LogLevel, message: string, fields?: Record<string, unknown>) => void;

export const jsonLogger: Logger = (level, message, fields) => {
  process.stderr.write(
    `${JSON.stringify({ at: new Date().toISOString(), level, message, ...fields })}\n`,
  );
};

export interface WorkerConfig {
  /** `PMOS_BOTS_URL`: base URL of the bot runtime, e.g. `http://127.0.0.1:8750`. */
  botsUrl: string;
  /** `PMOS_WORKER_TOKEN`: the runtime's `worker` client token. */
  workerToken: string;
  /** `PMOS_WORKER_BOT`: the one bot this worker (and its sandbox) serves. */
  bot: string;
  /** `PMOS_GATEWAY_URL`: the gateway's OpenAI-compatible base URL (`/v1` is added if missing). */
  gatewayUrl: string;
  /** `PMOS_GATEWAY_TOKEN`: the gateway client token. */
  gatewayToken?: string;
  /** `PMOS_TOOLSERVER_MCP_URL`: the tool server's MCP endpoint. */
  toolserverMcpUrl: string;
  /** The content repo checkout (skills, master context, `tools.yaml`). */
  contentRoot: string;
  /** Sent as `worker` on claim. Defaults to `<hostname>-<pid>`. */
  workerId?: string;
  /** Claim long-poll, at most 25,000 ms (the runtime's cap). */
  waitMs?: number;
  heartbeatMs?: number;
  /** How long `stop()` lets a running task finish before failing it. */
  stopGraceMs?: number;
  /**
   * Graph steps a task may take (LangGraph `recursionLimit`, default 200; each model turn and
   * each tool round is a step or two). Deep Agents' own default (10,000) would let a confused
   * model hold the GPU for hours.
   */
  maxSteps?: number;
  logger?: Logger;
  fetch?: typeof fetch;
  /** The clock for the system prompt's date line (tests). Defaults to the real time. */
  now?: () => Date;
  /** Builds the chat model for a task instead of the gateway model (tests). */
  model?: (claim: Claim, headers: Record<string, string>) => BaseChatModel;
}

export interface Worker {
  /** Claims and runs tasks until `stop()`. Never rejects because of a task. */
  run(): Promise<void>;
  /** Stops claiming; finishes or fails the current task; resolves when `run()` has returned. */
  stop(): Promise<void>;
}

/** An error whose message is already written for the PM. */
export class PmFacingError extends Error {
  constructor(message: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = "PmFacingError";
  }
}

/** The gateway's OpenAI-compatible base URL: `PMOS_GATEWAY_URL`, with `/v1` if missing. */
export function gatewayBaseUrl(url: string): string {
  const trimmed = url.replace(/\/+$/, "");
  return /\/v1$/.test(trimmed) ? trimmed : `${trimmed}/v1`;
}

/** What the worker says when the model source changed while a task ran (slice H4). */
export const MODEL_SOURCE_CHANGED =
  "The model PM OS uses changed while I was working on this (model source changed), so I stopped without sending it anything more. Please ask again.";

/** What the worker says when a claim doesn't say which model source the task runs on. */
export const MODEL_SOURCE_MISSING =
  "The bot runtime didn't say which model source this task runs on, so I didn't call the model. A platform owner needs to update the bot runtime.";

const SOURCE = /^(local|hosted-test|stub|hosted:[a-z0-9]([a-z0-9-]{0,62}[a-z0-9])?)$/;

/**
 * The model source this task runs on: the one the runtime recorded at its first claim and
 * passes in the claim's gateway headers (slice H4, X4). Throws a `PmFacingError` when the claim
 * has none: the worker never calls the model without it.
 */
export function taskModelSource(claim: Claim): string {
  const source = claim.gateway?.headers?.["X-Pmos-Model-Source"];
  if (typeof source !== "string" || !SOURCE.test(source)) {
    throw new PmFacingError(MODEL_SOURCE_MISSING);
  }
  return source;
}

/**
 * Headers the gateway uses for fair sharing and its usage log (docs/gateway.md), and the model
 * source every call of the task expects (X4: the gateway refuses another before forwarding).
 */
export function gatewayHeaders(claim: Claim): Record<string, string> {
  return {
    "X-Pmos-User": claim.task.requested_by,
    "X-Pmos-Run": claim.task.id,
    "X-Pmos-Priority": claim.task.priority,
    "X-Pmos-Model-Source": taskModelSource(claim),
  };
}

const LEVEL_TITLES: Record<Claim["level"], string> = {
  senior: "Senior",
  staff: "Staff",
  principal: "Principal",
};

/** What the worker says when a persona's skill folder isn't in the sandbox (spec §6). */
export const SKILLS_NOT_INSTALLED =
  "This bot's skills aren't installed in your sandbox yet. A platform owner needs to restart it; Nudge can help meanwhile.";

/** What the worker says when a persona's catalog text fails the shape check. */
export const PERSONA_TEXT_INVALID =
  "This bot's name or title in the PM OS catalog isn't valid, so I didn't run it. A platform owner needs to check content/personas; Nudge can help meanwhile.";

/** The lint's `display-name` rule (crates/contract/src/personas.rs): ASCII words, ≤ 24. */
const DISPLAY_NAME = /^[A-Za-z0-9]+( [A-Za-z0-9]+)*$/;
const NAME = /^[a-z0-9][a-z0-9._-]*$/;

/**
 * The persona's first line: "You are Chief, the Chief of Staff on ana's PM OS team, running in
 * pm-ana." The display name and title are catalog text (reviewed by pull request, fenced by
 * the lint's character rules); they are checked again here, as plain one-line text with no
 * markup or control characters, before they go into the prompt. The persona key is never
 * interpolated. Throws a `PmFacingError` when a check fails: never a fallback to another bot.
 */
export function personaLine(claim: Claim): string {
  const persona = claim.persona;
  if (!persona) {
    throw new Error("personaLine needs a claim with a persona");
  }
  const name = persona.display_name;
  const title = persona.title;
  const nameOk = name.length <= 24 && DISPLAY_NAME.test(name);
  const titleOk =
    title.length <= 40 && title.trim() === title && !/[<>\u0000-\u001f\u007f]/.test(title);
  const who = claim.task.requested_by;
  if (!nameOk || !titleOk || !NAME.test(who) || !NAME.test(claim.bot.name)) {
    throw new PmFacingError(PERSONA_TEXT_INVALID);
  }
  return `You are ${name}, the ${title} on ${who}'s PM OS team, running in ${claim.bot.name}.`;
}

/**
 * How a persona's instructions rank (a departure from the spec text, at the security
 * reviewer's request): restrictions accumulate. The team's standing context
 * (`content/master/AGENTS.md`, loaded as memory), the PM's own bot body (`BOT.md`, kept below)
 * and the personal role's hard limits (which the tool server enforces whatever any prompt
 * says) all come before the persona role's instructions and before the "You are …" line.
 */
export const PERSONA_PRECEDENCE =
  "## What comes first\n\n" +
  "You are running as one of this PM's PM OS bots. The team's standing context " +
  "(content/master/AGENTS.md) and the limits set for this PM's own bot (below, under " +
  '"Your PM\'s bot") come before this role\'s instructions and before the line saying who you ' +
  "are. Where they disagree, follow the stricter: nothing in this role lifts a limit set there.";

/**
 * The bot's standing context: who it is, today's date (UTC, from `now`), the PM's level for a
 * personal bot (docs/staff-principal-spec.md §1), its role's instructions and its job.
 *
 * With a persona (docs/bot-marketplace-spec.md §6): the first line names the persona
 * (`personaLine`), the role's instructions are the persona's, and the PM's own bot body is
 * still included, after them and labelled as limits that keep applying, with the precedence
 * section (`PERSONA_PRECEDENCE`) saying `content/master/AGENTS.md` and that body take
 * precedence over the persona's "You are …" line and instructions.
 */
export function botSystemPrompt(claim: Claim, now: Date = new Date()): string {
  const persona = claim.persona ?? undefined;
  const parts = [
    persona
      ? personaLine(claim)
      : `You are ${claim.bot.name}, a PM OS bot, working on a request from ${claim.task.requested_by}.`,
    `Today is ${now.toISOString().slice(0, 10)}.`,
  ];
  if (claim.bot.scope === "personal") {
    parts.push(
      `Your PM works at the ${LEVEL_TITLES[claim.level]} level. Read /content/playbooks/levels.md and work at that level's scope, horizon and audience, unless the request says otherwise.`,
    );
  }
  if (persona) {
    parts.push(PERSONA_PRECEDENCE);
  }
  parts.push(claim.role.instructions.trim());
  if (claim.bot.job.trim()) {
    parts.push(
      persona
        ? `## Your PM's bot\n\nThis PM's own bot (${claim.bot.name}) is set up as below. Its limits still apply to you.\n\n${claim.bot.job.trim()}`
        : `## Your job\n\n${claim.bot.job.trim()}`,
    );
  }
  parts.push(
    "## Writes need approval\n\n" +
      "Anything you write outside PM OS is a draft until the PM approves it. A write tool " +
      'answers `"status": "pending_approval"`: that means it has not happened yet. Do not ' +
      "retry it, and in your answer say plainly what is waiting for approval.",
  );
  if (claim.history.length > 0) {
    parts.push(
      "## This conversation\n\n" +
        "The messages before the last one are this conversation's earlier turns, oldest first; " +
        "the last message is the new request. A short reply is an answer to your last question.",
    );
    if (claim.history_sensitivity === "restricted") {
      parts.push(
        "Earlier answers in this conversation drew on restricted sources. Treat them as " +
          "restricted: PM OS blocks a Slack draft built on them in a channel with anyone " +
          "outside the product team, so say so instead of drafting one there.",
      );
    }
  }
  return parts.filter(Boolean).join("\n\n");
}

/** Ids of the messages that replay earlier turns, so the final answer is never one of them. */
const HISTORY_ID = "pmos-history-";

/** What the bot said in an earlier turn: its answer, or what became of the request. */
function turnReply(turn: Turn): string {
  if (turn.result?.trim()) {
    return turn.result;
  }
  if (turn.status === "failed") {
    const why = turn.error?.trim().replace(/\.$/, "");
    return `(This request failed${why ? `: ${why}` : ""}.)`;
  }
  if (turn.status === "cancelled") {
    return "(This request was cancelled before I answered.)";
  }
  return "(No answer was recorded.)";
}

/**
 * The model's messages for a task: each earlier turn of the conversation as the PM's message
 * and the bot's reply, oldest first, then the new request last.
 */
export function taskMessages(claim: Claim): BaseMessage[] {
  const messages: BaseMessage[] = [];
  claim.history.forEach((turn, i) => {
    messages.push(new HumanMessage({ content: turn.text, id: `${HISTORY_ID}${i}-user` }));
    messages.push(new AIMessage({ content: turnReply(turn), id: `${HISTORY_ID}${i}-bot` }));
  });
  messages.push(new HumanMessage(claim.task.text));
  return messages;
}

function textOf(content: BaseMessage["content"]): string {
  if (typeof content === "string") {
    return content;
  }
  return content
    .map((block) =>
      typeof block === "object" && block !== null && block.type === "text"
        ? String((block as { text?: unknown }).text ?? "")
        : "",
    )
    .join("");
}

/** The final answer: the last AI message with text, never a replayed earlier turn. */
export function finalAnswer(messages: BaseMessage[]): string {
  for (let i = messages.length - 1; i >= 0; i -= 1) {
    const message = messages[i];
    if (message?.getType() === "ai" && !message.id?.startsWith(HISTORY_ID)) {
      const text = textOf(message.content).trim();
      if (text) {
        return text;
      }
    }
  }
  return "";
}

/**
 * How many tries the gateway made at the model server for a failed call, from its
 * `X-Pmos-Upstream-Attempts` header (docs/gateway.md). 0 when the header is absent.
 */
export function upstreamAttempts(error: { headers?: unknown } | undefined): number {
  const headers = error?.headers;
  if (headers instanceof Headers) {
    return Number(headers.get("x-pmos-upstream-attempts")) || 0;
  }
  return 0;
}

/** A reason for a failed task that a PM can read: no stack traces, no internals. */
export function pmReason(error: unknown): string {
  if (error instanceof PmFacingError) {
    return error.message;
  }
  const e = error as
    | { name?: string; message?: string; status?: number; headers?: unknown }
    | undefined;
  const name = e?.name ?? "";
  const message = (e?.message ?? String(error)).replace(/\s+/g, " ").trim();
  if (name === "GraphRecursionError" || /recursion limit/i.test(message)) {
    return "I took too many steps without finishing. Try a narrower question, or split it into parts.";
  }
  if (typeof e?.status === "number") {
    if (e.status === 503 || e.status === 429) {
      // The gateway already retried this call; say so when it did.
      return upstreamAttempts(e) > 1
        ? "The model is still busy after several tries. Please try again in a few minutes."
        : "The model is busy right now. Please try again in a few minutes.";
    }
    if (e.status === 401 || e.status === 403) {
      return "PM OS couldn't sign in to the model gateway. A platform owner needs to check this bot's gateway token.";
    }
    return `The model gateway returned an error (${e.status}). Please try again in a few minutes.`;
  }
  if (name === "APIConnectionError" || name === "APIConnectionTimeoutError") {
    return "I couldn't reach the model gateway. Please try again in a few minutes.";
  }
  const short = message.length > 200 ? `${message.slice(0, 200)}…` : message;
  return `Something went wrong while I was working on this (${short}). Please try again; if it keeps happening, tell a platform owner.`;
}

const sleep = (ms: number, signal?: AbortSignal) =>
  new Promise<void>((resolve) => {
    if (signal?.aborted) {
      resolve();
      return;
    }
    const timer = setTimeout(done, ms);
    function done() {
      clearTimeout(timer);
      signal?.removeEventListener("abort", done);
      resolve();
    }
    signal?.addEventListener("abort", done, { once: true });
  });

type AbortReason = "stopped" | "lost";

/**
 * Is this the rejection LangChain leaves behind when we abort a model call? Aborting the
 * signal we pass to `agent.invoke` makes `@langchain/core` reject an internal promise with
 * `new Error(<our abort reason>)` that nothing awaits. The task itself is handled (see
 * `raceAbort`); this stray rejection must not take the worker down with it.
 */
export function isAbortRejection(reason: unknown): boolean {
  return reason instanceof Error && (reason.message === "lost" || reason.message === "stopped");
}

class TaskAborted extends Error {
  constructor(readonly reason: AbortReason) {
    super(`task aborted: ${reason}`);
  }
}

function raceAbort<T>(promise: Promise<T>, signal: AbortSignal): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const onAbort = () => reject(new TaskAborted(signal.reason as AbortReason));
    if (signal.aborted) {
      onAbort();
      return;
    }
    signal.addEventListener("abort", onAbort, { once: true });
    promise.then(
      (value) => {
        signal.removeEventListener("abort", onAbort);
        resolve(value);
      },
      (error: unknown) => {
        signal.removeEventListener("abort", onAbort);
        reject(error);
      },
    );
  });
}

/**
 * Sends one task's events and trace to the runtime in order, without ever failing the task:
 * a lost progress message is logged, not fatal. Trace entries are batched while a send is in
 * flight.
 */
class TaskReporter {
  private chain: Promise<void> = Promise.resolve();
  private traceBuffer: TraceEntry[] = [];

  constructor(
    private readonly client: RuntimeClient,
    private readonly taskId: string,
    private readonly log: Logger,
  ) {}

  private closed = false;

  private enqueue(what: string, send: () => Promise<void>): void {
    if (this.closed) {
      // The task is over (or no longer ours): an abandoned run's late output goes nowhere.
      return;
    }
    this.chain = this.chain.then(send).catch((error: unknown) => {
      this.log("warn", `couldn't send ${what}`, {
        task: this.taskId,
        error: (error as Error).message,
      });
    });
  }

  event(event: TaskEvent): void {
    this.enqueue(`${event.kind} event`, () => this.client.event(this.taskId, event));
  }

  trace(entry: TraceEntry): void {
    this.traceBuffer.push(entry);
    if (this.traceBuffer.length === 1) {
      this.enqueue("trace", () => this.client.trace(this.taskId, this.traceBuffer.splice(0)));
    }
  }

  drain(): Promise<void> {
    return this.chain;
  }

  /** Sends what is queued, then drops anything later. */
  close(): Promise<void> {
    this.closed = true;
    return this.chain;
  }

  /**
   * The task's last word (complete or fail), after everything before it, retried a few times.
   * If it never lands, the lease expires and the runtime retries or fails the task (D2).
   */
  async finish(what: string, send: () => Promise<void>, delays = [500, 2000, 5000]): Promise<void> {
    await this.close();
    for (let attempt = 0; ; attempt += 1) {
      try {
        await send();
        return;
      } catch (error) {
        const status = error instanceof RuntimeError ? error.status : undefined;
        const retryable = status === undefined || status >= 500;
        const delay = delays[attempt];
        if (!retryable || delay === undefined) {
          this.log("error", `couldn't ${what} the task`, {
            task: this.taskId,
            error: (error as Error).message,
          });
          return;
        }
        await sleep(delay);
      }
    }
  }
}

interface RunningTask {
  controller: AbortController;
  done: Promise<void>;
}

export function createWorker(config: WorkerConfig): Worker {
  const log = config.logger ?? jsonLogger;
  const workerId = config.workerId ?? `${hostname()}-${process.pid}`;
  const waitMs = Math.min(config.waitMs ?? 25_000, 25_000);
  const heartbeatMs = config.heartbeatMs ?? 30_000;
  const stopGraceMs = config.stopGraceMs ?? 10_000;
  const maxSteps = config.maxSteps ?? 200;
  const client = new RuntimeClient({
    baseUrl: config.botsUrl,
    token: config.workerToken,
    fetch: config.fetch,
  });
  const toolKinds = harnessToolNames(loadToolRegistry(join(config.contentRoot, "tools.yaml")));
  const toolKind = (name: string): ToolKind | undefined => toolKinds.get(name)?.kind;

  const stopController = new AbortController();
  let current: RunningTask | undefined;
  let running: Promise<void> | undefined;

  async function execute(claim: Claim, signal: AbortSignal, reporter: TaskReporter) {
    const { task, bot, role } = claim;
    const approvals = new Set<string>();
    const progress = new Progress((event) => reporter.event(event));
    reporter.event({ kind: "status", text: `started on ${workerId}` });
    progress.milestone(FIRST_MILESTONE);

    if (task.bot !== config.bot || bot.name !== config.bot) {
      throw new PmFacingError(
        `This request reached the wrong bot's worker (${config.bot}, not ${task.bot}). A platform owner needs to check the bot runtime.`,
      );
    }
    if (!(role.model in MODEL_ALIASES)) {
      throw new PmFacingError(
        `The ${role.name} role asks for a model role PM OS doesn't have (${role.model}). A platform owner needs to fix the role.`,
      );
    }

    // Slice H4: before anything else, the source every model call of the task names.
    const sourceGuard = new ModelSourceGuard(taskModelSource(claim));
    const stagingRoot = mkdtempSync(join(tmpdir(), `pmos-task-${task.id}-`));
    let toolset: McpToolSet | undefined;
    try {
      try {
        toolset = await connectMcpTools({
          url: config.toolserverMcpUrl,
          sessionToken: claim.session_token,
          onApproval: (id) => approvals.add(id),
          onToolError: (name, message) => {
            if (/denied|not allowed|refused|permission|403/i.test(message)) {
              reporter.trace({ kind: "policy_denial", body: { name, message } });
            }
          },
        });
      } catch (error) {
        throw new PmFacingError(
          "I couldn't reach the PM OS tool server, so I couldn't look anything up. Please try again in a few minutes.",
          { cause: error },
        );
      }

      const headers = gatewayHeaders(claim);
      const model =
        config.model?.(claim, headers) ??
        gatewayModel({
          gatewayUrl: gatewayBaseUrl(config.gatewayUrl),
          role: role.model as ModelRole,
          apiKey: config.gatewayToken,
          headers,
          sourceGuard,
        });
      // A personal bot reads as its owner, so it gets that PM's own master context; a team
      // bot gets the team context only.
      const user =
        bot.scope === "personal" && bot.owner === task.requested_by ? task.requested_by : undefined;
      const systemPrompt = botSystemPrompt(claim, config.now?.() ?? new Date());
      let agent: ReturnType<typeof createPmosAgent>;
      try {
        agent = createPmosAgent({
          contentRoot: config.contentRoot,
          model,
          user,
          tools: toolset.tools,
          skills: role.skills,
          systemPrompt,
          interruptOnWrites: false,
          stagingRoot,
        });
      } catch (error) {
        // A role's skill folder missing from the sandbox: a persona added after the sandbox
        // was created (its uploads are fixed at start). Fail plainly; never run with fewer
        // skills or with another bot's.
        if (error instanceof MissingSkillError) {
          log("warn", "a skill of the role is not in this sandbox", {
            task: task.id,
            role: role.name,
            skill: error.skill,
          });
          throw new PmFacingError(SKILLS_NOT_INSTALLED, { cause: error });
        }
        throw error;
      }
      const tracer = new TaskTracer((entry) => reporter.trace(entry), progress, toolKind);
      let result: Awaited<ReturnType<typeof agent.invoke>>;
      try {
        result = await raceAbort(
          agent.invoke({ messages: taskMessages(claim) }, { signal, callbacks: [tracer], recursionLimit: maxSteps }),
          signal,
        );
      } catch (error) {
        // The gateway's source changed mid-task: the task ends, and is never retried under
        // the new source (the runtime gets a failure, not a lost lease).
        if (sourceGuard.changed) {
          throw new PmFacingError(MODEL_SOURCE_CHANGED, { cause: error });
        }
        throw error;
      }
      const answer = finalAnswer(result.messages);
      if (!answer) {
        throw new PmFacingError(
          "I finished without writing an answer. Please try asking again, a little more specifically.",
        );
      }
      progress.finish();
      return { result: answer, approvals: [...approvals] };
    } finally {
      await toolset?.close().catch(() => {});
      rmSync(stagingRoot, { recursive: true, force: true });
    }
  }

  async function runTask(claim: Claim, controller: AbortController): Promise<void> {
    const { task } = claim;
    const reporter = new TaskReporter(client, task.id, log);
    log("info", "claimed task", { task: task.id, bot: task.bot, requested_by: task.requested_by });

    const heartbeat = setInterval(() => {
      client.heartbeat(task.id).catch((error: unknown) => {
        if (error instanceof RuntimeError && [404, 409, 410].includes(error.status)) {
          // Cancelled, or the lease went to another worker: stop and report nothing.
          log("warn", "task no longer ours; stopping it", { task: task.id, status: error.status });
          controller.abort("lost" satisfies AbortReason);
        } else {
          log("warn", "heartbeat failed", { task: task.id, error: (error as Error).message });
        }
      });
    }, heartbeatMs);
    heartbeat.unref();

    try {
      const outcome = await execute(claim, controller.signal, reporter);
      await reporter.finish("complete", () => client.complete(task.id, outcome));
      log("info", "completed task", { task: task.id, approvals: outcome.approvals.length });
    } catch (error) {
      const aborted = error instanceof TaskAborted ? error.reason : undefined;
      // No longer ours, or we're shutting down: report nothing. On shutdown the lease runs out
      // and the runtime retries the task on another start, as D2 asks.
      if (aborted === "lost" || aborted === "stopped") {
        if (aborted === "stopped") {
          log("info", "left task for the runtime to retry", { task: task.id });
        }
        await reporter.close();
        return;
      }
      const reason = pmReason(error);
      log("warn", "failed task", {
        task: task.id,
        reason,
        error: error instanceof Error ? error.message : String(error),
      });
      await reporter.finish("fail", () => client.fail(task.id, reason));
    } finally {
      clearInterval(heartbeat);
    }
  }

  async function loop(): Promise<void> {
    let backoff = 1_000;
    log("info", "worker started", { worker: workerId, bot: config.bot });
    while (!stopController.signal.aborted) {
      let raw: unknown;
      try {
        raw = await client.claim(
          { worker: workerId, bots: [config.bot], wait_ms: waitMs },
          stopController.signal,
        );
        backoff = 1_000;
      } catch (error) {
        if (stopController.signal.aborted) {
          break;
        }
        log("warn", "claim failed; retrying", { error: (error as Error).message, retry_ms: backoff });
        await sleep(backoff, stopController.signal);
        backoff = Math.min(backoff * 2, 30_000);
        continue;
      }
      if (raw === null) {
        continue;
      }
      const parsed = ClaimSchema.safeParse(raw);
      if (!parsed.success) {
        const id = (raw as { task?: { id?: unknown } })?.task?.id;
        log("error", "unreadable claim", { task: id, error: parsed.error.message });
        if (typeof id === "string") {
          await client
            .fail(
              id,
              "The bot runtime sent a task this worker couldn't read. A platform owner needs to check that the worker and runtime versions match.",
            )
            .catch(() => {});
        }
        continue;
      }
      const controller = new AbortController();
      const done = runTask(parsed.data, controller);
      current = { controller, done };
      await done;
      current = undefined;
    }
    log("info", "worker stopped", { worker: workerId });
  }

  return {
    run() {
      running ??= loop();
      return running;
    },
    async stop() {
      if (!stopController.signal.aborted) {
        stopController.abort();
      }
      const task = current;
      if (task) {
        const grace = setTimeout(() => task.controller.abort("stopped"), stopGraceMs);
        await task.done;
        clearTimeout(grace);
      }
      await running;
    },
  };
}
