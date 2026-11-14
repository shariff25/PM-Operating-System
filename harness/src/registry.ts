/**
 * The tool registry (`content/tools.yaml`) as the harness sees it.
 *
 * The Rust `pmos-contract` crate is the authority on the file's format and `pmos lint`
 * validates it in CI; this module only reads it and derives harness configuration.
 */
import { readFileSync } from "node:fs";
import type { InterruptOnConfig } from "langchain";
import { parse } from "yaml";
import { z } from "zod";

const ToolDefSchema = z.object({
  name: z.string().min(1),
  system: z.string().min(1),
  kind: z.enum(["read", "internal", "write"]),
  description: z.string().optional(),
});

const ToolRegistrySchema = z.object({ tools: z.array(ToolDefSchema) });

export type ToolDef = z.infer<typeof ToolDefSchema>;
export type ToolKind = ToolDef["kind"];
export type ToolRegistry = z.infer<typeof ToolRegistrySchema>;

export function parseToolRegistry(text: string): ToolRegistry {
  return ToolRegistrySchema.parse(parse(text));
}

export function loadToolRegistry(path: string): ToolRegistry {
  return parseToolRegistry(readFileSync(path, "utf8"));
}

/**
 * The name a tool has inside the harness. Registry names use dots (`jira.create_issue`);
 * OpenAI-style function names allow only letters, digits, `_` and `-`, so dots become `_`.
 */
export function harnessToolName(name: string): string {
  return name.replaceAll(".", "_");
}

/** Map of harness tool name to registry entry. Throws if two names collide. */
export function harnessToolNames(registry: ToolRegistry): Map<string, ToolDef> {
  const byName = new Map<string, ToolDef>();
  for (const tool of registry.tools) {
    const name = harnessToolName(tool.name);
    const existing = byName.get(name);
    if (existing) {
      throw new Error(
        `tools ${existing.name} and ${tool.name} both map to harness name ${name}`,
      );
    }
    byName.set(name, tool);
  }
  return byName;
}

/** Decisions a PM can make on a proposed external write (PRD: approve, edit or reject). */
export const WRITE_DECISIONS = ["approve", "edit", "reject"] as const;

/**
 * Deep Agents `interruptOn` config: pause before every `write` tool so the PM approves,
 * edits or rejects that one action (OUT-4). Read and internal tools run without a pause.
 * The tool server enforces the same rule again behind the harness.
 */
export function buildInterruptOn(
  registry: ToolRegistry,
): Record<string, InterruptOnConfig> {
  const config: Record<string, InterruptOnConfig> = {};
  for (const [name, tool] of harnessToolNames(registry)) {
    if (tool.kind === "write") {
      config[name] = {
        allowedDecisions: [...WRITE_DECISIONS],
        description: `PM OS needs your approval to run ${tool.name}${tool.description ? `: ${tool.description}` : ""}`,
      };
    }
  }
  return config;
}
