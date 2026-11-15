/**
 * The read-only template, reviewer and playbook mounts, and the review panel's sub-agents.
 * Uses a small content fixture of its own, so it doesn't depend on the real content tree.
 */
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { AIMessage, HumanMessage, ToolMessage } from "@langchain/core/messages";
import { tool } from "@langchain/core/tools";
import { afterEach, describe, expect, it } from "vitest";
import { z } from "zod";
import { MODEL_ALIASES, createPmosAgent, reviewerSubagents } from "../src/agent.js";
import { ScriptedModel, systemText } from "./scripted-model.js";

const TOOLS_YAML = `tools:
  - {name: context.search, system: context, kind: read}
  - {name: memory.update, system: memory, kind: internal}
  - {name: confluence.read, system: confluence, kind: read}
  - {name: confluence.create_draft, system: confluence, kind: write}
  - {name: stats.sample_size, system: stats, kind: read}
`;

const skill = (name: string) =>
  `---\nname: ${name}\ndescription: Test skill ${name}.\nmetadata:\n  owner: "@pm"\n  version: "0.1.0"\n  model: "reasoning"\n  data-class: "internal"\n  tools: "context.search"\n---\n\n# /${name}\n\nSteps.\n`;

const reviewer = (title: string, lens: string, marker: string) =>
  `---\nname: ${title}\nlens: ${lens}\nflags:\n  - Something\n---\n\n# ${title}\n\n${marker}: check the draft.\n`;

let roots: string[] = [];
afterEach(() => {
  for (const root of roots) rmSync(root, { recursive: true, force: true });
  roots = [];
});

function contentFixture(options: { playbooks?: boolean; reviewers?: boolean } = {}): string {
  const root = mkdtempSync(join(tmpdir(), "pmos-mounts-"));
  roots.push(root);
  const write = (rel: string, text: string) => {
    mkdirSync(dirname(join(root, rel)), { recursive: true });
    writeFileSync(join(root, rel), text);
  };
  write("tools.yaml", TOOLS_YAML);
  write("master/AGENTS.md", "# PM OS team context\n\nCite or flag.\n");
  write("skills/ask/SKILL.md", skill("ask"));
  write("skills/review/SKILL.md", skill("review"));
  write("templates/prd.md", '---\nowner: "@pm"\nversion: "0.1.0"\n---\n\n# PRD\n\nTEMPLATE-MARKER\n');
  if (options.playbooks ?? true) {
    write(
      "playbooks/bar.md",
      '---\ntitle: The bar\nowner: "@pm"\nversion: "0.1.0"\n---\n\n# The bar\n\nPLAYBOOK-MARKER\n',
    );
  }
  if (options.reviewers ?? true) {
    write("reviewers/README.md", "# Reviewers\n");
    write("reviewers/senior-pm.md", reviewer("Senior PM", "Product judgement and the bar", "SENIOR-PM-RUBRIC"));
    write("reviewers/data-scientist.md", reviewer("Data scientist", "Metrics and experiment design", "DS-RUBRIC"));
  }
  return root;
}

const thread = () => ({ configurable: { thread_id: crypto.randomUUID() } });

const call = (name: string, args: Record<string, unknown>) =>
  new AIMessage({
    content: "",
    tool_calls: [{ id: crypto.randomUUID(), name, args }],
  });

const toolText = (messages: unknown[]) =>
  messages
    .filter(ToolMessage.isInstance)
    .map((m) => (typeof m.content === "string" ? m.content : JSON.stringify(m.content)));

/** Session tools as the MCP client builds them: harness names, counting their calls. */
function sessionTools() {
  const calls: string[] = [];
  const fake = (name: string) =>
    tool(
      async () => {
        calls.push(name);
        return `${name} ran`;
      },
      { name, description: `Test ${name}`, schema: z.object({}).passthrough() },
    );
  const tools = [
    fake("context_search"),
    fake("memory_update"),
    fake("confluence_read"),
    fake("confluence_create_draft"),
    fake("stats_sample_size"),
  ];
  return { tools, calls };
}

