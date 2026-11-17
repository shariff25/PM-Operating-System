import { ApiError, isServerErrorCode } from '../api';
import type { AllowanceChange, ContentChange, Issue, PolicySpec, SpecBot, SpecPatch, Validation } from '../api';
import { copy } from '../copy';
import { errorText } from '../errors';

// ---------- "Propose only after a clean check of exactly this change" ----------

/** JSON with object keys sorted, so two equal changes give the same key whatever order they were built in. */
export function stableKey(value: unknown): string {
  const norm = (v: unknown): unknown => {
    if (Array.isArray(v)) return v.map(norm);
    if (v && typeof v === 'object') {
      return Object.fromEntries(Object.keys(v).sort().map((k) => [k, norm((v as Record<string, unknown>)[k])]));
    }
    return v;
  };
  return JSON.stringify(norm(value));
}

export interface Checked {
  /** The key of the change this result is for. */
  key: string;
  result: Validation;
}

/**
 * May this change be proposed? Only when it was checked, the check was clean, and the change on
 * screen is exactly the one that was checked. Anything edited after the check needs a new one.
 */
export function canPropose(checked: Checked | null, currentKey: string, empty: boolean): boolean {
  return !empty && checked !== null && checked.key === currentKey && checked.result.valid;
}

/** The check on screen is for an older version of the change. */
export const checkIsStale = (checked: Checked | null, currentKey: string) => checked !== null && checked.key !== currentKey;

// ---------- The policy spec ----------

const sameList = (a: readonly string[], b: readonly string[]) => a.length === b.length && a.every((x, i) => x === b[i]);

/** The smallest patch that turns `base` into `edited`: only fields that differ. */
export function specPatch(base: PolicySpec, edited: PolicySpec): SpecPatch {
  const patch: SpecPatch = {};
  for (const [bot, b] of Object.entries(edited.bots)) {
    const was = base.bots[bot];
    const diff: Partial<SpecBot> = {};
    for (const f of ['role', 'scope', 'owner'] as const) if (!was || was[f] !== b[f]) diff[f] = b[f];
    if (Object.keys(diff).length) (patch.bots ??= {})[bot] = diff;
  }
  for (const [role, r] of Object.entries(edited.roles)) {
    if (!base.roles[role] || !sameList(base.roles[role].skills, r.skills)) (patch.roles ??= {})[role] = { skills: [...r.skills] };
  }
  for (const [skill, s] of Object.entries(edited.skills)) {
    if (!base.skills[skill] || !sameList(base.skills[skill].tools, s.tools)) (patch.skills ??= {})[skill] = { tools: [...s.tools] };
  }
  return patch;
}

/** The spec with a patch applied. A patch naming something the spec does not have adds it as given. */
export function applySpecPatch(base: PolicySpec, patch: SpecPatch): PolicySpec {
  const out: PolicySpec = structuredClone(base);
  for (const [bot, p] of Object.entries(patch.bots ?? {})) out.bots[bot] = { role: '', scope: '', owner: '', ...out.bots[bot], ...p };
  for (const [role, p] of Object.entries(patch.roles ?? {})) if (p.skills) out.roles[role] = { skills: [...p.skills] };
  for (const [skill, p] of Object.entries(patch.skills ?? {})) if (p.tools) out.skills[skill] = { tools: [...p.tools] };
  return out;
}

/** How many fields a patch changes. */
export function patchSize(patch: SpecPatch): number {
  let n = 0;
  for (const b of Object.values(patch.bots ?? {})) n += Object.keys(b).length;
  n += Object.keys(patch.roles ?? {}).length;
  n += Object.keys(patch.skills ?? {}).length;
  return n;
}

/** Tools typed as words: split on spaces and commas, blanks dropped, order kept. */
export const splitTools = (text: string) => text.split(/[\s,]+/).filter(Boolean);

/**
 * The raw JSON view: a patch or a sentence saying why not. Only the fields the server knows are
 * let through here; anything else would be refused with 400, so it is named before a check.
 */
