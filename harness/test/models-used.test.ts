import { describe, expect, it } from "vitest";
import { ModelSourceGuard } from "../src/agent.js";
import { ClaimSchema } from "../src/runtime-client.js";
import { gatewayHeaders } from "../src/worker.js";

// Slice MP7 (docs/model-providers-spec.md §4.9, §4.10): the worker reads which labelled model
// answered every call, and passes on the runtime's pin to the primary.

const SHA_A = "a".repeat(64);
const SHA_B = "b".repeat(64);

function answer(headers: Record<string, string>, status = 200): typeof fetch {
  return async () =>
    new Response("{}", {
      status,
      headers: { "x-pmos-model-source": "hosted-test", ...headers },
    });
}

describe("ModelSourceGuard", () => {
  it("records each answering label, its hash and whether a fallback answered", async () => {
    const responses: Record<string, string>[] = [
      { "x-pmos-model": "hosted-model-a", "x-pmos-model-sha256": SHA_A, "x-pmos-fallback": "0" },
      { "x-pmos-model": "hosted-model-a", "x-pmos-model-sha256": SHA_A, "x-pmos-fallback": "0" },
      { "x-pmos-model": "hosted-model-b", "x-pmos-model-sha256": SHA_B, "x-pmos-fallback": "1" },
      // Not a label: ignored.
      { "x-pmos-model": "a model id with spaces" },
    ];
    let i = 0;
    const base: typeof fetch = async (input, init) => answer(responses[i++] ?? {})(input, init);
    const guard = new ModelSourceGuard("hosted-test", base);
    for (let n = 0; n < responses.length; n++) {
      await guard.fetch("http://gateway/v1/chat/completions");
    }
    expect(guard.modelsUsed()).toEqual([
      { label: "hosted-model-a", model_sha256: SHA_A, calls: 2 },
      { label: "hosted-model-b", model_sha256: SHA_B, calls: 1 },
    ]);
    expect(guard.fallbackUsed).toBe(true);
  });

  it("records nothing from an error answer or a call it refused", async () => {
    const guard = new ModelSourceGuard(
      "hosted-test",
      answer({ "x-pmos-model": "hosted-model-a", "x-pmos-fallback": "1" }, 503),
    );
    await guard.fetch("http://gateway/v1/chat/completions");
    expect(guard.modelsUsed()).toEqual([]);
    expect(guard.fallbackUsed).toBe(false);
  });
});

describe("gatewayHeaders", () => {
  const claim = (headers: Record<string, unknown>) =>
    ClaimSchema.parse({
      task: {
        id: "tsk_1",
        bot: "docs",
        requested_by: "ana",
        source: "eval",
        priority: "personal",
        text: "Q",
        status: "working",
        checklist: [],
        approvals: [],
        attempts: 1,
      },
      bot: {
        name: "docs",
        role: "docs-helper",
        scope: "team",
        owner: "ana",
        description: "d",
        reads: {},
        actions: {},
        job: "j",
      },
      role: {
        name: "docs-helper",
        description: "d",
        skills: [],
        data_class: "public",
        model: "default",
        actions: {},
        instructions: "i",
      },
      tools: [],
      session_token: "sess-1",
      lease_until: new Date().toISOString(),
      gateway: { model: "pmos/default", headers: { "X-Pmos-Model-Source": "local", ...headers } },
    });

  it("passes on the runtime's pin to the primary, and nothing that would widen it", () => {
    expect(gatewayHeaders(claim({ "X-Pmos-Fallback": "off" }))["X-Pmos-Fallback"]).toBe("off");
    expect(gatewayHeaders(claim({}))["X-Pmos-Fallback"]).toBeUndefined();
    expect(gatewayHeaders(claim({ "X-Pmos-Fallback": "on" }))["X-Pmos-Fallback"]).toBeUndefined();
    expect(gatewayHeaders(claim({ "X-Pmos-Route-Model": "x" }))["X-Pmos-Route-Model"]).toBeUndefined();
  });
});
