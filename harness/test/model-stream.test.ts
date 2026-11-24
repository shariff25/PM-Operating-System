/**
 * Streamed model calls (`src/model-stream.ts`, `gatewayModel`): the worker's model asks the
 * gateway for a streamed answer and assembles it, and a stream that stalls, ends early or
 * carries an error event fails the call instead of yielding a short answer. Each test runs the
 * real ChatOpenAI client against a local server that plays one scripted event stream.
 */
import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import type { AddressInfo } from "node:net";
import { HumanMessage, type AIMessage } from "@langchain/core/messages";
import { tool } from "@langchain/core/tools";
import { afterEach, describe, expect, it } from "vitest";
import { z } from "zod";
import { ModelSourceGuard, gatewayModel } from "../src/agent.js";
import { ModelStreamError, checkedStreamFetch } from "../src/model-stream.js";
import { pmReason } from "../src/worker.js";

/** One write of the scripted answer, after `delayMs`. */
interface Write {
  data: string;
  delayMs?: number;
}

interface Played {
  body: any;
  headers: IncomingMessage["headers"];
}

const servers: (() => Promise<void>)[] = [];
afterEach(async () => {
  await Promise.all(servers.splice(0).map((close) => close()));
});

/**
 * A gateway that answers every call with `writes`, then ends the body (or, with `hang`, leaves
 * it open), saying `x-pmos-model-source: local` as the gateway does.
 */
