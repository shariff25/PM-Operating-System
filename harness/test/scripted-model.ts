/**
 * A chat model that replays a fixed script of AI messages and records every prompt it got.
 * Lets tests drive the agent through tool calls without a live model.
 */
import type { CallbackManagerForLLMRun } from "@langchain/core/callbacks/manager";
import { BaseChatModel } from "@langchain/core/language_models/chat_models";
import { AIMessage, type BaseMessage } from "@langchain/core/messages";
import type { ChatResult } from "@langchain/core/outputs";

export class ScriptedModel extends BaseChatModel {
  readonly prompts: BaseMessage[][] = [];
  private step = 0;

  constructor(private readonly script: AIMessage[]) {
    super({});
  }

  _llmType(): string {
    return "scripted";
  }

  override bindTools(): this {
    return this;
  }

  async _generate(
    messages: BaseMessage[],
    _options: this["ParsedCallOptions"],
    _runManager?: CallbackManagerForLLMRun,
  ): Promise<ChatResult> {
    this.prompts.push(messages);
    const next =
      this.script[Math.min(this.step, this.script.length - 1)] ??
      new AIMessage("done");
    this.step += 1;
    // Fresh copy each call so message ids do not collide across steps.
    const message = new AIMessage({
      content: next.content,
      tool_calls: next.tool_calls,
    });
    return {
      generations: [
        {
          message,
          text: typeof message.content === "string" ? message.content : "",
        },
      ],
    };
  }
}

export function systemText(messages: BaseMessage[]): string {
  return messages
    .filter((m) => m.getType() === "system")
    .map((m) => (typeof m.content === "string" ? m.content : JSON.stringify(m.content)))
    .join("\n");
}
