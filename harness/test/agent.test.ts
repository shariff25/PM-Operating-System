import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { AIMessage, HumanMessage, ToolMessage } from "@langchain/core/messages";
import { tool } from "@langchain/core/tools";
import { MemorySaver } from "@langchain/langgraph";
import type { HITLRequest } from "langchain";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { z } from "zod";
import { createPmosAgent, gatewayModel, memorySources } from "../src/agent.js";
import { ScriptedModel, systemText } from "./scripted-model.js";

const REPO_CONTENT = join(import.meta.dirname, "..", "..", "content");

// Work on a copy so a failing permission check can never touch the repo's content.
let contentRoot: string;
beforeAll(() => {
  contentRoot = mkdtempSync(join(tmpdir(), "pmos-content-"));
  cpSync(REPO_CONTENT, contentRoot, { recursive: true });
});
afterAll(() => rmSync(contentRoot, { recursive: true, force: true }));

const thread = () => ({ configurable: { thread_id: crypto.randomUUID() } });

describe("gatewayModel", () => {
  it("never retries on its own: the gateway retries 429, 503 and connection failures", () => {
    const model = gatewayModel({ gatewayUrl: "http://127.0.0.1:9/v1" });
    // LangChain's per-model retry count (default 6) is what would multiply with the gateway's.
    expect((model as unknown as { caller: { maxRetries: number } }).caller.maxRetries).toBe(0);
  });
});

describe("createPmosAgent", () => {
  it("loads the team master context and the skill catalog", async () => {
    const model = new ScriptedModel([new AIMessage("hello")]);
    const agent = createPmosAgent({ contentRoot, model });
    await agent.invoke({ messages: [new HumanMessage("hi")] }, thread());

    const system = systemText(model.prompts[0] ?? []);
    expect(system).toContain("PM OS team context");
    expect(system).toContain("Cite or flag");
    for (const skill of ["ask", "meeting-notes", "prd-draft"]) {
      expect(system).toContain(skill);
    }
  });

  it("refuses writes to the content repo", async () => {
    const target = join(contentRoot, "skills", "ask", "SKILL.md");
    const before = readFileSync(target, "utf8");
    const model = new ScriptedModel([
      new AIMessage({
        content: "",
        tool_calls: [
          {
            id: "call_1",
            name: "write_file",
            args: { file_path: "/content/skills/ask/SKILL.md", content: "pwned" },
          },
        ],
      }),
      new AIMessage("done"),
    ]);
    const agent = createPmosAgent({ contentRoot, model });
    const result = await agent.invoke(
      { messages: [new HumanMessage("overwrite the ask skill")] },
      thread(),
    );

    expect(readFileSync(target, "utf8")).toBe(before);
    const toolMessages = result.messages.filter(ToolMessage.isInstance);
    expect(toolMessages.some((m) => String(m.content).includes("permission denied"))).toBe(
      true,
    );
  });

  it("allows scratch writes outside the content mount", async () => {
    const model = new ScriptedModel([
      new AIMessage({
        content: "",
        tool_calls: [
          {
            id: "call_1",
            name: "write_file",
            args: { file_path: "/scratch/notes.md", content: "draft" },
          },
        ],
      }),
      new AIMessage("done"),
    ]);
    const agent = createPmosAgent({ contentRoot, model });
    const result = await agent.invoke(
      { messages: [new HumanMessage("take notes")] },
      thread(),
    );
    const toolMessages = result.messages.filter(ToolMessage.isInstance);
    expect(toolMessages.some((m) => String(m.content).includes("permission denied"))).toBe(
      false,
    );
  });

  it("pauses for approval before an external write", async () => {
    let created = 0;
    const createIssue = tool(
      async () => {
        created += 1;
        return "PMOS-1";
      },
      {
        name: "jira_create_issue",
        description: "Create a Jira issue",
        schema: z.object({ summary: z.string() }),
      },
    );
    const model = new ScriptedModel([
      new AIMessage({
        content: "",
        tool_calls: [
          { id: "call_1", name: "jira_create_issue", args: { summary: "Add SSO" } },
        ],
      }),
      new AIMessage("done"),
    ]);
    const agent = createPmosAgent({
      contentRoot,
      model,
      tools: [createIssue],
      checkpointer: new MemorySaver(),
    });
    const result = await agent.invoke(
      { messages: [new HumanMessage("file a ticket")] },
      thread(),
    );

    expect(created).toBe(0);
    expect(result.__interrupt__).toHaveLength(1);
    const request = result.__interrupt__?.[0]?.value as HITLRequest;
    expect(request.actionRequests.map((a) => a.name)).toEqual(["jira_create_issue"]);
    expect(request.reviewConfigs[0]?.allowedDecisions).toEqual([
      "approve",
      "edit",
      "reject",
    ]);
  });
});

describe("content mount", () => {
  const readFile = (path: string) =>
    new AIMessage({
      content: "",
      tool_calls: [{ id: crypto.randomUUID(), name: "read_file", args: { file_path: path } }],
    });

  it("does not expose the context library or other PMs' personal context", async () => {
    // Context files carry data-class labels that only the tool server enforces, so the
    // sandbox must never read them directly.
    mkdirSync(join(contentRoot, "context", "products"), { recursive: true });
    writeFileSync(join(contentRoot, "context", "products", "secret.md"), "RESTRICTED-MARKER");
    mkdirSync(join(contentRoot, "master", "people", "bea"), { recursive: true });
    writeFileSync(join(contentRoot, "master", "people", "bea", "AGENTS.md"), "BEA-PRIVATE");
    mkdirSync(join(contentRoot, "master", "people", "ana"), { recursive: true });
    writeFileSync(join(contentRoot, "master", "people", "ana", "AGENTS.md"), "ANA-NOTES");

    const model = new ScriptedModel([
      readFile("/content/context/products/secret.md"),
      readFile("/content/master/people/bea/AGENTS.md"),
      readFile("/content/skills/ask/SKILL.md"),
      new AIMessage("done"),
    ]);
    const agent = createPmosAgent({ contentRoot, model, user: "ana" });
    const result = await agent.invoke({ messages: [new HumanMessage("read")] }, thread());
    const out = result.messages
      .filter(ToolMessage.isInstance)
      .map((m) => (typeof m.content === "string" ? m.content : JSON.stringify(m.content)))
      .join("\n");

    expect(out).not.toContain("RESTRICTED-MARKER");
    expect(out).not.toContain("BEA-PRIVATE");
    expect(out).toContain("name: ask");
    // The PM's own personal context is still loaded.
    expect(systemText(model.prompts[0] ?? [])).toContain("ANA-NOTES");
  });
});

describe("memorySources", () => {
  it("adds a personal file only when it exists", () => {
    expect(memorySources(contentRoot)).toEqual(["/content/master/AGENTS.md"]);
    expect(memorySources(contentRoot, "nobody")).toEqual(["/content/master/AGENTS.md"]);
  });

  it("rejects handles that could escape the people folder", () => {
    expect(() => memorySources(contentRoot, "../../etc")).toThrow(/invalid user handle/);
  });
});
