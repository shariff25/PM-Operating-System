import { join } from "node:path";
import { describe, expect, it } from "vitest";
import {
  buildInterruptOn,
  harnessToolName,
  harnessToolNames,
  loadToolRegistry,
  parseToolRegistry,
} from "../src/registry.js";

const CONTENT = join(import.meta.dirname, "..", "..", "content");

describe("tool registry", () => {
  const registry = loadToolRegistry(join(CONTENT, "tools.yaml"));

  it("loads the repo's tools.yaml", () => {
    expect(registry.tools.length).toBeGreaterThan(0);
    expect(registry.tools.map((t) => t.name)).toContain("jira.create_issue");
  });

  it("pauses on every write tool and nothing else", () => {
    const interruptOn = buildInterruptOn(registry);
    const writes = registry.tools
      .filter((t) => t.kind === "write")
      .map((t) => harnessToolName(t.name))
      .sort();
    expect(Object.keys(interruptOn).sort()).toEqual(writes);
    expect(writes).toEqual([
      "bets_record",
      "confluence_create_draft",
      "confluence_update_page",
      "jira_create_issue",
      "slack_post_reply",
      "slack_send_message",
    ]);
    for (const config of Object.values(interruptOn)) {
      expect(config.allowedDecisions).toEqual(["approve", "edit", "reject"]);
    }
  });

  it("never pauses on read or internal tools", () => {
    const interruptOn = buildInterruptOn(registry);
    expect(interruptOn).not.toHaveProperty("context_search");
    expect(interruptOn).not.toHaveProperty("memory_update");
  });

  it("maps dotted names to function-safe names", () => {
    expect(harnessToolName("confluence.create_draft")).toBe(
      "confluence_create_draft",
    );
  });

  it("rejects names that collide after mapping", () => {
    const clash = parseToolRegistry(
      "tools:\n  - {name: a.b_c, system: a, kind: read}\n  - {name: a_b.c, system: a_b, kind: read}\n",
    );
    expect(() => harnessToolNames(clash)).toThrow(/both map/);
  });

  it("rejects an unknown kind", () => {
    expect(() =>
      parseToolRegistry("tools:\n  - {name: a.b, system: a, kind: delete}\n"),
    ).toThrow();
  });
});
