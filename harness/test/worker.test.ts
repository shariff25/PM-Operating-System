import { join } from "node:path";
import { AIMessage } from "@langchain/core/messages";
import { afterEach, describe, expect, it } from "vitest";
import { configFromEnv } from "../src/main.js";
import { pendingApprovals } from "../src/mcp-tools.js";
import { milestoneFor } from "../src/progress.js";
import {
  type Logger,
  type WorkerConfig,
  createWorker,
  gatewayBaseUrl,
  pmReason,
} from "../src/worker.js";
import {
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
  job: "BOT-JOB: answer the product team's questions about Acme products.",
};

function claim(id: string, overrides: { priority?: string; text?: string } = {}) {
  return {
    task: {
      id,
      bot: "docs",
      requested_by: "ana",
      source: "slack",
      priority: overrides.priority ?? "interactive",
      text: overrides.text ?? "What ports does DSM need?",
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
      call("docs_search", { query: "dsm ports" }),
      call("docs_search", { query: "dsm firewall" }),
      call("confluence_create_draft", { space: "PM", title: "DSM ports", body: "4443" }),
      new AIMessage("DSM needs **port 4443**. A Confluence draft is waiting for your approval."),
    ]);
    const { runtime, mcp } = await setup({ claims: [claim("tsk_1")], models: { tsk_1: model } });
    const done = await waitFor(() => finished(runtime, "tsk_1"));

    // Sequence: claim, then events and trace, then complete last.
    expect(done.path).toBe("/v1/worker/tasks/tsk_1/complete");
    expect(done.body).toEqual({
      result: "DSM needs **port 4443**. A Confluence draft is waiting for your approval.",
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
    const beats = runtime.forTask("tsk_1", "heartbeat").length;
    expect(beats).toBeGreaterThanOrEqual(3);
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
    const gateway = await startFakeGateway("Port 4443, see https://support.acme.com/docs/ports");
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
      result: "Port 4443, see https://support.acme.com/docs/ports",
      approvals: [],
    });
    const request = gateway.requests[0];
    expect(request?.path).toBe("/v1/chat/completions");
    expect(request?.headers["x-pmos-user"]).toBe("ana");
    expect(request?.headers["x-pmos-run"]).toBe("tsk_gw");
    expect(request?.headers["x-pmos-priority"]).toBe("scheduled");
    expect(request?.headers.authorization).toBe("Bearer gateway-token-0123456789abcdef0123456789");
    expect(request?.body.model).toBe("pmos/default");
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
    expect(milestoneFor("read_file", undefined)).toBeNull();
    expect(milestoneFor("write_todos", undefined)).toBeNull();
  });

  it("writes failure reasons for a PM", () => {
    expect(pmReason(Object.assign(new Error("busy"), { status: 503 }))).toContain("busy right now");
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
