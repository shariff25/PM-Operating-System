import { join } from "node:path";
import { AIMessage } from "@langchain/core/messages";
import { afterEach, describe, expect, it } from "vitest";
import { configFromEnv } from "../src/main.js";
import { pendingApprovals } from "../src/mcp-tools.js";
import { milestoneFor } from "../src/progress.js";
import { ClaimSchema } from "../src/runtime-client.js";
import {
  type Logger,
  MODEL_SOURCE_CHANGED,
  MODEL_SOURCE_MISSING,
  type WorkerConfig,
  botSystemPrompt,
  createWorker,
  finalAnswer,
  gatewayBaseUrl,
  pmReason,
  taskMessages,
  isAbortRejection,
} from "../src/worker.js";
import {
  FAKE_DOCS_REF,
  type FakeMcp,
  type FakeRuntime,
  WORKER_TOKEN,
  startFakeGateway,
  startFakeMcp,
  startFakeRuntime,
  waitFor,
} from "./fakes.js";
import { ScriptedModel, systemText } from "./scripted-model.js";

const CONTENT = join(import.meta.dirname, "..", "..", "content");

const DOCS_ROLE = {
  name: "docs-helper",
  description: "Answers questions from the public support docs.",
  skills: ["docs"],
  data_class: "public",
  model: "default",
  actions: {},
  instructions: "ROLE-INSTRUCTIONS: search the docs first and cite every page.",
};
const DOCS_BOT = {
  name: "docs",
  role: "docs-helper",
  scope: "team",
  owner: "ana",
  description: "The team's Docs bot.",
  reads: {},
  actions: {},
  job: "BOT-JOB: answer the product team's questions about the company's products.",
};

function claim(id: string, overrides: { priority?: string; text?: string } = {}) {
  return {
    task: {
      id,
      bot: "docs",
      requested_by: "ana",
      source: "slack",
      priority: overrides.priority ?? "interactive",
      text: overrides.text ?? "What ports does Acme Vault need?",
      status: "working",
      checklist: [],
      approvals: [],
      attempts: 1,
    },
    bot: DOCS_BOT,
    role: DOCS_ROLE,
    tools: ["docs.search", "docs.read"],
    session_token: `sess-${id}`,
    lease_until: new Date(Date.now() + 120_000).toISOString(),
    gateway: { model: "pmos/default", headers: { "X-Pmos-Model-Source": "local" } },
  };
}

const call = (name: string, args: Record<string, unknown>) =>
  new AIMessage({
    content: "",
    tool_calls: [{ id: crypto.randomUUID(), name, args }],
  });

class FailingModel extends ScriptedModel {
  constructor() {
    super([]);
  }
  override async _generate(): Promise<never> {
    throw new Error("upstream exploded\n    at secret/internal/stack.ts:1");
  }
}

const silent: Logger = () => {};

let cleanups: (() => Promise<void>)[] = [];
afterEach(async () => {
  for (const cleanup of cleanups.reverse()) {
    await cleanup();
  }
  cleanups = [];
});

async function setup(options: {
  claims: unknown[];
  models?: Record<string, ScriptedModel>;
  searchDelayMs?: number;
  config?: Partial<WorkerConfig>;
  respond?: Parameters<typeof startFakeRuntime>[0]["respond"];
}) {
  const runtime = await startFakeRuntime({ claims: options.claims, respond: options.respond });
  const mcp = await startFakeMcp({ searchDelayMs: options.searchDelayMs });
  const models = options.models;
  const worker = createWorker({
    botsUrl: runtime.url,
    workerToken: WORKER_TOKEN,
    bot: "docs",
    gatewayUrl: "http://127.0.0.1:9/v1",
    toolserverMcpUrl: mcp.url,
    contentRoot: CONTENT,
    workerId: "test-worker",
    waitMs: 50,
    logger: silent,
    model: models
      ? (c) => {
          const model = models[c.task.id];
          if (!model) throw new Error(`no model scripted for ${c.task.id}`);
          return model;
        }
      : undefined,
    ...options.config,
  });
  const running = worker.run();
  cleanups.push(async () => {
    await worker.stop();
    await running;
    await mcp.close();
    await runtime.close();
  });
  return { runtime, mcp, worker, running };
}

