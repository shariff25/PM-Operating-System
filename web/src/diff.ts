/** A run of words inside a changed line; `changed` runs get an inline highlight. */
export interface DiffPart {
  text: string;
  changed: boolean;
}

export interface DiffLine {
  kind: 'same' | 'added' | 'removed';
  text: string;
  /** Present on a removed or added line that pairs with a line on the other side: the words that differ. */
  parts?: DiffPart[];
  /** On a removed line: it was paired with an added line and is shown once, inline, on that line. */
  merged?: boolean;
  /** On an added line paired with a removed one: the old and new words in one run of text. */
  inline?: InlinePart[];
  /** On an unchanged line: only its list marker differs ("1." became "-"). Shown as the new line, not as a change. */
  markerOnly?: boolean;
}

/** A run of words in a paragraph shown once: kept as is, struck through, or newly added. */
export interface InlinePart {
  text: string;
  kind: 'same' | 'removed' | 'added';
}

/** A paired paragraph is shown inline only when at least this share of its words is kept. */
export const INLINE_MIN_SHARED = 0.4;

const MARKER = /^(\s*)((?:[-*+]|\d+[.)])\s+)/;

/** A list item's marker, apart from its words. Lines that are not list items have no marker. */
function splitMarker(line: string): { marker: string; rest: string } {
  const m = MARKER.exec(line);
  return m ? { marker: m[0], rest: line.slice(m[0].length) } : { marker: '', rest: line };
}

/** The line with its list marker (but not its indent) taken out, so two markers compare equal. */
const markerKey = (line: string) => line.replace(MARKER, '$1');

type Op = { kind: 'same' | 'added' | 'removed'; a?: number; b?: number };

/** Sequence diff by longest common subsequence. Drafts are short, so O(n*m) is fine. */
function lcsOps(a: readonly string[], b: readonly string[]): Op[] {
  const n = a.length;
  const m = b.length;
  const table: number[][] = Array.from({ length: n + 1 }, () => new Array<number>(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      table[i]![j] = a[i] === b[j] ? table[i + 1]![j + 1]! + 1 : Math.max(table[i + 1]![j]!, table[i]![j + 1]!);
    }
  }
  const out: Op[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (a[i] === b[j]) {
      out.push({ kind: 'same', a: i, b: j });
      i++;
      j++;
    } else if (table[i + 1]![j]! >= table[i]![j + 1]!) {
      out.push({ kind: 'removed', a: i++ });
    } else {
      out.push({ kind: 'added', b: j++ });
    }
  }
  while (i < n) out.push({ kind: 'removed', a: i++ });
  while (j < m) out.push({ kind: 'added', b: j++ });
  return out;
}

const tokens = (line: string) => line.match(/\s+|[^\s]+/g) ?? [];

/** Merge neighbouring parts that share a state, so a changed phrase is one highlight. */
function merge(parts: DiffPart[]): DiffPart[] {
  const out: DiffPart[] = [];
  for (const p of parts) {
    const last = out[out.length - 1];
    if (last && last.changed === p.changed) last.text += p.text;
    else out.push({ ...p });
  }
  return out;
}

/** Which words changed between two lines: one parts list for the old line, one for the new. */
export function diffWords(before: string, after: string): { before: DiffPart[]; after: DiffPart[]; shared: number } {
  const a = tokens(before);
  const b = tokens(after);
  const ops = lcsOps(a, b);
  const left: DiffPart[] = [];
  const right: DiffPart[] = [];
  let shared = 0;
  for (const op of ops) {
    if (op.kind === 'same') {
      const t = a[op.a!]!;
      left.push({ text: t, changed: false });
      right.push({ text: t, changed: false });
      if (t.trim()) shared++;
    } else if (op.kind === 'removed') {
      left.push({ text: a[op.a!]!, changed: true });
    } else {
      right.push({ text: b[op.b!]!, changed: true });
    }
  }
  // Whitespace between two changed words belongs to the highlight, so the phrase reads as one.
  const bridge = (parts: DiffPart[]) =>
    parts.map((p, i) => {
      const prev = parts[i - 1];
      const next = parts[i + 1];
      return !p.changed && !p.text.trim() && prev?.changed && next?.changed ? { ...p, changed: true } : p;
    });
  return { before: merge(bridge(left)), after: merge(bridge(right)), shared };
}

