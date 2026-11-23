import { describe, expect, it } from 'vitest';
import { ApiError } from '../api';
import type { AllowanceChange, PolicySpec, Validation } from '../api';
import { copy } from '../copy';
import { unifiedDiff } from './unified';
import { handEdits } from './policyView';
import { createMockPolicy } from '../api/mockPolicy';
import {
  applySpecPatch,
  canPropose,
  checkIsStale,
  diffStat,
  groupChanges,
  newPathProblem,
  parseRawPatch,
  parseUnified,
  patchSize,
  problemOf,
  specPatch,
  stableKey,
  toChanges,
  upsertDraft,
  wideningsOf,
} from './proposals';
import type { ContentDraft } from './proposals';

const P = copy.admin.propose.problems;

const SPEC: PolicySpec = {
  bots: { docs: { role: 'docs-helper', scope: 'team', owner: 'TODO' } },
  roles: { 'docs-helper': { skills: ['docs'] } },
  skills: { docs: { tools: ['docs.search', 'docs.read'] } },
};

const clean = (over: Partial<Validation> = {}): Validation => ({
  valid: true, errors: [], warnings: [], content_changes: [], policy_changes: [], allowance_changes: [], widening: false,
  summary: { wider: 0, narrower: 0, changed: 0 }, ...over,
});

describe('propose only after a clean check of exactly this change', () => {
  const key = stableKey({ skills: { docs: { tools: ['docs.search'] } } });

  it('allows a proposal for the change that passed', () => {
    expect(canPropose({ key, result: clean() }, key, false)).toBe(true);
  });

  it('refuses before any check, after a failed check, and for an empty change', () => {
    expect(canPropose(null, key, false)).toBe(false);
    expect(canPropose({ key, result: clean({ valid: false }) }, key, false)).toBe(false);
    expect(canPropose({ key, result: clean() }, key, true)).toBe(false);
  });

  it('refuses once the change is edited after the check, and says the check is stale', () => {
    const edited = stableKey({ skills: { docs: { tools: ['docs.search', 'jira.search'] } } });
    expect(canPropose({ key, result: clean() }, edited, false)).toBe(false);
    expect(checkIsStale({ key, result: clean() }, edited)).toBe(true);
    expect(checkIsStale({ key, result: clean() }, key)).toBe(false);
  });

  it('gives the same key whatever order the keys were written in, but not for a reordered list', () => {
    expect(stableKey({ a: 1, b: { c: [1, 2], d: 2 } })).toBe(stableKey({ b: { d: 2, c: [1, 2] }, a: 1 }));
    expect(stableKey({ x: ['a', 'b'] })).not.toBe(stableKey({ x: ['b', 'a'] }));
  });
});

describe('the spec patch', () => {
  it('holds only what differs, and applying it gives the edited spec back', () => {
    const edited = applySpecPatch(SPEC, {});
    edited.bots.docs!.owner = 'ana';
    edited.skills.docs!.tools = ['docs.search', 'docs.read', 'jira.search'];
    const patch = specPatch(SPEC, edited);
    expect(patch).toEqual({ bots: { docs: { owner: 'ana' } }, skills: { docs: { tools: ['docs.search', 'docs.read', 'jira.search'] } } });
    expect(patchSize(patch)).toBe(2);
    expect(applySpecPatch(SPEC, patch)).toEqual(edited);
    expect(specPatch(SPEC, SPEC)).toEqual({});
  });

  it('reads the raw JSON view, and names an unknown field before the server refuses it', () => {
    expect(parseRawPatch('{"bots": {"docs": {"owner": "ana"}}}')).toEqual({ patch: { bots: { docs: { owner: 'ana' } } } });
    expect(parseRawPatch('{"bots": {"docs": {"hosts": []}}}')).toEqual({ error: copy.admin.policy.editor.rawUnknown('bots.docs.hosts') });
    expect(parseRawPatch('{"skills": {"docs": {"tools": "docs.read"}}}')).toEqual({ error: copy.admin.policy.editor.rawType('skills.docs.tools', true) });
    expect(parseRawPatch('{oops')).toEqual({ error: copy.admin.policy.editor.rawBad });
    expect(parseRawPatch('[]')).toEqual({ error: copy.admin.policy.editor.rawNotObject });
  });
});

