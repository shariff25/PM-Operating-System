/**
 * In-process fakes for the worker tests: the bot runtime (`pmos-bots`), the tool server's
 * MCP endpoint (a real MCP SDK server over Streamable HTTP), and an OpenAI-compatible gateway.
 */
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";
import { z } from "zod";

export interface Recorded {
  method: string;
  path: string;
  headers: IncomingMessage["headers"];
  body: any;
}

async function readBody(req: IncomingMessage): Promise<any> {
  const chunks: Buffer[] = [];
  for await (const chunk of req) {
    chunks.push(chunk as Buffer);
  }
  const text = Buffer.concat(chunks).toString("utf8");
  return text ? JSON.parse(text) : undefined;
}

function json(res: ServerResponse, status: number, body?: unknown): void {
  if (body === undefined) {
    res.writeHead(status).end();
    return;
  }
  res.writeHead(status, { "content-type": "application/json" }).end(JSON.stringify(body));
}

async function listen(server: Server): Promise<string> {
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  return `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
}

function closer(server: Server): () => Promise<void> {
  return () =>
    new Promise<void>((resolve) => {
      server.closeAllConnections();
      server.close(() => resolve());
    });
}

export async function waitFor<T>(
  check: () => T | undefined | false,
  timeoutMs = 10_000,
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const value = check();
    if (value) {
      return value;
    }
    if (Date.now() > deadline) {
      throw new Error("timed out waiting for a condition");
    }
    await new Promise((r) => setTimeout(r, 10));
  }
}

export const WORKER_TOKEN = "worker-token-0123456789abcdef0123456789";

export interface FakeRuntime {
  url: string;
  requests: Recorded[];
  /** Requests for one task, by action (`events`, `trace`, `heartbeat`, `complete`, `fail`). */
  forTask(id: string, action?: string): Recorded[];
  close(): Promise<void>;
}

export async function startFakeRuntime(options: {
  claims: unknown[];
  /** Override a response: return `[status, body]`, or nothing for the default. */
  respond?: (req: Recorded) => [number, unknown?] | undefined;
}): Promise<FakeRuntime> {
  const queue = [...options.claims];
  const requests: Recorded[] = [];
  const server = createServer(async (req, res) => {
    const recorded: Recorded = {
      method: req.method ?? "",
      path: req.url ?? "",
      headers: req.headers,
      body: await readBody(req),
    };
    requests.push(recorded);
    if (req.headers.authorization !== `Bearer ${WORKER_TOKEN}`) {
      json(res, 401, { error: "unauthorized" });
      return;
    }
    const override = options.respond?.(recorded);
    if (override) {
      json(res, override[0], override[1]);
      return;
    }
    if (recorded.path === "/v1/worker/claim") {
      const next = queue.shift();
      if (next) {
        json(res, 200, next);
        return;
      }
      // A short long-poll, so the loop doesn't spin.
      const wait = Math.min(Number(recorded.body?.wait_ms ?? 0), 20);
      setTimeout(() => {
        if (!res.writableEnded) {
          json(res, 204);
        }
      }, wait);
      return;
    }
    json(res, 200, {});
  });
  const url = await listen(server);
  return {
    url,
    requests,
    forTask(id, action) {
      const prefix = `/v1/worker/tasks/${id}/`;
      return requests.filter(
        (r) => r.path.startsWith(prefix) && (!action || r.path === `${prefix}${action}`),
      );
    },
    close: closer(server),
  };
}

export interface FakeMcp {
  url: string;
  calls: { name: string; args: unknown; authorization?: string }[];
  close(): Promise<void>;
}

/**
 * A stateless MCP server with one read tool (`docs_search`) and one write tool
 * (`confluence_create_draft`) that, like the tool server, returns `pending_approval`.
 */
export async function startFakeMcp(options: { searchDelayMs?: number } = {}): Promise<FakeMcp> {
  const calls: FakeMcp["calls"] = [];
  let approvals = 0;

  const build = (authorization?: string) => {
    const server = new McpServer({ name: "fake-toolserver", version: "0.0.0" });
    const structured = (value: Record<string, unknown>) => ({
      content: [{ type: "text" as const, text: JSON.stringify(value) }],
      structuredContent: value,
    });
    server.registerTool(
      "docs_search",
      {
        description: "Search the public support docs",
        inputSchema: { query: z.string() },
        annotations: { readOnlyHint: true },
      },
      async ({ query }) => {
        calls.push({ name: "docs_search", args: { query }, authorization });
        if (options.searchDelayMs) {
          await new Promise((r) => setTimeout(r, options.searchDelayMs));
        }
        return structured({
          untrusted: true,
          source: "docs",
          content: [{ url: "https://support.acme.com/docs/ports", title: `Ports (${query})` }],
        });
      },
    );
    server.registerTool(
      "confluence_create_draft",
      {
        description: "Create a draft page. Needs the PM's approval per call.",
        inputSchema: { space: z.string(), title: z.string(), body: z.string() },
      },
      async (args) => {
        calls.push({ name: "confluence_create_draft", args, authorization });
        approvals += 1;
        return structured({
          status: "pending_approval",
          approval_id: `apr_${approvals}`,
          message: "Not done yet: waiting for approval. Do not retry, and do not say it was done.",
        });
      },
    );
    server.registerTool(
      "jira_read",
      { description: "Read an issue", inputSchema: { key: z.string() } },
      async (args) => {
        calls.push({ name: "jira_read", args, authorization });
        return {
          content: [{ type: "text" as const, text: "permission denied: PMOS is outside the bot's read scope" }],
          isError: true,
        };
      },
    );
    return server;
  };

  const server = createServer(async (req, res) => {
    if (req.method !== "POST") {
      json(res, 405, { error: "method not allowed" });
      return;
    }
    const authorization = req.headers.authorization;
    if (!authorization?.startsWith("Bearer sess-")) {
      json(res, 401, { error: "missing or expired PM OS session" });
      return;
    }
    const body = await readBody(req);
    const mcp = build(authorization);
    const transport = new StreamableHTTPServerTransport({ sessionIdGenerator: undefined });
    res.on("close", () => {
      void transport.close();
      void mcp.close();
    });
    await mcp.connect(transport);
    await transport.handleRequest(req, res, body);
  });
  const url = await listen(server);
  return { url: `${url}/mcp`, calls, close: closer(server) };
}

export interface FakeGateway {
  url: string;
  requests: Recorded[];
  close(): Promise<void>;
}

/** An OpenAI-compatible `/v1/chat/completions` that always answers `answer`. */
export async function startFakeGateway(answer: string): Promise<FakeGateway> {
  const requests: Recorded[] = [];
  const server = createServer(async (req, res) => {
    const recorded: Recorded = {
      method: req.method ?? "",
      path: req.url ?? "",
      headers: req.headers,
      body: await readBody(req),
    };
    requests.push(recorded);
    if (recorded.path !== "/v1/chat/completions") {
      json(res, 404, { error: { message: "not found" } });
      return;
    }
    const model = recorded.body?.model;
    const created = Math.floor(Date.now() / 1000);
    if (recorded.body?.stream) {
      res.writeHead(200, { "content-type": "text/event-stream" });
      const chunk = (delta: unknown, finish: string | null) =>
        `data: ${JSON.stringify({
          id: "chatcmpl-fake",
          object: "chat.completion.chunk",
          created,
          model,
          choices: [{ index: 0, delta, finish_reason: finish }],
        })}\n\n`;
      res.write(chunk({ role: "assistant", content: answer }, null));
      res.write(chunk({}, "stop"));
      res.end("data: [DONE]\n\n");
      return;
    }
    json(res, 200, {
      id: "chatcmpl-fake",
      object: "chat.completion",
      created,
      model,
      choices: [
        { index: 0, message: { role: "assistant", content: answer }, finish_reason: "stop" },
      ],
      usage: { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 },
    });
  });
  const url = await listen(server);
  return { url, requests, close: closer(server) };
}
