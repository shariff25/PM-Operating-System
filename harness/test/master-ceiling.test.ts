/**
 * Slice H12 (docs/openshell-any-host-spec.md §3.6, "An optional data-class ceiling", r7 V11):
 * master context never passes through the tool server, so under a hosted ceiling below
 * `restricted` it counts as `restricted` unless its first line is a sensitivity marker, and a
 * file above the ceiling stops the task, named with its class, never its text.
 */
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { MasterContextAboveCeilingError, checkMasterContext, masterContextClass } from "../src/agent.js";
import { ClaimSchema } from "../src/runtime-client.js";

let roots: string[] = [];
afterEach(() => {
  for (const root of roots) rmSync(root, { recursive: true, force: true });
  roots = [];
});

function content(files: Record<string, string>): string {
  const root = mkdtempSync(join(tmpdir(), "pmos-ceiling-"));
  roots.push(root);
  for (const [rel, text] of Object.entries(files)) {
    mkdirSync(dirname(join(root, rel)), { recursive: true });
    writeFileSync(join(root, rel), text);
  }
  return root;
}

describe("the master-context ceiling", () => {
  it("reads the first-line marker, and counts an unmarked file as restricted", () => {
    expect(masterContextClass("<!-- sensitivity: public -->\n# Team")).toBe("public");
    expect(masterContextClass("<!-- sensitivity: internal -->\r\n# Team")).toBe("internal");
    expect(masterContextClass("# Team\n<!-- sensitivity: public -->")).toBe("restricted");
    expect(masterContextClass("<!-- sensitivity: restricted -->")).toBe("restricted");
    expect(masterContextClass("<!-- sensitivity: secret -->")).toBe("restricted");
    expect(masterContextClass("")).toBe("restricted");
  });

  it("needs nothing with no ceiling or the default `restricted`", () => {
    const root = content({ "master/AGENTS.md": "SECRET-TEAM-TEXT" });
    expect(() => checkMasterContext(root, undefined, undefined)).not.toThrow();
    expect(() => checkMasterContext(root, undefined, null)).not.toThrow();
    expect(() => checkMasterContext(root, undefined, "restricted")).not.toThrow();
  });

  it("refuses an unmarked file under a lower ceiling, naming the file and not its text", () => {
    const root = content({ "master/AGENTS.md": "SECRET-TEAM-TEXT" });
    let caught: unknown;
    try {
      checkMasterContext(root, undefined, "internal");
    } catch (e) {
      caught = e;
    }
    expect(caught).toBeInstanceOf(MasterContextAboveCeilingError);
    const err = caught as MasterContextAboveCeilingError;
    expect(err.file).toBe("content/master/AGENTS.md");
    expect(err.dataClass).toBe("restricted");
    expect(err.message).not.toContain("SECRET-TEAM-TEXT");
  });

  it("accepts a marked file at the ceiling, and checks the PM's own file too", () => {
    const root = content({
      "master/AGENTS.md": "<!-- sensitivity: internal -->\n# Team",
      "master/people/ana/AGENTS.md": "<!-- sensitivity: public -->\n# Ana",
      "master/people/ben/AGENTS.md": "# Ben, unmarked",
    });
    expect(() => checkMasterContext(root, "ana", "internal")).not.toThrow();
    expect(() => checkMasterContext(root, "ana", "public")).toThrow(MasterContextAboveCeilingError);
    expect(() => checkMasterContext(root, "ben", "internal")).toThrow(/people\/ben\/AGENTS\.md counts as restricted/);
  });

  it("takes the ceiling from the claim, an unknown value as public", () => {
    const base = {
      task: {
        id: "tsk_1",
        bot: "docs",
        requested_by: "ana",
        source: "slack",
        priority: "interactive",
        text: "hi",
        status: "working",
        checklist: [],
        approvals: [],
        attempts: 1,
      },
      bot: { name: "docs", role: "docs-helper", scope: "team", owner: "ana", description: "Docs.", reads: {}, actions: {} },
      role: {
        name: "docs-helper",
        description: "Docs.",
        skills: ["docs"],
        data_class: "public",
        model: "default",
        actions: {},
        instructions: "Cite.",
      },
      tools: [],
      session_token: "sess-1",
      gateway: { model: "pmos/default", headers: { "X-Pmos-Model-Source": "hosted:owner-hosted" } },
    };
    const parse = (extra: Record<string, unknown>) => ClaimSchema.parse({ ...base, ...extra });
    expect(parse({ max_data_class: "internal" }).max_data_class).toBe("internal");
    expect(parse({ max_data_class: "secret" }).max_data_class).toBe("public");
    expect(parse({}).max_data_class ?? null).toBeNull();
  });
});