async function startStreamGateway(
  writes: Write[],
  options: { hang?: boolean; status?: number; json?: unknown } = {},
): Promise<{ url: string; played: Played[] }> {
  const played: Played[] = [];
  const server = createServer(async (req: IncomingMessage, res: ServerResponse) => {
    const chunks: Buffer[] = [];
    for await (const c of req) chunks.push(c as Buffer);
    played.push({ body: JSON.parse(Buffer.concat(chunks).toString("utf8")), headers: req.headers });
    if (options.json !== undefined) {
      res
        .writeHead(options.status ?? 200, {
          "content-type": "application/json",
          "x-pmos-model-source": "local",
        })
        .end(JSON.stringify(options.json));
      return;
    }
    res.writeHead(200, { "content-type": "text/event-stream", "x-pmos-model-source": "local" });
    for (const w of writes) {
      if (w.delayMs) await new Promise((r) => setTimeout(r, w.delayMs));
      if (res.destroyed) return;
      res.write(w.data);
    }
    if (!options.hang) res.end();
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  servers.push(
    () =>
      new Promise<void>((resolve) => {
        server.closeAllConnections();
        server.close(() => resolve());
      }),
  );
  return { url: `http://127.0.0.1:${(server.address() as AddressInfo).port}/v1`, played };
}

const ev = (choices: unknown[], extra: Record<string, unknown> = {}): Write => ({
  data: `data: ${JSON.stringify({ id: "c", object: "chat.completion.chunk", created: 1, model: "pmos/default", choices, ...extra })}\n\n`,
});
const text = (content: string, role?: string): Write =>
  ev([{ index: 0, delta: { ...(role ? { role } : {}), content }, finish_reason: null }]);
const finish = (reason = "stop"): Write => ev([{ index: 0, delta: {}, finish_reason: reason }]);
const usage = ev([], { usage: { prompt_tokens: 11, completion_tokens: 4, total_tokens: 15 } });
const DONE: Write = { data: "data: [DONE]\n\n" };

function model(url: string, streamIdleMs?: number) {
  return gatewayModel({
    gatewayUrl: url,
    headers: { "X-Pmos-Model-Source": "local" },
    sourceGuard: new ModelSourceGuard("local"),
    streamIdleMs,
  });
}

async function failure(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
  } catch (error) {
    return error;
  }
  throw new Error("expected the call to fail");
}

describe("streamed model calls", () => {
  it("asks for a stream with usage and assembles the answer, with the source header", async () => {
    const gw = await startStreamGateway([text("Port ", "assistant"), text("4443."), finish(), usage, DONE]);
    const answer = (await model(gw.url).invoke([new HumanMessage("Which port?")])) as AIMessage;
    expect(answer.content).toBe("Port 4443.");
    expect(answer.usage_metadata).toMatchObject({ input_tokens: 11, output_tokens: 4, total_tokens: 15 });
    expect(answer.response_metadata.finish_reason).toBe("stop");
    expect(gw.played).toHaveLength(1);
    expect(gw.played[0]?.body.stream).toBe(true);
    expect(gw.played[0]?.body.stream_options).toEqual({ include_usage: true });
    expect(gw.played[0]?.headers["x-pmos-model-source"]).toBe("local");
  });

  it("assembles a tool call whose arguments are split across events, even mid-line", async () => {
    const args = JSON.stringify({ query: "rotation policy", limit: 3 });
    const call = (fn: Record<string, unknown>, first: boolean) =>
      ev([
        {
          index: 0,
          delta: {
            ...(first ? { role: "assistant", content: "" } : {}),
            tool_calls: [{ index: 0, ...(first ? { id: "call_1", type: "function" } : {}), function: fn }],
          },
          finish_reason: null,
        },
      ]);
    const whole = [
      call({ name: "context_search", arguments: "" }, true),
      call({ arguments: args.slice(0, 9) }, false),
      call({ arguments: args.slice(9) }, false),
      finish("tool_calls"),
      usage,
      DONE,
    ]
      .map((w) => w.data)
      .join("");
    // Bytes cut at arbitrary points, so events and lines are split across reads.
    const pieces: Write[] = [];
    for (let i = 0; i < whole.length; i += 37) pieces.push({ data: whole.slice(i, i + 37) });
    const gw = await startStreamGateway(pieces);
    const search = tool(async () => "", {
      name: "context_search",
      description: "search",
      schema: z.object({ query: z.string(), limit: z.number() }),
    });
    const bound = model(gw.url).bindTools!([search]);
    const answer = (await bound.invoke([new HumanMessage("find it")])) as AIMessage;
    expect(answer.tool_calls).toEqual([
      { id: "call_1", name: "context_search", args: { query: "rotation policy", limit: 3 }, type: "tool_call" },
    ]);
    expect(gw.played[0]?.body.tools?.[0]?.function?.name).toBe("context_search");
  });

  it("fails a stream that goes silent, never returning what came so far", async () => {
    const gw = await startStreamGateway([text("Port ", "assistant")], { hang: true });
    const error = await failure(model(gw.url, 200).invoke([new HumanMessage("Which port?")]));
    expect(error).toBeInstanceOf(ModelStreamError);
    expect((error as ModelStreamError).problem).toBe("stalled");
    expect(pmReason(error)).toBe(
      "The model gateway's answer went silent before it finished. Please try again in a few minutes.",
    );
  });

  it("fails a stream that ends without [DONE], or without a finish reason", async () => {
    for (const writes of [
      [text("Port ", "assistant"), text("44")],
      [text("Port ", "assistant"), finish()],
      [text("Port ", "assistant"), usage, DONE],
    ]) {
      const gw = await startStreamGateway(writes);
      const error = await failure(model(gw.url).invoke([new HumanMessage("Which port?")]));
      expect(error).toBeInstanceOf(ModelStreamError);
      expect((error as ModelStreamError).problem).toBe("incomplete");
      expect(pmReason(error)).toBe(
        "The model gateway's answer was cut off before it finished. Please try again in a few minutes.",
      );
    }
  });

  it("fails a stream whose connection breaks mid-answer", async () => {
    const server = createServer((_req, res) => {
      res.writeHead(200, { "content-type": "text/event-stream", "x-pmos-model-source": "local" });
      res.write(text("Port ", "assistant").data);
      setTimeout(() => res.socket?.destroy(), 20);
    });
    await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
    servers.push(() => new Promise<void>((resolve) => server.close(() => resolve())));
    const url = `http://127.0.0.1:${(server.address() as AddressInfo).port}/v1`;
    const error = await failure(model(url).invoke([new HumanMessage("Which port?")]));
    expect(error).toBeInstanceOf(ModelStreamError);
    expect((error as ModelStreamError).problem).toBe("incomplete");
  });

  it("fails on an error event mid-stream (the gateway's neutral one for a labelled role)", async () => {
    const gw = await startStreamGateway([
      text("Port ", "assistant"),
      {
        data: `data: ${JSON.stringify({ error: { type: "upstream_error", message: "the model server reported an error mid-answer" } })}\n\n`,
      },
      text("4443."),
      finish(),
      DONE,
    ]);
    const error = await failure(model(gw.url).invoke([new HumanMessage("Which port?")]));
    expect(error).toBeInstanceOf(ModelStreamError);
    expect((error as ModelStreamError).problem).toBe("error");
    expect((error as Error).message).toContain("reported an error mid-answer");
    expect(pmReason(error)).toContain("reported an error before it finished");
  });

  it("leaves error answers as they were: a 503 is still 'the model is busy'", async () => {
    const gw = await startStreamGateway([], { status: 503, json: { error: { message: "busy" } } });
    const error = await failure(model(gw.url).invoke([new HumanMessage("Which port?")]));
    expect(error).not.toBeInstanceOf(ModelStreamError);
    expect(pmReason(error)).toBe("The model is busy right now. Please try again in a few minutes.");
    expect(gw.played).toHaveLength(1);
  });

  it("passes answers that aren't event streams through untouched", async () => {
    const body = JSON.stringify({ ok: true });
    const base = (async () =>
      new Response(body, { headers: { "content-type": "application/json" } })) as typeof fetch;
    const res = await checkedStreamFetch(base, 50)("http://x/v1/models");
    expect(await res.text()).toBe(body);
  });
});