const finished = (runtime: FakeRuntime, id: string) =>
  runtime.forTask(id, "complete")[0] ?? runtime.forTask(id, "fail")[0];

const events = (runtime: FakeRuntime, id: string) =>
  runtime.forTask(id, "events").map((r) => r.body);

const milestones = (runtime: FakeRuntime, id: string) =>
  events(runtime, id)
    .filter((e) => e.kind === "milestone")
    .map((e) => e.text);

const traceEntries = (runtime: FakeRuntime, id: string) =>
  runtime.forTask(id, "trace").flatMap((r) => r.body.entries as { kind: string; body: any }[]);

describe("harness worker", () => {
  it("claims, runs, reports progress and trace, then completes with the approvals", async () => {
    const model = new ScriptedModel([
      call("docs_search", { query: "vault ports" }),
      call("docs_search", { query: "vault firewall" }),
      call("confluence_create_draft", { space: "PM", title: "Acme Vault ports", body: "4443" }),
      new AIMessage("Acme Vault needs **port 4443**. A Confluence draft is waiting for your approval."),
    ]);
    const { runtime, mcp } = await setup({ claims: [claim("tsk_1")], models: { tsk_1: model } });
    const done = await waitFor(() => finished(runtime, "tsk_1"));

    // Sequence: claim, then events and trace, then complete last.
    expect(done.path).toBe("/v1/worker/tasks/tsk_1/complete");
    expect(done.body).toEqual({
      result: "Acme Vault needs **port 4443**. A Confluence draft is waiting for your approval.",
      approvals: ["apr_1"],
    });
    const order = runtime.requests.map((r) => r.path);
    const firstClaim = order.indexOf("/v1/worker/claim");
    const firstEvent = order.indexOf("/v1/worker/tasks/tsk_1/events");
    const firstTrace = order.indexOf("/v1/worker/tasks/tsk_1/trace");
    const complete = order.indexOf("/v1/worker/tasks/tsk_1/complete");
    expect(firstClaim).toBe(0);
    expect(runtime.requests[0]?.body).toEqual({ worker: "test-worker", bots: ["docs"], wait_ms: 50 });
    expect(firstClaim).toBeLessThan(firstEvent);
    expect(firstEvent).toBeLessThan(firstTrace);
    const lastTaskReport = Math.max(
      ...order.map((p, i) => (/tasks\/tsk_1\/(events|trace)$/.test(p) ? i : -1)),
    );
    expect(lastTaskReport).toBeLessThan(complete);

    // Milestones: "Working on it" first, then one per family, each once.
    expect(milestones(runtime, "tsk_1")).toEqual([
      "Working on it",
      "Searching the support docs",
      "Drafting in Confluence",
    ]);
    const checklists = events(runtime, "tsk_1").filter((e) => e.kind === "checklist");
    expect(checklists.at(-1)?.checklist).toEqual([
      { item: "Working on it", done: true },
      { item: "Searching the support docs", done: true },
      { item: "Drafting in Confluence", done: true },
    ]);

    // Trace: the prompt, every model turn, every tool call and result.
    const trace = traceEntries(runtime, "tsk_1");
    expect(trace[0]?.kind).toBe("prompt");
    expect(trace.filter((e) => e.kind === "model_reply")).toHaveLength(4);
    expect(trace.filter((e) => e.kind === "tool_call").map((e) => e.body.name)).toEqual([
      "docs_search",
      "docs_search",
      "confluence_create_draft",
    ]);
    const results = trace.filter((e) => e.kind === "tool_result");
    expect(results).toHaveLength(3);
    expect(JSON.stringify(results[2]?.body.content)).toContain("pending_approval");

    // MCP: called with the task's session token, writes reached the tool server.
    expect(mcp.calls.map((c) => c.name)).toEqual([
      "docs_search",
      "docs_search",
      "confluence_create_draft",
    ]);
    expect(new Set(mcp.calls.map((c) => c.authorization))).toEqual(new Set(["Bearer sess-tsk_1"]));

    // System context: the role's instructions, the bot's job, and only the role's skills.
    const system = systemText(model.prompts[0] ?? []);
    expect(system).toContain("ROLE-INSTRUCTIONS");
    expect(system).toContain("BOT-JOB");
    expect(system).toContain("Backs the /docs Slack command"); // the docs skill is listed
    expect(system).not.toContain("prd-draft");
    expect(system).not.toContain("meeting-notes");
  });

  it("shows the model each read's ref and passes source markers to the tool server as written", async () => {
    const body = `Acme Vault needs port 4443 [[${FAKE_DOCS_REF}]].`;
    const model = new ScriptedModel([
      call("docs_search", { query: "vault ports" }),
      call("confluence_create_draft", { space: "PM", title: "Ports", body }),
      new AIMessage("A Confluence draft is waiting for your approval."),
    ]);
    const { runtime, mcp } = await setup({ claims: [claim("tsk_ref")], models: { tsk_ref: model } });
    await waitFor(() => finished(runtime, "tsk_ref"));

    // The read's result, as the model saw it on its next turn, carries the ref.
    const seen = (model.prompts[1] ?? []).filter((m) => m.getType() === "tool");
    expect(seen).toHaveLength(1);
    expect(JSON.parse(String(seen[0]?.content)).ref).toBe(FAKE_DOCS_REF);
    // The harness changes nothing in a write: the tool server keeps the marker for the PM's
    // review and removes it when it writes.
    const write = mcp.calls.find((c) => c.name === "confluence_create_draft");
    expect(write?.args).toEqual({ space: "PM", title: "Ports", body });
  });

  it("tells a team bot today's date from the worker's clock, and no level", async () => {
    const model = new ScriptedModel([new AIMessage("ok")]);
    const { runtime } = await setup({
      claims: [{ ...claim("tsk_date"), level: "principal" }],
      models: { tsk_date: model },
      config: { now: () => new Date("2026-11-15T23:59:00Z") },
    });
    await waitFor(() => runtime.forTask("tsk_date", "complete")[0]);
    const system = systemText(model.prompts[0] ?? []);
    const first = system.indexOf("You are docs, a PM OS bot, working on a request from ana.");
    const date = system.indexOf("Today is 2026-11-15.");
    expect(first).toBeGreaterThanOrEqual(0);
    expect(date).toBeGreaterThan(first);
    expect(system.indexOf("ROLE-INSTRUCTIONS")).toBeGreaterThan(date);
    expect(system).not.toContain("Your PM works at");
  });

  it("fails a task with a reason a PM can read and keeps claiming", async () => {
    const { runtime } = await setup({
      claims: [claim("tsk_bad"), claim("tsk_good")],
      models: { tsk_bad: new FailingModel(), tsk_good: new ScriptedModel([new AIMessage("ok")]) },
    });
    const failed = await waitFor(() => runtime.forTask("tsk_bad", "fail")[0]);
    expect(failed.body.reason).toMatch(/^Something went wrong while I was working on this/);
    expect(failed.body.reason).not.toContain("\n");
    expect(runtime.forTask("tsk_bad", "complete")).toHaveLength(0);

    const completed = await waitFor(() => runtime.forTask("tsk_good", "complete")[0]);
    expect(completed.body).toEqual({ result: "ok", approvals: [] });
  });

  it("fails with a plain reason when the tool server can't be reached", async () => {
    const { runtime } = await setup({
      claims: [claim("tsk_1")],
      models: { tsk_1: new ScriptedModel([new AIMessage("ok")]) },
      config: { toolserverMcpUrl: "http://127.0.0.1:9/mcp" },
    });
    const failed = await waitFor(() => runtime.forTask("tsk_1", "fail")[0]);
    expect(failed.body.reason).toContain("couldn't reach the PM OS tool server");
  });

  it("traces a tool server refusal as a policy denial", async () => {
    const { runtime } = await setup({
      claims: [claim("tsk_1")],
      models: {
        tsk_1: new ScriptedModel([call("jira_read", { key: "PMOS-1" }), new AIMessage("no access")]),
      },
    });
    await waitFor(() => finished(runtime, "tsk_1"));
    const denials = traceEntries(runtime, "tsk_1").filter((e) => e.kind === "policy_denial");
    expect(denials).toEqual([
      {
        kind: "policy_denial",
        body: { name: "jira_read", message: "permission denied: PMOS is outside the bot's read scope" },
      },
    ]);
    expect(milestones(runtime, "tsk_1")).toEqual(["Working on it", "Reading Jira"]);
  });

  it("heartbeats while a task runs, and stops when it ends", async () => {
    const { runtime } = await setup({
      claims: [claim("tsk_1")],
      models: {
        tsk_1: new ScriptedModel([call("docs_search", { query: "slow" }), new AIMessage("done")]),
      },
      searchDelayMs: 300,
      config: { heartbeatMs: 40 },
    });
    await waitFor(() => finished(runtime, "tsk_1"));
    expect(runtime.forTask("tsk_1", "heartbeat").length).toBeGreaterThanOrEqual(3);
    // The runtime sees the task finish before the worker's own call returns and clears its
    // timer, so one beat can still be on its way. Let that settle, then nothing more may come.
    await new Promise((r) => setTimeout(r, 120));
    const beats = runtime.forTask("tsk_1", "heartbeat").length;
    await new Promise((r) => setTimeout(r, 150));
    expect(runtime.forTask("tsk_1", "heartbeat").length).toBe(beats);
  });

  it("drops a task without reporting when the runtime says it is no longer ours", async () => {
    const { runtime } = await setup({
      claims: [claim("tsk_1")],
      models: {
        tsk_1: new ScriptedModel([call("docs_search", { query: "slow" }), new AIMessage("done")]),
      },
      searchDelayMs: 5_000,
      config: { heartbeatMs: 30 },
      respond: (r) => (r.path.endsWith("/heartbeat") ? [409, { error: "cancelled" }] : undefined),
    });
    await waitFor(() => runtime.forTask("tsk_1", "heartbeat")[0]);
    // The loop goes back to claiming without completing or failing the task.
    await waitFor(() => runtime.requests.filter((r) => r.path === "/v1/worker/claim").length >= 3);
    expect(finished(runtime, "tsk_1")).toBeUndefined();
  });

  it("stop() leaves an unfinished task for the runtime to retry, then run() returns", async () => {
    const { runtime, mcp, worker, running } = await setup({
      claims: [claim("tsk_1")],
      models: {
        tsk_1: new ScriptedModel([call("docs_search", { query: "slow" }), new AIMessage("done")]),
      },
      searchDelayMs: 10_000,
      config: { stopGraceMs: 0 },
    });
    await waitFor(() => (mcp as FakeMcp).calls.length > 0);
    const started = Date.now();
    await worker.stop();
    await running;
    expect(Date.now() - started).toBeLessThan(3_000);

    // Neither failed nor completed: the lease runs out and the runtime retries it (D2).
    expect(finished(runtime, "tsk_1")).toBeUndefined();
    // Nothing more is claimed or reported once stopped, even if the abandoned run moves on.
    const count = runtime.requests.length;
    await new Promise((r) => setTimeout(r, 150));
    expect(runtime.requests.length).toBe(count);
  });

  it("stop() lets a task that finishes within the grace period complete", async () => {
    const { runtime, mcp, worker, running } = await setup({
      claims: [claim("tsk_1")],
      models: {
        tsk_1: new ScriptedModel([call("docs_search", { query: "quick" }), new AIMessage("done")]),
      },
      searchDelayMs: 200,
      config: { stopGraceMs: 5_000 },
    });
    await waitFor(() => mcp.calls.length > 0);
    await worker.stop();
    await running;
    expect(runtime.forTask("tsk_1", "complete")[0]?.body.result).toBe("done");
    expect(runtime.forTask("tsk_1", "fail")).toHaveLength(0);
  });

  it("calls the gateway with the task's user, run and priority headers", async () => {
    const gateway = await startFakeGateway("Port 4443, see https://docs.example.com/docs/ports");
    cleanups.push(gateway.close);
    const { runtime } = await setup({
      claims: [claim("tsk_gw", { priority: "scheduled" })],
      config: {
        // No `/v1`: the worker adds it.
        gatewayUrl: gateway.url,
        gatewayToken: "gateway-token-0123456789abcdef0123456789",
      },
    });
    const done = await waitFor(() => finished(runtime, "tsk_gw"));
    expect(done.body).toEqual({
      result: "Port 4443, see https://docs.example.com/docs/ports",
      approvals: [],
    });
    const request = gateway.requests[0];
    expect(request?.path).toBe("/v1/chat/completions");
    expect(request?.headers["x-pmos-user"]).toBe("ana");
    expect(request?.headers["x-pmos-run"]).toBe("tsk_gw");
    expect(request?.headers["x-pmos-priority"]).toBe("scheduled");
    // X4 (slice H4): the source the task was claimed under, on every call.
    expect(request?.headers["x-pmos-model-source"]).toBe("local");
    expect(request?.headers.authorization).toBe("Bearer gateway-token-0123456789abcdef0123456789");
    expect(request?.body.model).toBe("pmos/default");
  });

  it("ends a task when the model source changes between two answers, and sends nothing more", async () => {
    const search = { toolCall: { name: "docs_search", args: { query: "ports" } } };
    const gateway = await startFakeGateway("unused", undefined, [
      search,
      { ...search, source: "hosted-test" },
      { source: "hosted-test" },
    ]);
    cleanups.push(gateway.close);
    const { runtime } = await setup({
      claims: [claim("tsk_switch")],
      config: { gatewayUrl: gateway.url },
    });
    const failed = await waitFor(() => finished(runtime, "tsk_switch"));
    expect(failed.path).toContain("/fail");
    expect(failed.body.reason).toBe(MODEL_SOURCE_CHANGED);
    // The answer that said another source ended it: no call carried its tool result on.
    expect(gateway.requests).toHaveLength(2);
    for (const r of gateway.requests) {
      expect(r.headers["x-pmos-model-source"]).toBe("local");
    }
  });

  it("ends a task when the gateway refuses its source (409), never retrying it", async () => {
    const gateway = await startFakeGateway("unused", undefined, [
      { status: 409, source: "hosted-test" },
    ]);
    cleanups.push(gateway.close);
    const { runtime } = await setup({
      claims: [claim("tsk_409")],
      config: { gatewayUrl: gateway.url },
    });
    const failed = await waitFor(() => finished(runtime, "tsk_409"));
    expect(failed.body.reason).toBe(MODEL_SOURCE_CHANGED);
    expect(gateway.requests).toHaveLength(1);
  });

  it("calls no model for a claim that doesn't say its model source", async () => {
    const gateway = await startFakeGateway("unused");
    cleanups.push(gateway.close);
    const bare = { ...claim("tsk_nosrc"), gateway: { model: "pmos/default", headers: {} } };
    const { runtime, mcp } = await setup({
      claims: [bare],
      config: { gatewayUrl: gateway.url },
    });
    const failed = await waitFor(() => finished(runtime, "tsk_nosrc"));
    expect(failed.body.reason).toBe(MODEL_SOURCE_MISSING);
    expect(gateway.requests).toHaveLength(0);
    expect(mcp.calls).toHaveLength(0);
  });

  it("makes one gateway call per model turn when the gateway keeps answering busy", async () => {
    // The gateway owns retries (docs/gateway.md). The worker's client must not add its own,
    // or each model turn would be tried again on top of the gateway's own tries.
    const gateway = await startFakeGateway("unused", { status: 503, upstreamAttempts: 3 });
    cleanups.push(gateway.close);
    const { runtime } = await setup({
      claims: [claim("tsk_busy")],
      config: { gatewayUrl: gateway.url },
    });
    const failed = await waitFor(() => finished(runtime, "tsk_busy"));
    expect(failed.body.reason).toBe(
      "The model is still busy after several tries. Please try again in a few minutes.",
    );
    expect(gateway.requests).toHaveLength(1);
  });

  it("calls the reasoning model for a role pinned to it", async () => {
    const gateway = await startFakeGateway("A critique.");
    cleanups.push(gateway.close);
    const reasoning = claim("tsk_reason");
    const { runtime } = await setup({
      claims: [{ ...reasoning, role: { ...DOCS_ROLE, model: "reasoning" } }],
      config: { gatewayUrl: gateway.url },
    });
    const done = await waitFor(() => finished(runtime, "tsk_reason"));
    expect(done.body.result).toBe("A critique.");
    expect(gateway.requests[0]?.body.model).toBe("pmos/reasoning");
  });

  it("fails a task whose role names a model role PM OS doesn't have", async () => {
    const bad = claim("tsk_bad_model");
    const { runtime } = await setup({
      claims: [{ ...bad, role: { ...DOCS_ROLE, model: "huge" } }],
      models: { tsk_bad_model: new ScriptedModel([new AIMessage("should not run")]) },
    });
    const failed = await waitFor(() => runtime.forTask("tsk_bad_model", "fail")[0]);
    expect(failed.body.reason).toContain("model role PM OS doesn't have");
  });

  it("fails a task that runs past its step limit", async () => {
    const { runtime } = await setup({
      claims: [claim("tsk_loop")],
      // The script's last message repeats: a model that never stops searching.
      models: { tsk_loop: new ScriptedModel([call("docs_search", { query: "again" })]) },
      config: { maxSteps: 12 },
    });
    const failed = await waitFor(() => runtime.forTask("tsk_loop", "fail")[0]);
    expect(failed.body.reason).toContain("too many steps");
  });

  it("fails a claim for another bot instead of running it", async () => {
    const other = claim("tsk_x");
    other.task.bot = "pm-bea";
    const { runtime } = await setup({
      claims: [other],
      models: { tsk_x: new ScriptedModel([new AIMessage("should not run")]) },
    });
    const failed = await waitFor(() => runtime.forTask("tsk_x", "fail")[0]);
    expect(failed.body.reason).toContain("wrong bot's worker");
  });
});