describe("read-only content mounts", () => {
  it("reads templates, playbooks and reviewers and can't write them", async () => {
    const contentRoot = contentFixture();
    const before = {
      prd: readFileSync(join(contentRoot, "templates", "prd.md"), "utf8"),
      bar: readFileSync(join(contentRoot, "playbooks", "bar.md"), "utf8"),
    };
    const model = new ScriptedModel([
      call("read_file", { file_path: "/content/templates/prd.md" }),
      call("read_file", { file_path: "/content/playbooks/bar.md" }),
      call("read_file", { file_path: "/content/reviewers/senior-pm.md" }),
      call("write_file", { file_path: "/content/templates/prd.md", content: "pwned" }),
      call("edit_file", {
        file_path: "/content/playbooks/bar.md",
        old_string: "PLAYBOOK-MARKER",
        new_string: "pwned",
      }),
      call("write_file", { file_path: "/content/playbooks/new.md", content: "pwned" }),
      new AIMessage("done"),
    ]);
    const agent = createPmosAgent({ contentRoot, model, skills: ["ask"] });
    const result = await agent.invoke({ messages: [new HumanMessage("read")] }, thread());
    const out = toolText(result.messages);

    expect(out[0]).toContain("TEMPLATE-MARKER");
    expect(out[1]).toContain("PLAYBOOK-MARKER");
    expect(out[2]).toContain("SENIOR-PM-RUBRIC");
    for (const denied of out.slice(3, 6)) {
      expect(denied).toContain("permission denied");
    }
    expect(readFileSync(join(contentRoot, "templates", "prd.md"), "utf8")).toBe(before.prd);
    expect(readFileSync(join(contentRoot, "playbooks", "bar.md"), "utf8")).toBe(before.bar);
  });

  it("mounts a folder only when the content repo has it", async () => {
    const contentRoot = contentFixture({ playbooks: false });
    // A playbook written later into the content root isn't reachable: there is no mount.
    const model = new ScriptedModel([
      call("ls", { path: "/content/templates/" }),
      call("ls", { path: "/content/playbooks/" }),
      call("read_file", { file_path: "/content/playbooks/bar.md" }),
      new AIMessage("done"),
    ]);
    const agent = createPmosAgent({ contentRoot, model, skills: ["ask"] });
    mkdirSync(join(contentRoot, "playbooks"));
    writeFileSync(join(contentRoot, "playbooks", "bar.md"), "PLAYBOOK-MARKER");
    const result = await agent.invoke({ messages: [new HumanMessage("read")] }, thread());
    const [templates, playbooks, read] = toolText(result.messages);
    expect(templates).toContain("prd.md");
    expect(playbooks).not.toContain("bar.md");
    expect(read).not.toContain("PLAYBOOK-MARKER");
  });
});

describe("review sub-agents", () => {
  it("builds one per reviewer rubric with the read tools it is given", () => {
    const contentRoot = contentFixture();
    const { tools } = sessionTools();
    const subagents = reviewerSubagents(contentRoot, tools.slice(0, 1));
    expect(subagents.map((s) => s.name)).toEqual(["data-scientist", "senior-pm"]);
    const pm = subagents.find((s) => s.name === "senior-pm");
    expect(pm?.description).toBe("Product judgement and the bar");
    expect(pm?.systemPrompt).toContain("SENIOR-PM-RUBRIC");
    expect(pm?.systemPrompt).not.toContain("lens:");
    expect(pm?.tools?.map((t) => t.name)).toEqual(["context_search"]);
    expect(reviewerSubagents(contentFixture({ reviewers: false }), tools)).toEqual([]);
  });

  it("gives a role with review one sub-agent per reviewer, with read tools only", async () => {
    const contentRoot = contentFixture();
    const { tools, calls } = sessionTools();
    // Asking for a reviewer that doesn't exist fails, naming every sub-agent there is.
    const probe = createPmosAgent({
      contentRoot,
      model: new ScriptedModel([call("task", { description: "x", subagent_type: "nobody" })]),
      tools,
      skills: ["ask", "review"],
    });
    await expect(
      probe.invoke({ messages: [new HumanMessage("review it")] }, thread()),
    ).rejects.toThrow(/allowed types are `general-purpose`, `data-scientist`, `senior-pm`$/);

    const model = new ScriptedModel([
      call("task", { description: "Review the PRD: DOC-TEXT", subagent_type: "senior-pm" }),
      // Inside the sub-agent: a write is not among its tools; a read is.
      call("confluence_create_draft", {}),
      call("memory_update", {}),
      call("confluence_read", {}),
      call("stats_sample_size", {}),
      new AIMessage("FINDINGS: one blocker"),
      new AIMessage("done"),
    ]);
    // As in the bot worker: writes go to the tool server's approvals, not a harness pause.
    const agent = createPmosAgent({
      contentRoot,
      model,
      tools,
      skills: ["ask", "review"],
      interruptOnWrites: false,
    });
    const result = await agent.invoke({ messages: [new HumanMessage("review it")] }, thread());
    const out = toolText(result.messages);

    expect(out.at(-1)).toContain("FINDINGS: one blocker");
    // The sub-agent ran the read tools and could not reach the write or internal ones.
    expect(calls).toEqual(["confluence_read", "stats_sample_size"]);

    // The sub-agent's prompt is the rubric; it got the document as its task.
    const subPrompt = model.prompts.find((p) => systemText(p).includes("SENIOR-PM-RUBRIC"));
    expect(subPrompt).toBeDefined();
    expect(JSON.stringify(subPrompt?.map((m) => m.content))).toContain("DOC-TEXT");
  });

  it("gives a role without review no reviewer sub-agents", async () => {
    const contentRoot = contentFixture();
    const { tools, calls } = sessionTools();
    const model = new ScriptedModel([
      call("task", { description: "Review this", subagent_type: "senior-pm" }),
      new AIMessage("done"),
    ]);
    const agent = createPmosAgent({ contentRoot, model, tools, skills: ["ask"] });
    await expect(
      agent.invoke({ messages: [new HumanMessage("review it")] }, thread()),
    ).rejects.toThrow(/the only allowed types are `general-purpose`$/);
    expect(calls).toEqual([]);
  });
});

describe("model aliases", () => {
  it("include the reasoning role", () => {
    expect(MODEL_ALIASES).toEqual({
      default: "pmos/default",
      extraction: "pmos/extraction",
      reasoning: "pmos/reasoning",
    });
  });
});
