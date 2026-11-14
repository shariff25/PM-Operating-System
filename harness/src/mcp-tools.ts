/**
 * The tool server's MCP tools for one session, as LangChain tools.
 *
 * Uses the official MCP TypeScript SDK client over Streamable HTTP. The session token goes in
 * the `Authorization` header, never in the URL (docs/toolserver.md). Each tool keeps the
 * server's name and JSON input schema; the tool server validates arguments itself.
 *
 * Writes come back as `{"status": "pending_approval", "approval_id": ...}`; every such id is
 * reported through `onApproval` so the worker can hand them to the runtime on completion.
 */
import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";
import type { CallToolResult } from "@modelcontextprotocol/sdk/types.js";
import { type StructuredToolInterface, tool } from "@langchain/core/tools";

export const HARNESS_CLIENT = { name: "pmos-harness", version: "0.1.0" } as const;

export interface McpToolHooks {
  /** A write is waiting for the PM's approval. */
  onApproval?: (approvalId: string, tool: string) => void;
  /** The tool server refused a call (an `isError` result). */
  onToolError?: (tool: string, message: string) => void;
}

export interface McpToolsOptions extends McpToolHooks {
  /** The tool server's MCP endpoint, e.g. `http://toolserver.pmos.internal:8710/mcp`. */
  url: string;
  /** The session token the runtime created for this task. */
  sessionToken: string;
}

export interface McpToolSet {
  tools: StructuredToolInterface[];
  close(): Promise<void>;
}

/** The text the model sees for a tool result: text blocks, else the structured content. */
export function resultText(result: CallToolResult): string {
  const texts = (result.content ?? [])
    .filter((block): block is { type: "text"; text: string } => block.type === "text")
    .map((block) => block.text);
  if (texts.length > 0) {
    return texts.join("\n");
  }
  if (result.structuredContent !== undefined) {
    return JSON.stringify(result.structuredContent);
  }
  return "";
}

/** Approval ids in a tool result: from `structuredContent`, else from JSON text blocks. */
export function pendingApprovals(result: CallToolResult): string[] {
  const candidates: unknown[] = [];
  if (result.structuredContent !== undefined) {
    candidates.push(result.structuredContent);
  } else {
    for (const block of result.content ?? []) {
      if (block.type === "text") {
        try {
          candidates.push(JSON.parse(block.text));
        } catch {
          // Not JSON: not an approval.
        }
      }
    }
  }
  const ids: string[] = [];
  for (const value of candidates) {
    if (
      typeof value === "object" &&
      value !== null &&
      (value as { status?: unknown }).status === "pending_approval" &&
      typeof (value as { approval_id?: unknown }).approval_id === "string"
    ) {
      ids.push((value as { approval_id: string }).approval_id);
    }
  }
  return ids;
}

export async function connectMcpTools(options: McpToolsOptions): Promise<McpToolSet> {
  const transport = new StreamableHTTPClientTransport(new URL(options.url), {
    requestInit: { headers: { authorization: `Bearer ${options.sessionToken}` } },
  });
  const client = new Client(HARNESS_CLIENT);
  await client.connect(transport);

  try {
    const listed = [];
    let cursor: string | undefined;
    do {
      const page = await client.listTools(cursor ? { cursor } : undefined);
      listed.push(...page.tools);
      cursor = page.nextCursor;
    } while (cursor);

    const tools = listed.map((spec) =>
      tool(
        async (args: Record<string, unknown>, config) => {
          const result = (await client.callTool(
            { name: spec.name, arguments: args },
            undefined,
            { signal: config?.signal },
          )) as CallToolResult;
          const text = resultText(result);
          if (result.isError) {
            options.onToolError?.(spec.name, text);
            return `Error: ${text}`;
          }
          for (const id of pendingApprovals(result)) {
            options.onApproval?.(id, spec.name);
          }
          return text;
        },
        {
          name: spec.name,
          description: spec.description ?? spec.name,
          schema: spec.inputSchema as Record<string, unknown> & { type: "object" },
        },
      ),
    );
    return { tools, close: () => client.close() };
  } catch (error) {
    await client.close().catch(() => {});
    throw error;
  }
}