describe('allowance changes', () => {
  const c = (direction: AllowanceChange['direction'], item: string): AllowanceChange => ({
    bot: 'docs', kind: 'network.mcp_tool', item, before: null, after: item, direction, widening: direction !== 'narrower', description: `${direction} ${item}`,
  });

  it('groups them wider, narrower and changed, and counts a changed setting as a widening', () => {
    const all = [c('narrower', 'a'), c('wider', 'b'), c('changed', 'c'), c('wider', 'd')];
    const g = groupChanges(all);
    expect(g.wider.map((x) => x.item)).toEqual(['b', 'd']);
    expect(g.narrower.map((x) => x.item)).toEqual(['a']);
    expect(g.changed.map((x) => x.item)).toEqual(['c']);
    expect(wideningsOf(all).map((x) => x.item)).toEqual(['b', 'c', 'd']);
  });

  it('the sample data really marks a new tool as a widening', () => {
    const mock = createMockPolicy(Date.now(), {});
    const v = mock.validate({ skills: { docs: { tools: ['docs.search', 'docs.read', 'jira.search'] } } });
    expect(v.valid).toBe(true);
    expect(v.widening).toBe(true);
    expect(v.allowance_changes.some((x) => x.item === 'jira_search' && x.direction === 'wider' && x.bot === 'docs')).toBe(true);
    expect(v.policy_changes.map((f) => f.path)).toContain('policies/bots/docs.yaml');
  });
});

describe('hand edits in a checked-in policy', () => {
  it('names an unknown destination, extra IP ranges, logging only and TLS passed through', () => {
    const mock = createMockPolicy(Date.now(), {});
    const ben = mock.policy(true).bots.find((b) => b.bot === 'pm-ben')!;
    const flags = handEdits(ben.allowances!);
    expect(flags.join('\n')).toContain('not a PM OS service');
    expect(flags.join('\n')).toContain('203.0.113.0/24');
    expect(flags.some((f) => f.includes(copy.admin.policy.bots.logsOnly))).toBe(true);
    const docs = mock.policy(true).bots.find((b) => b.bot === 'docs')!;
    expect(handEdits(docs.allowances!)).toEqual([]);
  });

  it('takes each service pinned to its own address as generated, and flags one that is not', () => {
    const mock = createMockPolicy(Date.now(), {});
    const docs = mock.policy(true).bots.find((b) => b.bot === 'docs')!;
    const a = docs.allowances!;
    expect(a.network.every((n) => n.pinned && n.allowed_ips.length === 1)).toBe(true);
    expect(handEdits(a)).toEqual([]);
    const B = copy.admin.policy.bots;
    const unpinned = structuredClone(a);
    const rt = unpinned.network.find((n) => n.service === 'runtime')!;
    rt.allowed_ips = [];
    rt.pinned = false;
    expect(handEdits(unpinned)).toEqual([`bots.pmos.internal:8750: ${B.notPinned('')}`]);
    const swapped = structuredClone(a);
    const gw = swapped.network.find((n) => n.service === 'gateway')!;
    gw.allowed_ips = ['10.201.77.12/32'];
    gw.pinned = false;
    expect(handEdits(swapped).join('\n')).toContain('not pinned to its own address (allows 10.201.77.12/32)');
  });
});

describe('unified diffs', () => {
  it('parses hunks with line numbers and counts the lines folded away between them', () => {
    const a = Array.from({ length: 30 }, (_, i) => `line ${i + 1}`).join('\n');
    const b = a.replace('line 5', 'line five').replace('line 25', 'line twenty-five');
    const hunks = parseUnified(unifiedDiff('x.md', a, b));
    expect(hunks).toHaveLength(2);
    expect(hunks[0]!.skipped).toBe(1);
    expect(hunks[0]!.lines.find((l) => l.kind === 'del')).toEqual({ kind: 'del', text: 'line 5', old: 5, new: null });
    expect(hunks[0]!.lines.find((l) => l.kind === 'add')).toEqual({ kind: 'add', text: 'line five', old: null, new: 5 });
    // Lines 9 to 21 are unchanged and not in either hunk.
    expect(hunks[1]!.skipped).toBe(13);
    expect(diffStat(unifiedDiff('x.md', a, b))).toEqual({ added: 2, removed: 2 });
  });

  it('reads an added file', () => {
    const hunks = parseUnified(unifiedDiff('new.md', null, 'a\nb\n'));
    expect(hunks[0]!.lines.map((l) => [l.kind, l.new])).toEqual([['add', 1], ['add', 2]]);
    expect(hunks[0]!.skipped).toBe(0);
  });
});

