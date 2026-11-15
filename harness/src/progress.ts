/**
 * What a PM sees while a bot works (D6): milestones and a short checklist, each change sent
 * once. Also the LangChain callback handler that turns every model turn and tool call into
 * trace entries and progress.
 */
import { BaseCallbackHandler } from "@langchain/core/callbacks/base";
import type { BaseMessage } from "@langchain/core/messages";
import type { Serialized } from "@langchain/core/load/serializable";
import type { LLMResult } from "@langchain/core/outputs";
import type { ChecklistItem, TaskEvent, TraceEntry } from "./runtime-client.js";
import type { ToolKind } from "./registry.js";

export const FIRST_MILESTONE = "Working on it";

/** Milestone text per tool family, for reads and for writes (drafts awaiting approval). */
const FAMILY_MILESTONES: Record<string, { read: string; write: string }> = {
  docs: { read: "Searching the support docs", write: "Searching the support docs" },
  jira: { read: "Reading Jira", write: "Drafting in Jira" },
  confluence: { read: "Reading Confluence", write: "Drafting in Confluence" },
  slack: { read: "Reading Slack", write: "Drafting a Slack message" },
  context: { read: "Checking team context", write: "Checking team context" },
  memory: { read: "Checking its notes", write: "Updating its notes" },
  stats: { read: "Running the numbers", write: "Running the numbers" },
};

/** The tool family of a harness tool name: `jira_create_issue` is `jira`. */
export function toolFamily(name: string): string {
  return name.split("_", 1)[0] ?? name;
}

/**
 * The milestone for a call to `name`, or `null` for tools that aren't a PM-visible step
 * (the agent's own file, todo and sub-agent tools).
 */
export function milestoneFor(name: string, kind: ToolKind | undefined): string | null {
  const family = FAMILY_MILESTONES[toolFamily(name)];
  if (!family || kind === undefined) {
    return null;
  }
  return kind === "write" ? family.write : family.read;
}

/**
 * Tracks milestones and the checklist for one task and emits each change once.
 *
 * The checklist is the agent's own todo list once it writes one (`write_todos`); until then
 * it is the milestones so far, each one done when the next starts.
 */
export class Progress {
  private readonly milestones: string[] = [];
  private todos: ChecklistItem[] | null = null;
  private lastChecklist = "";

  constructor(private readonly emit: (event: TaskEvent) => void) {}

  milestone(text: string): void {
    if (this.milestones.includes(text)) {
      return;
    }
    this.milestones.push(text);
    this.emit({ kind: "milestone", text });
    this.sendChecklist();
  }

  setTodos(todos: ChecklistItem[]): void {
    this.todos = todos;
    this.sendChecklist();
  }

  /** Marks every step done (the task finished). */
  finish(): void {
    if (this.todos) {
      this.todos = this.todos.map((t) => ({ ...t, done: true }));
    }
    this.sendChecklist(true);
  }

  checklist(allDone = false): ChecklistItem[] {
    if (this.todos) {
      return this.todos;
    }
    return this.milestones.map((item, i) => ({
      item,
      done: allDone || i < this.milestones.length - 1,
    }));
  }

  private sendChecklist(allDone = false): void {
    const checklist = this.checklist(allDone);
    const key = JSON.stringify(checklist);
    if (checklist.length === 0 || key === this.lastChecklist) {
      return;
    }
    this.lastChecklist = key;
    this.emit({ kind: "checklist", checklist });
  }
}

function messageContent(message: BaseMessage): unknown {
  return message.content;
}

function systemOf(messages: BaseMessage[]): string {
  return messages
    .filter((m) => m.getType() === "system")
    .map((m) => (typeof m.content === "string" ? m.content : JSON.stringify(m.content)))
    .join("\n");
}

function parseTodos(input: string): ChecklistItem[] | null {
  try {
    const parsed = JSON.parse(input) as { todos?: unknown };
    if (!Array.isArray(parsed.todos)) {
      return null;
    }
    return parsed.todos
      .filter(
        (t): t is { content: string; status?: string } =>
          typeof t === "object" && t !== null && typeof (t as { content?: unknown }).content === "string",
      )
      .map((t) => ({ item: t.content, done: t.status === "completed" }));
  } catch {
    return null;
  }
}

function toolOutput(output: unknown): unknown {
  if (typeof output === "object" && output !== null && "content" in output) {
    return (output as { content: unknown }).content;
  }
  return output;
}

/**
 * Sends every model turn and tool call to the trace, and tool use to `Progress`. Handlers are
 * awaited (not run in the background), so entries keep the order the agent produced them.
 */
export class TaskTracer extends BaseCallbackHandler {
  name = "pmos-task-tracer";
  override awaitHandlers = true;

  private readonly tools = new Map<string, { name: string; toolCallId?: string }>();
  private readonly systemsSeen = new Set<string>();

  constructor(
    private readonly trace: (entry: TraceEntry) => void,
    private readonly progress: Progress,
    private readonly toolKind: (name: string) => ToolKind | undefined,
  ) {
    super();
  }

  override handleChatModelStart(_llm: Serialized, prompts: BaseMessage[][]): void {
    // The full prompt once per distinct system prompt (the agent, then each sub-agent); later
    // turns add only messages that are already traced as replies and tool results.
    const messages = prompts[0] ?? [];
    const system = systemOf(messages);
    if (this.systemsSeen.has(system)) {
      return;
    }
    this.systemsSeen.add(system);
    this.trace({
      kind: "prompt",
      body: {
        messages: messages.map((m) => ({ role: m.getType(), content: messageContent(m) })),
      },
    });
  }

  override handleLLMEnd(output: LLMResult): void {
    const generation = output.generations[0]?.[0] as
      | { text?: string; message?: BaseMessage & { tool_calls?: unknown[] } }
      | undefined;
    const message = generation?.message;
    this.trace({
      kind: "model_reply",
      body: message
        ? { content: message.content, tool_calls: message.tool_calls ?? [] }
        : { content: generation?.text ?? "" },
    });
  }

  override handleToolStart(
    _tool: Serialized,
    input: string,
    runId: string,
    _parentRunId?: string,
    _tags?: string[],
    _metadata?: Record<string, unknown>,
    runName?: string,
    toolCallId?: string,
  ): void {
    const name = runName ?? "unknown";
    this.tools.set(runId, { name, toolCallId });
    let args: unknown = input;
    try {
      args = JSON.parse(input);
    } catch {
      // Keep the raw string.
    }
    this.trace({ kind: "tool_call", body: { name, tool_call_id: toolCallId, args } });

    if (name === "write_todos") {
      const todos = parseTodos(input);
      if (todos) {
        this.progress.setTodos(todos);
      }
      return;
    }
    const milestone = milestoneFor(name, this.toolKind(name));
    if (milestone) {
      this.progress.milestone(milestone);
    }
  }

  override handleToolEnd(output: unknown, runId: string): void {
    const call = this.tools.get(runId);
    this.tools.delete(runId);
    this.trace({
      kind: "tool_result",
      body: { name: call?.name, tool_call_id: call?.toolCallId, content: toolOutput(output) },
    });
  }

  override handleToolError(error: Error, runId: string): void {
    const call = this.tools.get(runId);
    this.tools.delete(runId);
    this.trace({
      kind: "tool_result",
      body: { name: call?.name, tool_call_id: call?.toolCallId, error: error.message },
    });
  }
}
