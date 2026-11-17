/**
 * A unified diff of two texts, three lines of context, in the shape `git diff` and the server use
 * (`--- a/path`, `+++ b/path`, `/dev/null` for a missing side). Used to show how a checked-in
 * policy file differs from what the content generates, and by the sample data.
 */
export function unifiedDiff(path: string, before: string | null, after: string | null): string {
  const a = before === null ? [] : before.replace(/\n$/, '').split('\n');
  const b = after === null ? [] : after.replace(/\n$/, '').split('\n');
  const n = a.length;
  const m = b.length;
  const t: number[][] = Array.from({ length: n + 1 }, () => new Array<number>(m + 1).fill(0));
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--) t[i]![j] = a[i] === b[j] ? t[i + 1]![j + 1]! + 1 : Math.max(t[i + 1]![j]!, t[i]![j + 1]!);
  type Op = { k: ' ' | '-' | '+'; s: string; ai: number; bi: number };
  const ops: Op[] = [];
  let i = 0;
  let j = 0;
  while (i < n || j < m) {
    if (i < n && j < m && a[i] === b[j]) ops.push({ k: ' ', s: a[i]!, ai: i++, bi: j++ });
    else if (j >= m || (i < n && t[i + 1]![j]! >= t[i]![j + 1]!)) ops.push({ k: '-', s: a[i]!, ai: i++, bi: j });
    else ops.push({ k: '+', s: b[j]!, ai: i, bi: j++ });
  }
  const out = [`--- ${before === null ? '/dev/null' : `a/${path}`}`, `+++ ${after === null ? '/dev/null' : `b/${path}`}`];
  const changed = ops.map((o, k) => (o.k !== ' ' ? k : -1)).filter((k) => k >= 0);
  let k = 0;
  while (k < changed.length) {
    const start = Math.max(0, changed[k]! - 3);
    let end = changed[k]!;
    while (k + 1 < changed.length && changed[k + 1]! - end <= 7) end = changed[++k]!;
    end = Math.min(ops.length - 1, end + 3);
    const hunk = ops.slice(start, end + 1);
    const aLen = hunk.filter((o) => o.k !== '+').length;
    const bLen = hunk.filter((o) => o.k !== '-').length;
    const aStart = aLen ? hunk.find((o) => o.k !== '+')!.ai + 1 : hunk[0]!.ai;
    const bStart = bLen ? hunk.find((o) => o.k !== '-')!.bi + 1 : hunk[0]!.bi;
    out.push(`@@ -${aStart},${aLen} +${bStart},${bLen} @@`);
    for (const o of hunk) out.push(`${o.k}${o.s}`);
    k++;
  }
  return out.join('\n') + '\n';
}