/** Old and new words in one sequence: removed words come right before the words that replace them. */
export function diffInline(before: string, after: string): { parts: InlinePart[]; shared: number; most: number } {
  const a = tokens(before);
  const b = tokens(after);
  const parts: InlinePart[] = [];
  let shared = 0;
  let gone = '';
  let came = '';
  const flush = () => {
    // Spacing both sides share is not part of the change.
    const tail = /\s*$/.exec(gone)![0];
    const tailB = /\s*$/.exec(came)![0];
    const keep = tail && tail === tailB && (gone.trim() || came.trim()) ? tail : '';
    if (keep) {
      gone = gone.slice(0, -keep.length);
      came = came.slice(0, -keep.length);
    }
    if (gone.trim()) parts.push({ text: gone, kind: 'removed' });
    if (came.trim()) parts.push({ text: came, kind: 'added' });
    if (keep) parts.push({ text: keep, kind: 'same' });
    gone = '';
    came = '';
  };
  for (const op of lcsOps(a, b)) {
    if (op.kind === 'removed') gone += a[op.a!]!;
    else if (op.kind === 'added') came += b[op.b!]!;
    else {
      const t = a[op.a!]!;
      // Whitespace between two changed runs stays inside the change, so a phrase reads as one.
      if (!t.trim() && (gone || came)) {
        gone += t;
        came += t;
        continue;
      }
      flush();
      const last = parts[parts.length - 1];
      if (last?.kind === 'same') last.text += t;
      else parts.push({ text: t, kind: 'same' });
      if (t.trim()) shared++;
    }
  }
  flush();
  const count = (t: string[]) => t.filter((x) => x.trim()).length;
  return { parts, shared, most: Math.max(count(a), count(b)) };
}

/**
 * Line diff, with word-level differences inside lines that were rewritten rather than
 * replaced outright: in each run of removed lines followed by added lines, a removed line
 * and an added line at the same place that still share most of their words are paired.
 */
export function diffLines(before: string, after: string): DiffLine[] {
  const a = before.split('\n');
  const b = after.split('\n');
  // Lines are matched without their list markers, so a changed marker alone is not a change.
  const lines: DiffLine[] = lcsOps(a.map(markerKey), b.map(markerKey)).map((op) =>
    op.kind === 'same'
      ? a[op.a!] === b[op.b!]
        ? { kind: 'same', text: b[op.b!]! }
        : { kind: 'same', text: b[op.b!]!, markerOnly: true }
      : op.kind === 'removed'
        ? { kind: 'removed', text: a[op.a!]! }
        : { kind: 'added', text: b[op.b!]! },
  );

  // Pair lines inside each changed run.
  let i = 0;
  while (i < lines.length) {
    if (lines[i]!.kind === 'same') {
      i++;
      continue;
    }
    let j = i;
    while (j < lines.length && lines[j]!.kind !== 'same') j++;
    const run = lines.slice(i, j);
    const removed = run.filter((l) => l.kind === 'removed');
    const added = run.filter((l) => l.kind === 'added');
    const pairs = Math.min(removed.length, added.length);
    for (let k = 0; k < pairs; k++) {
      const r = removed[k]!;
      const ad = added[k]!;
      // Compare the words only; a list marker that changed is carried over unchanged from the new line.
      const old = splitMarker(r.text);
      const now = splitMarker(ad.text);
      const words = diffInline(old.rest, now.rest);
      if (words.most > 0 && words.shared / words.most >= INLINE_MIN_SHARED) {
        const sides = diffWords(old.rest, now.rest);
        const lead = now.marker ? [{ text: now.marker, changed: false }] : [];
        r.parts = [...(old.marker ? [{ text: old.marker, changed: false }] : []), ...sides.before];
        ad.parts = [...lead, ...sides.after];
        r.merged = true;
        ad.inline = [...(now.marker ? [{ text: now.marker, kind: 'same' as const }] : []), ...words.parts];
      }
    }
    // Show each run as the old lines, then the new lines.
    lines.splice(i, run.length, ...removed, ...added);
    i += run.length;
  }
  return lines;
}

/** A paragraph of the diff: the lines between blank lines. */
export interface DiffBlock {
  lines: DiffLine[];
  changed: boolean;
  /** Nothing in the paragraph changed except its list markers. */
  styleOnly: boolean;
}

/**
 * Group a line diff into paragraphs. A blank line ends a paragraph; a blank line that was
 * added or removed on its own is just spacing and is dropped from view.
 */
export function diffBlocks(diff: DiffLine[]): DiffBlock[] {
  const blocks: DiffBlock[] = [];
  let current: DiffLine[] = [];
  const close = () => {
    if (current.length > 0) {
      const changed = current.some((l) => l.kind !== 'same');
      blocks.push({ lines: current, changed, styleOnly: !changed && current.some((l) => l.markerOnly) });
    }
    current = [];
  };
  for (const l of diff) {
    if (!l.text.trim()) {
      close();
      continue;
    }
    current.push(l);
  }
  close();
  return blocks;
}