export function parseRawPatch(text: string): { patch: SpecPatch } | { error: string } {
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch {
    return { error: copy.admin.policy.editor.rawBad };
  }
  if (!v || typeof v !== 'object' || Array.isArray(v)) return { error: copy.admin.policy.editor.rawNotObject };
  const fields: Record<string, readonly string[]> = { bots: ['role', 'scope', 'owner'], roles: ['skills'], skills: ['tools'] };
  for (const [part, items] of Object.entries(v)) {
    const allowed = fields[part];
    if (!allowed || !items || typeof items !== 'object' || Array.isArray(items)) return { error: copy.admin.policy.editor.rawUnknown(part) };
    for (const [name, item] of Object.entries(items as Record<string, unknown>)) {
      if (!item || typeof item !== 'object' || Array.isArray(item)) return { error: copy.admin.policy.editor.rawUnknown(`${part}.${name}`) };
      for (const [f, val] of Object.entries(item as Record<string, unknown>)) {
        if (!allowed.includes(f)) return { error: copy.admin.policy.editor.rawUnknown(`${part}.${name}.${f}`) };
        const list = f === 'skills' || f === 'tools';
        if (list ? !Array.isArray(val) || val.some((x) => typeof x !== 'string') : typeof val !== 'string') {
          return { error: copy.admin.policy.editor.rawType(`${part}.${name}.${f}`, list) };
        }
      }
    }
  }
  return { patch: v as SpecPatch };
}

// ---------- Allowance changes ----------

/** Wider first (each a widening a reviewer must see), then narrower, then changed. */
export function groupChanges(changes: readonly AllowanceChange[]): { wider: AllowanceChange[]; narrower: AllowanceChange[]; changed: AllowanceChange[] } {
  return {
    wider: changes.filter((c) => c.direction === 'wider'),
    narrower: changes.filter((c) => c.direction === 'narrower'),
    changed: changes.filter((c) => c.direction === 'changed'),
  };
}

/** Every change a reviewer must look at: the server marks `changed` as a widening too. */
export const wideningsOf = (changes: readonly AllowanceChange[]) => changes.filter((c) => c.widening);

// ---------- Unified diffs ----------

export interface UnifiedLine {
  kind: 'ctx' | 'add' | 'del';
  text: string;
  old: number | null;
  new: number | null;
}

export interface UnifiedHunk {
  /** Unchanged lines left out before this hunk (0 when it follows straight on). */
  skipped: number;
  lines: UnifiedLine[];
}

/** Parse a unified diff (`--- a/x`, `+++ b/x`, `@@ -a,b +c,d @@`) into hunks with line numbers. */
export function parseUnified(diff: string): UnifiedHunk[] {
  const hunks: UnifiedHunk[] = [];
  let oldNo = 0;
  let newNo = 0;
  let lastOld = 0;
  let hunk: UnifiedHunk | null = null;
  for (const line of diff.split('\n')) {
    const head = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(line);
    if (head) {
      oldNo = Number(head[1]);
      newNo = Number(head[3]);
      // An empty side starts at 0 ("-0,0"); the first real line is then line 1.
      const firstOld = head[2] === '0' ? oldNo + 1 : oldNo;
      hunk = { skipped: Math.max(0, firstOld - lastOld - 1), lines: [] };
      if (head[2] === '0') oldNo += 1;
      if (head[4] === '0') newNo += 1;
      hunks.push(hunk);
      continue;
    }
    if (!hunk || line.startsWith('--- ') || line.startsWith('+++ ') || line.startsWith('\\')) continue;
    const mark = line[0];
    const text = line.slice(1);
    if (mark === '+') hunk.lines.push({ kind: 'add', text, old: null, new: newNo++ });
    else if (mark === '-') {
      hunk.lines.push({ kind: 'del', text, old: oldNo, new: null });
      lastOld = oldNo++;
    } else if (mark === ' ') {
      hunk.lines.push({ kind: 'ctx', text, old: oldNo, new: newNo++ });
      lastOld = oldNo++;
    }
  }
  return hunks;
}

/** Lines added and removed, for a file's header. */
export function diffStat(diff: string): { added: number; removed: number } {
  let added = 0;
  let removed = 0;
  for (const h of parseUnified(diff)) for (const l of h.lines) {
    if (l.kind === 'add') added++;
    else if (l.kind === 'del') removed++;
  }
  return { added, removed };
}

// ---------- Errors from checking and proposing ----------

const P = copy.admin.propose.problems;
type ProblemCode = keyof typeof P;

export interface Problem {
  code: string;
  text: string;
  next: string | null;
  /** The files a `stale_base` or `policy_mismatch` names. */
  paths: string[];
  base: { branch: string; commit: string } | null;
  /** Errors and warnings an `invalid` refusal carries. */
  issues: Issue[];
  /** The outcome is not known: send the admin to the open proposals before they try again. */
  checkProposals: boolean;
  /** A call to action rather than a failure (GitHub not linked). */
  callToAction: boolean;
}