describe('every documented error code reads as its own sentence and next step', () => {
  const codes: [number, string][] = [
    [409, 'github_not_linked'], [409, 'stale_base'], [422, 'policy_mismatch'], [429, 'rate_limited'], [501, 'not_configured'],
    [502, 'unknown_outcome'], [502, 'audit_unavailable'], [502, 'upstream_unavailable'], [422, 'invalid'], [502, 'github_refused'],
    [502, 'github_unavailable'], [503, 'github_rate_limited'], [400, 'no_change'], [400, 'bad_request'], [403, 'forbidden'],
    [400, 'bad_path'], [400, 'path_not_allowed'], [400, 'not_text'], [400, 'not_found'], [413, 'too_large'], [413, 'too_many_files'],
  ];

  it.each(codes)('%i %s', (status, code) => {
    const p = problemOf(new ApiError(status, code, 'the server sentence'), 'fallback');
    expect(p.text).toBe(P[code as keyof typeof P].text);
    expect(p.text).not.toBe('the server sentence');
    expect(p.text).not.toContain('—');
  });

  it('never says nothing changed when the outcome is not known, and sends the admin to the proposals', () => {
    for (const code of ['unknown_outcome', 'upstream_unavailable']) {
      const p = problemOf(new ApiError(502, code, ''), 'x');
      expect(p.text).not.toMatch(/nothing changed/i);
      expect(p.checkProposals).toBe(true);
    }
    expect(problemOf(new ApiError(409, 'stale_base', ''), 'x').checkProposals).toBe(false);
  });

  it('carries the paths and base of stale_base, the wait of rate_limited, and the issues of invalid', () => {
    const stale = problemOf(new ApiError(409, 'stale_base', '', { paths: ['content/a.md'], base: { branch: 'main', commit: 'abc1234def' } }), 'x');
    expect(stale.paths).toEqual(['content/a.md']);
    expect(stale.base).toEqual({ branch: 'main', commit: 'abc1234def' });
    expect(problemOf(new ApiError(429, 'rate_limited', '', {}, 1800), 'x').next).toBe(copy.admin.propose.waitFor(30));
    expect(problemOf(new ApiError(429, 'rate_limited', ''), 'x').next).toBe(copy.admin.propose.waitLater);
    const issue = { severity: 'error' as const, code: 'net-tools', path: 'policies/bots/docs.yaml', line: null, message: 'no' };
    expect(problemOf(new ApiError(422, 'invalid', '', { errors: [issue], warnings: [] }), 'x').issues).toEqual([issue]);
  });

  it('treats github_not_linked as a call to action, not a failure', () => {
    expect(problemOf(new ApiError(409, 'github_not_linked', ''), 'x').callToAction).toBe(true);
  });
});

describe('the content tray', () => {
  const edit = (path: string, text: string, original = 'old'): ContentDraft => ({ path, kind: 'edit', text, original, base_sha256: 'sha' });

  it('collects several files, replaces an earlier draft of the same file, and drops an edit back to the original', () => {
    let d: ContentDraft[] = [];
    d = upsertDraft(d, edit('content/a.md', 'new a'));
    d = upsertDraft(d, { path: 'content/b.md', kind: 'add', text: 'b' });
    d = upsertDraft(d, { path: 'content/c.md', kind: 'delete' });
    d = upsertDraft(d, edit('content/a.md', 'newer a'));
    expect(d.map((x) => x.path)).toEqual(['content/a.md', 'content/b.md', 'content/c.md']);
    expect(toChanges(d)).toEqual([
      { path: 'content/a.md', text: 'newer a', base_sha256: 'sha' },
      { path: 'content/b.md', text: 'b' },
      { path: 'content/c.md', delete: true },
    ]);
    d = upsertDraft(d, edit('content/a.md', 'old'));
    expect(d.map((x) => x.path)).toEqual(['content/b.md', 'content/c.md']);
  });

  it('checks a new path the way the server does', () => {
    const have = new Set(['content/playbooks/a.md']);
    const A = copy.admin.content.add;
    expect(newPathProblem('content/playbooks/b.md', have)).toBeNull();
    expect(newPathProblem('content/playbooks/a.md', have)).toBe(A.exists);
    expect(newPathProblem('policies/x.md', have)).toBe(A.outside);
    expect(newPathProblem('content/team.yaml', have)).toBe(A.outside);
    expect(newPathProblem('content/../x.md', have)).toBe(A.badPath);
    expect(newPathProblem('content/.hidden.md', have)).toBe(A.badPath);
    expect(newPathProblem('content/a.png', have)).toBe(A.notText);
    expect(newPathProblem('', have)).toBe(A.needPath);
  });
});
