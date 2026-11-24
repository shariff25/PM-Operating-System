/**
 * Checks every streamed answer from the model gateway as it arrives (docs/gateway.md, "How long
 * a model server may take"; harness/README.md, "Streamed model calls").
 *
 * The worker asks the gateway for a streamed answer (`stream: true`), so bytes flow while the
 * model writes and no hop between the gateway and the model server sees a silent connection
 * for minutes (eval run E3 lost 45 attempts to a 502 at about 300 s on non-streamed calls).
 * The OpenAI client the worker uses ends a stream quietly when its connection closes early,
 * so on its own a cut-off answer would look whole. This fetch wrapper therefore reads the
 * server-sent events as they pass and turns each of these into a `ModelStreamError`, which the
 * task handles as a failed model call:
 *
 * - **stalled:** no byte for `idleMs` (default 180 s, longer than the gateway's own 120 s
 *   silence limit, so the gateway's clearer `502` normally arrives first);
 * - **incomplete:** the body ended before `data: [DONE]`, or with no `finish_reason` on any
 *   choice;
 * - **error:** an event carrying `error` (the gateway's "the model server reported an error
 *   mid-answer", or an upstream's own).
 *
 * Only successful `text/event-stream` answers are wrapped; every other answer (an error status,
 * a whole JSON answer) passes as it came.
 */

/** Longest silence inside a streamed answer before it counts as stalled. */
export const STREAM_IDLE_MS = 180_000;

export type ModelStreamProblem = "stalled" | "incomplete" | "error";

/** A streamed answer that stalled, ended early or carried an error event. */
export class ModelStreamError extends Error {
  constructor(
    readonly problem: ModelStreamProblem,
    detail: string,
  ) {
    super(`model stream ${problem}: ${detail}`);
    this.name = "ModelStreamError";
  }
}

/** What one streamed answer has shown so far. */
class StreamWatch {
  private pending = "";
  private readonly decoder = new TextDecoder();
  done = false;
  finished = false;

  /** Reads `bytes`; returns the error an event carried, if one did. */
  feed(bytes: Uint8Array): ModelStreamError | undefined {
    this.pending += this.decoder.decode(bytes, { stream: true });
    let end = this.pending.indexOf("\n");
    while (end >= 0) {
      const line = this.pending.slice(0, end).replace(/\r$/, "");
      this.pending = this.pending.slice(end + 1);
      const problem = this.line(line);
      if (problem) {
        return problem;
      }
      end = this.pending.indexOf("\n");
    }
    return undefined;
  }

  /** The last line, if the body ended without a newline after it. */
  close(): ModelStreamError | undefined {
    this.pending += this.decoder.decode();
    const rest = this.pending;
    this.pending = "";
    return rest ? this.line(rest.replace(/\r$/, "")) : undefined;
  }

  private line(line: string): ModelStreamError | undefined {
    if (!line.startsWith("data:") || this.done) {
      return undefined;
    }
    const data = line.slice("data:".length).trim();
    if (data === "[DONE]") {
      this.done = true;
      // Checked here, not only at the end of the body: the OpenAI client stops reading at
      // `[DONE]`, so the end of the body may never be read.
      return this.finished
        ? undefined
        : new ModelStreamError("incomplete", "the answer ended without a finish reason");
    }
    let event: unknown;
    try {
      event = JSON.parse(data);
    } catch {
      // Left to the OpenAI client, which refuses malformed event JSON itself.
      return undefined;
    }
    if (typeof event !== "object" || event === null) {
      return undefined;
    }
    const e = event as { error?: unknown; choices?: unknown };
    if (e.error !== undefined && e.error !== null) {
      const message = (e.error as { message?: unknown }).message;
      return new ModelStreamError(
        "error",
        typeof message === "string" ? message.slice(0, 200) : "the answer carried an error",
      );
    }
    if (Array.isArray(e.choices)) {
      for (const choice of e.choices) {
        const reason = (choice as { finish_reason?: unknown } | null)?.finish_reason;
        if (typeof reason === "string" && reason !== "") {
          this.finished = true;
        }
      }
    }
    return undefined;
  }

  /** Why an ended body isn't a whole answer, if it isn't. */
  missing(): ModelStreamError | undefined {
    if (!this.done) {
      return new ModelStreamError("incomplete", "the answer ended before its last event");
    }
    if (!this.finished) {
      return new ModelStreamError("incomplete", "the answer ended without a finish reason");
    }
    return undefined;
  }
}

function checkedBody(
  body: ReadableStream<Uint8Array>,
  idleMs: number,
): ReadableStream<Uint8Array> {
  const reader = body.getReader();
  const watch = new StreamWatch();
  let timer: ReturnType<typeof setTimeout> | undefined;
  const stop = () => {
    if (timer !== undefined) {
      clearTimeout(timer);
      timer = undefined;
    }
  };
  return new ReadableStream<Uint8Array>({
    async pull(controller) {
      const stalled = new Promise<"stalled">((resolve) => {
        timer = setTimeout(() => resolve("stalled"), idleMs);
      });
      let next: ReadableStreamReadResult<Uint8Array> | "stalled";
      try {
        next = await Promise.race([reader.read(), stalled]);
      } catch (error) {
        stop();
        // The connection broke: an incomplete answer, whatever the transport said.
        controller.error(
          new ModelStreamError(
            "incomplete",
            `the connection broke mid-answer (${(error as Error)?.name ?? "error"})`,
          ),
        );
        return;
      } finally {
        stop();
      }
      if (next === "stalled") {
        reader.cancel().catch(() => {});
        controller.error(
          new ModelStreamError("stalled", `no part of the answer for ${Math.round(idleMs / 1000)} s`),
        );
        return;
      }
      if (next.done) {
        const problem = watch.close() ?? watch.missing();
        if (problem) {
          controller.error(problem);
        } else {
          controller.close();
        }
        return;
      }
      const problem = watch.feed(next.value);
      if (problem) {
        reader.cancel().catch(() => {});
        controller.error(problem);
        return;
      }
      controller.enqueue(next.value);
    },
    cancel(reason) {
      stop();
      return reader.cancel(reason);
    },
  });
}

/**
 * `base`, with every successful streamed answer checked as above. An answer that isn't an
 * event stream, or isn't a success, is returned as it came.
 */
export function checkedStreamFetch(
  base: typeof fetch = globalThis.fetch,
  idleMs: number = STREAM_IDLE_MS,
): typeof fetch {
  return (async (input: string | URL | Request, init?: RequestInit) => {
    const res = await base(input, init);
    const type = res.headers.get("content-type") ?? "";
    if (!res.ok || !res.body || !type.toLowerCase().includes("text/event-stream")) {
      return res;
    }
    return new Response(checkedBody(res.body, idleMs), {
      status: res.status,
      statusText: res.statusText,
      headers: res.headers,
    });
  }) as typeof fetch;
}