describe("the bot's system prompt", () => {
  const NOW = new Date("2026-03-01T00:30:00Z");
  const personal = (level?: string) =>
    ClaimSchema.parse({
      ...claim("tsk_p"),
      bot: { ...DOCS_BOT, name: "pm-ana", scope: "personal" },
      ...(level ? { level } : {}),
    });
  const levelLine = (title: string) =>
    `Your PM works at the ${title} level. Read /content/playbooks/levels.md and work at that level's scope, horizon and audience, unless the request says otherwise.`;

  it.each([
    ["senior", "Senior"],
    ["staff", "Staff"],
    ["principal", "Principal"],
  ])("tells a personal bot its PM works at the %s level", (level, title) => {
    const lines = botSystemPrompt(personal(level), NOW).split("\n\n");
    expect(lines.slice(0, 4)).toEqual([
      "You are pm-ana, a PM OS bot, working on a request from ana.",
      "Today is 2026-03-01.",
      levelLine(title),
      "ROLE-INSTRUCTIONS: search the docs first and cite every page.",
    ]);
  });

  it("gives a team bot the date and no level line", () => {
    const parsed = ClaimSchema.parse({ ...claim("tsk_t"), level: "staff" });
    const text = botSystemPrompt(parsed, NOW);
    expect(text.split("\n\n").slice(0, 3)).toEqual([
      "You are docs, a PM OS bot, working on a request from ana.",
      "Today is 2026-03-01.",
      "ROLE-INSTRUCTIONS: search the docs first and cite every page.",
    ]);
    expect(text).not.toContain("levels.md");
  });

  it("reads a claim with no level (an older runtime) as senior", () => {
    const parsed = personal();
    expect(parsed.level).toBe("senior");
    expect(botSystemPrompt(parsed, NOW)).toContain(levelLine("Senior"));
    expect(() => ClaimSchema.parse({ ...claim("tsk_x"), level: "vp" })).toThrow();
  });

  it("uses the UTC date", () => {
    const late = new Date("2026-12-31T23:59:59-05:00");
    expect(botSystemPrompt(personal("staff"), late)).toContain("Today is 2027-01-01.");
  });
});