const strings = (v: unknown) => (Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : []);

/** Minutes to wait, rounded up, from `Retry-After` seconds. */
export const waitMinutes = (seconds: number) => Math.max(1, Math.ceil(seconds / 60));

/** The plain sentence and next step for an error from a validate or proposal route. */
export function problemOf(e: unknown, fallback: string): Problem {
  const code = e instanceof ApiError ? e.code : 'unknown';
  const known = Object.hasOwn(P, code) ? P[code as ProblemCode] : null;
  const details = e instanceof ApiError ? e.details : {};
  const baseRaw = details.base as { branch?: unknown; commit?: unknown } | undefined;
  const issues = [...((details.errors as Issue[] | undefined) ?? []), ...((details.warnings as Issue[] | undefined) ?? [])];
  let next: string | null = known ? known.next : null;
  if (code === 'rate_limited' && e instanceof ApiError) next = e.retryAfter !== null ? copy.admin.propose.waitFor(waitMinutes(e.retryAfter)) : copy.admin.propose.waitLater;
  return {
    code,
    text: known ? known.text : isServerErrorCode(code) ? copy.errors.codes[code] : errorText(e, fallback),
    next,
    paths: strings(details.paths),
    base: baseRaw && typeof baseRaw.branch === 'string' && typeof baseRaw.commit === 'string' ? { branch: baseRaw.branch, commit: baseRaw.commit } : null,
    issues,
    checkProposals: code === 'unknown_outcome' || code === 'upstream_unavailable' || (e instanceof ApiError && e.status === 0),
    callToAction: code === 'github_not_linked',
  };
}

// ---------- Title and reason ----------

export const TITLE_MAX = 120;
export const REASON_MAX = 4000;

/** As the server cleans a title: control characters out, whitespace collapsed. */
// eslint-disable-next-line no-control-regex
export const cleanTitle = (t: string) => t.replace(/[\u0000-\u001f\u007f]/g, ' ').replace(/\s+/g, ' ').trim();

// ---------- The content tray ----------

/** One file in the tray: edited, added, or to be deleted. */
export type ContentDraft =
  | { path: string; kind: 'edit'; text: string; original: string; base_sha256: string }
  | { path: string; kind: 'add'; text: string }
  | { path: string; kind: 'delete' };

export const MAX_FILES = 50;

/** The changes to send: an edit equal to the deployed text is left out, as the server would drop it. */
export function toChanges(drafts: readonly ContentDraft[]): ContentChange[] {
  const out: ContentChange[] = [];
  for (const d of drafts) {
    if (d.kind === 'delete') out.push({ path: d.path, delete: true });
    else if (d.kind === 'add') out.push({ path: d.path, text: d.text });
    else if (d.text !== d.original) out.push({ path: d.path, text: d.text, base_sha256: d.base_sha256 });
  }
  return out;
}

/** Put a draft in the tray, replacing any earlier one for the same path. An edit back to the original leaves the tray. */
export function upsertDraft(drafts: readonly ContentDraft[], next: ContentDraft): ContentDraft[] {
  const rest = drafts.filter((d) => d.path !== next.path);
  if (next.kind === 'edit' && next.text === next.original) return rest;
  const at = drafts.findIndex((d) => d.path === next.path);
  if (at < 0) return [...rest, next];
  const out = [...drafts];
  out[at] = next;
  return out;
}

export const removeDraft = (drafts: readonly ContentDraft[], path: string) => drafts.filter((d) => d.path !== path);

const PART = /^[A-Za-z0-9._-]+$/;

/** Why a new file's path would be refused, or null. Mirrors the server's path rule (web-api.md, Paths). */
export function newPathProblem(path: string, existing: ReadonlySet<string>): string | null {
  const t = copy.admin.content.add;
  const parts = path.split('/');
  if (!path) return t.needPath;
  if (path.length > 200 || parts.length > 8 || parts.some((p) => !p || p === '.' || p === '..' || p.startsWith('.') || !PART.test(p))) return t.badPath;
  if (parts[0] !== 'content' || parts.length < 2 || path === 'content/team.yaml') return t.outside;
  if (!/\.(md|ya?ml|txt)$/.test(path)) return t.notText;
  if (existing.has(path)) return t.exists;
  return null;
}
