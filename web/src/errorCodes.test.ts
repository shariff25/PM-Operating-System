import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { SERVER_ERROR_CODES } from './api/types';
import { copy } from './copy';

// The contract is docs/web-api.md. Its "Error codes" table is read here, so the client's list of
// codes and the sentences in copy.ts cannot drift away from it without a test failing.
const doc = readFileSync(resolve(__dirname, '../../docs/web-api.md'), 'utf8');

type Row = { status: string; code: string; nothing: 'yes' | 'decision' | 'unknown' };

function tableRows(): Row[] {
  const section = doc.split(/^## Error codes$/m)[1]!.split(/^## /m)[0]!;
  const rows: Row[] = [];
  for (const line of section.split('\n')) {
    if (!line.startsWith('|')) continue;
    const cells = line.split(/(?<!\\)\|/).slice(1, -1).map((c) => c.trim());
    if (cells.length < 4) continue;
    // one code, or several in one cell: "`github_refused`, `github_unavailable`"
    const codes = /^`[a-z_]+`(, `[a-z_]+`)*$/.test(cells[1]!) ? [...cells[1]!.matchAll(/`([a-z_]+)`/g)].map((m) => m[1]!) : [];
    if (codes.length === 0 || codes[0] === 'code') continue; // the header, the divider, and the row with no body (413)
    const col = cells[3]!;
    const code = codes[0]!;
    const nothing = /^\*\*Unknown\*\*/i.test(col) ? 'unknown' : /^Yes on a decision/i.test(col) ? 'decision' : /^Yes\b/i.test(col) ? 'yes' : null;
    expect(nothing, `the "Nothing changed?" cell of ${code}: ${col}`).not.toBeNull();
    for (const c of codes) rows.push({ status: cells[0]!, code: c, nothing: nothing! });
  }
  return rows;
}

describe('the client\'s error codes match the table in docs/web-api.md', () => {
  const rows = tableRows();
  const docCodes = [...new Set(rows.map((r) => r.code))].sort();

  it('finds the table', () => {
    expect(docCodes.length).toBeGreaterThan(20);
    expect(docCodes).toContain('unknown_outcome');
  });

  it('has exactly the documented codes: none missing, none made up', () => {
    expect(Object.keys(SERVER_ERROR_CODES).sort()).toEqual(docCodes);
  });

  it('agrees with the "Nothing changed?" column for every code', () => {
    for (const r of rows) expect(SERVER_ERROR_CODES[r.code as keyof typeof SERVER_ERROR_CODES], r.code).toBe(r.nothing);
  });

  it('has a plain sentence in copy.ts for each, and no more', () => {
    expect(Object.keys(copy.errors.codes).sort()).toEqual(docCodes);
    for (const code of docCodes) expect(copy.errors.codes[code as keyof typeof copy.errors.codes].length, code).toBeGreaterThan(10);
  });

  it('claims nothing changed only where the table says so', () => {
    const claim = /nothing (changed|was (sent|saved|started|stored))|nothing more was sent/i;
    for (const r of rows) {
      const text = copy.errors.codes[r.code as keyof typeof copy.errors.codes];
      if (r.nothing === 'unknown') expect(text, r.code).not.toMatch(claim);
      // upstream_unavailable: for a decision only, so the sentence says which
      if (r.nothing === 'decision') expect(text, r.code).toMatch(/if you were deciding a draft, nothing was sent/i);
    }
  });

  it('has no em dashes in any of them', () => {
    for (const text of Object.values(copy.errors.codes)) expect(text).not.toContain('—');
  });
});