describe("a conversation's earlier turns", () => {
  const SHORT_REPLY = "A short reply is an answer to your last question.";
  const followUp = (history: unknown[]) =>
    ClaimSchema.parse({ ...claim("tsk_f", { text: "customer" }), history });
  const turns = [
    {
      text: "Draft a PRD for SSO",
      result: "Who is the user of this feature?",
      status: "done",
      at: "2026-11-15T10:00:00Z",
    },
    {
      text: "generate the content",
      error: "The model is busy right now.",
      status: "failed",
      at: "2026-11-15T10:05:00Z",
    },
    { text: "never mind", status: "cancelled", at: "2026-11-15T10:06:00Z" },
  ];

  it("puts earlier turns first as PM and bot messages, oldest first, then the new request", () => {
    const messages = taskMessages(followUp(turns));
    expect(messages.map((m) => [m.getType(), m.content])).toEqual([
      ["human", "Draft a PRD for SSO"],
      ["ai", "Who is the user of this feature?"],
      ["human", "generate the content"],
      ["ai", "(This request failed: The model is busy right now.)"],
      ["human", "never mind"],
      ["ai", "(This request was cancelled before I answered.)"],
      ["human", "customer"],
    ]);
  });

  it("says in the system prompt that a short reply answers the last question", () => {
    const text = botSystemPrompt(followUp(turns));
    expect(text).toContain(SHORT_REPLY);
    expect(text.indexOf("## This conversation")).toBeGreaterThan(text.indexOf("## Writes need approval"));
  });

  it("leaves a first message as it was: the request alone, and no conversation section", () => {
    const first = ClaimSchema.parse(claim("tsk_first"));
    expect(first.history).toEqual([]);
    const messages = taskMessages(first);
    expect(messages).toHaveLength(1);
    expect(messages[0]?.getType()).toBe("human");
    expect(messages[0]?.content).toBe("What ports does Acme Vault need?");
    expect(botSystemPrompt(first)).not.toContain("This conversation");
  });

  it("reads the history's level from the claim, and tells the model when it is restricted", () => {
    const RESTRICTED = "Earlier answers in this conversation drew on restricted sources.";
    const restricted = ClaimSchema.parse({
      ...claim("tsk_r", { text: "post that" }),
      history: turns,
      history_sensitivity: "restricted",
    });
    expect(restricted.history_sensitivity).toBe("restricted");
    expect(botSystemPrompt(restricted)).toContain(RESTRICTED);
    const internal = ClaimSchema.parse({
      ...claim("tsk_i"),
      history: turns,
      history_sensitivity: "internal",
    });
    expect(botSystemPrompt(internal)).not.toContain(RESTRICTED);
    // An older runtime sends none; a value the harness doesn't know reads as restricted.
    expect(followUp(turns).history_sensitivity).toBeUndefined();
    const odd = ClaimSchema.parse({ ...claim("tsk_o"), history: turns, history_sensitivity: "secret" });
    expect(odd.history_sensitivity).toBe("restricted");
  });

  it("never lowers or reports the history's level: the tool server applies it", async () => {
    const model = new ScriptedModel([new AIMessage("Drafted.")]);
    const { runtime } = await setup({
      claims: [
        {
          ...claim("tsk_lvl", { text: "post that" }),
          history: turns.slice(0, 1),
          history_sensitivity: "restricted",
        },
      ],
      models: { tsk_lvl: model },
    });
    const done = await waitFor(() => runtime.forTask("tsk_lvl", "complete")[0]);
    expect(JSON.stringify(done.body)).not.toContain("sensitivity");
    expect(systemText(model.prompts[0] ?? [])).toContain("restricted sources");
  });

  it("never takes a replayed reply as the final answer", () => {
    const messages = taskMessages(followUp(turns));
    expect(finalAnswer(messages)).toBe("");
    expect(finalAnswer([...messages, new AIMessage("Here is the PRD.")])).toBe("Here is the PRD.");
  });

  it("sends the turns to the model ahead of the new request", async () => {
    const model = new ScriptedModel([new AIMessage("Here is the PRD for customers.")]);
    const { runtime } = await setup({
      claims: [{ ...claim("tsk_conv", { text: "customer" }), history: turns.slice(0, 1) }],
      models: { tsk_conv: model },
    });
    const done = await waitFor(() => runtime.forTask("tsk_conv", "complete")[0]);
    expect(done.body.result).toBe("Here is the PRD for customers.");
    const prompt = model.prompts[0] ?? [];
    expect(prompt.map((m) => m.getType())).toEqual(["system", "human", "ai", "human"]);
    expect(prompt.slice(1).map((m) => m.content)).toEqual([
      "Draft a PRD for SSO",
      "Who is the user of this feature?",
      "customer",
    ]);
    expect(systemText(prompt)).toContain(SHORT_REPLY);
  });
});

