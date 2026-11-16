import { describe, expect, it } from 'vitest';
import { diffBlocks, diffLines, diffWords } from './diff';

describe('diffWords', () => {
  it('marks only the words that changed, on each side', () => {
    const d = diffWords('Support handled 140 tickets last quarter', 'Support handled 160 tickets last month');
    expect(d.before.filter((p) => p.changed).map((p) => p.text)).toEqual(['140', 'quarter']);
    expect(d.after.filter((p) => p.changed).map((p) => p.text)).toEqual(['160', 'month']);
    expect(d.before.map((p) => p.text).join('')).toBe('Support handled 140 tickets last quarter');
    expect(d.after.map((p) => p.text).join('')).toBe('Support handled 160 tickets last month');
  });

  it('joins neighbouring changed words into one highlight', () => {
    const d = diffWords('a very old card', 'a brand new card');
    expect(d.before.filter((p) => p.changed).map((p) => p.text)).toEqual(['very old']);
    expect(d.after.filter((p) => p.changed).map((p) => p.text)).toEqual(['brand new']);
  });
});

describe('diffLines', () => {
  it('pairs a rewritten line with its old line and marks the words', () => {
    const d = diffLines('Keep this\nThe grace period is 14 days\nAlso this', 'Keep this\nThe grace period is 7 days\nAlso this');
    expect(d.map((l) => l.kind)).toEqual(['same', 'removed', 'added', 'same']);
    expect(d[1]!.parts?.filter((p) => p.changed).map((p) => p.text)).toEqual(['14']);
    expect(d[2]!.parts?.filter((p) => p.changed).map((p) => p.text)).toEqual(['7']);
  });

  it('shows a replaced line with nothing in common as a plain removal and addition', () => {
    const d = diffLines('Alpha beta gamma', 'Completely different words here');
    expect(d.map((l) => l.kind)).toEqual(['removed', 'added']);
    expect(d[0]!.parts).toBeUndefined();
    expect(d[1]!.parts).toBeUndefined();
  });

  it('shows a removed line with no partner as a plain removal', () => {
    const d = diffLines('one\ntwo\nthree', 'one\nthree');
    expect(d.map((l) => [l.kind, l.text])).toEqual([['same', 'one'], ['removed', 'two'], ['same', 'three']]);
  });
});

describe('diffBlocks', () => {
  it('groups lines into paragraphs and says which paragraphs changed', () => {
    const before = '## A\nsame one\n\n## B\nsame two\n\n## C\nold words here';
    const after = '## A\nsame one\n\n## B\nsame two\n\n## C\nnew words here';
    const blocks = diffBlocks(diffLines(before, after));
    expect(blocks.map((b) => b.changed)).toEqual([false, false, true]);
    expect(blocks[2]!.lines.map((l) => l.kind)).toEqual(['same', 'removed', 'added']);
  });

  it('drops blank lines instead of showing them as changes', () => {
    const blocks = diffBlocks(diffLines('a\n\nb', 'a\n\n\n\nb'));
    expect(blocks.every((b) => !b.changed)).toBe(true);
  });
});
