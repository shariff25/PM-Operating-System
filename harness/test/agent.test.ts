import { cpSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { AIMessage, HumanMessage, ToolMessage } from "@langchain/core/messages";
import { tool } from "@langchain/core/tools";
import { MemorySaver } from "@langchain/langgraph";
import type { HITLRequest } from "langchain";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { z } from "zod";
import { createPmosAgent, memorySources } from "../src/agent.js";
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

describe("memorySources", () => {
  it("adds a personal file only when it exists", () => {
    expect(memorySources(contentRoot)).toEqual(["/content/master/AGENTS.md"]);
    expect(memorySources(contentRoot, "nobody")).toEqual(["/content/master/AGENTS.md"]);
  });

  it("rejects handles that could escape the people folder", () => {
    expect(() => memorySources(contentRoot, "../../etc")).toThrow(/invalid user handle/);
  });
});