describe("worker helpers", () => {
  it("reads approval ids from structured content or JSON text", () => {
    const value = { status: "pending_approval", approval_id: "apr_9" };
    expect(pendingApprovals({ content: [], structuredContent: value })).toEqual(["apr_9"]);
    expect(
      pendingApprovals({ content: [{ type: "text", text: JSON.stringify(value) }] }),
    ).toEqual(["apr_9"]);
    expect(pendingApprovals({ content: [{ type: "text", text: "not json" }] })).toEqual([]);
    expect(
      pendingApprovals({ content: [], structuredContent: { status: "approved", url: "x" } }),
    ).toEqual([]);
  });

  it("maps tool families to milestones", () => {
    expect(milestoneFor("docs_search", "read")).toBe("Searching the support docs");
    expect(milestoneFor("jira_search", "read")).toBe("Reading Jira");
    expect(milestoneFor("confluence_read", "read")).toBe("Reading Confluence");
    expect(milestoneFor("confluence_create_draft", "write")).toBe("Drafting in Confluence");
    expect(milestoneFor("slack_read_thread", "read")).toBe("Reading Slack");
    expect(milestoneFor("context_search", "read")).toBe("Checking team context");
    expect(milestoneFor("stats_sample_size", "read")).toBe("Running the numbers");
    expect(milestoneFor("stats_cashflow", "read")).toBe("Running the numbers");
    expect(milestoneFor("jira_rollup", "read")).toBe("Reading Jira");
    expect(milestoneFor("bets_list", "read")).toBe("Checking your bets register");
    expect(milestoneFor("bets_read", "read")).toBe("Checking your bets register");
    expect(milestoneFor("bets_record", "write")).toBe("Drafting a bets register update");
    expect(milestoneFor("read_file", undefined)).toBeNull();
    expect(milestoneFor("write_todos", undefined)).toBeNull();
  });

  it("writes failure reasons for a PM", () => {
    expect(pmReason(Object.assign(new Error("busy"), { status: 503 }))).toContain("busy right now");
    expect(
      pmReason(
        Object.assign(new Error("busy"), {
          status: 429,
          headers: new Headers({ "x-pmos-upstream-attempts": "3" }),
        }),
      ),
    ).toBe("The model is still busy after several tries. Please try again in a few minutes.");
    expect(
      pmReason(
        Object.assign(new Error("busy"), {
          status: 503,
          headers: new Headers({ "x-pmos-upstream-attempts": "1" }),
        }),
      ),
    ).toBe("The model is busy right now. Please try again in a few minutes.");
    expect(pmReason(Object.assign(new Error("x"), { name: "GraphRecursionError" }))).toContain(
      "too many steps",
    );
    expect(pmReason(new Error("x".repeat(500))).length).toBeLessThan(400);
  });

  it("adds /v1 to the gateway URL only when missing", () => {
    expect(gatewayBaseUrl("http://gw:8740")).toBe("http://gw:8740/v1");
    expect(gatewayBaseUrl("http://gw:8740/v1/")).toBe("http://gw:8740/v1");
  });

  it("reads its configuration from the environment", () => {
    expect(() => configFromEnv({})).toThrow(/PMOS_BOTS_URL.*PMOS_TOOLSERVER_MCP_URL/);
    const config = configFromEnv({
      PMOS_BOTS_URL: "http://127.0.0.1:8750",
      PMOS_WORKER_TOKEN: "t",
      PMOS_WORKER_BOT: "docs",
      PMOS_GATEWAY_URL: "http://127.0.0.1:8740/v1",
      PMOS_TOOLSERVER_MCP_URL: "http://127.0.0.1:8710/mcp",
      PMOS_WORKER_WAIT_MS: "1000",
    });
    expect(config.bot).toBe("docs");
    expect(config.waitMs).toBe(1000);
    expect(config.contentRoot).toBe(CONTENT);
  });
});

describe("isAbortRejection", () => {
  it("matches only the rejection an aborted model call leaves behind", () => {
    expect(isAbortRejection(new Error("lost"))).toBe(true);
    expect(isAbortRejection(new Error("stopped"))).toBe(true);
    expect(isAbortRejection(new Error("lost connection to the gateway"))).toBe(false);
    expect(isAbortRejection(new Error("Aborted"))).toBe(false);
    expect(isAbortRejection("lost")).toBe(false);
    expect(isAbortRejection(undefined)).toBe(false);
  });
});
