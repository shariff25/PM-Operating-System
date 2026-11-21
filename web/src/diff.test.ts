import { describe, expect, it } from 'vitest';
import { INLINE_MIN_SHARED, diffBlocks, diffInline, diffLines, diffWords } from './diff';

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

describe('diffInline', () => {
  it('shows old and new words once, in order', () => {
    const d = diffInline('The grace period is 14 days', 'The grace period is 7 days');
    expect(d.parts).toEqual([
      { text: 'The grace period is ', kind: 'same' },
      { text: '14', kind: 'removed' },
      { text: '7', kind: 'added' },
      { text: ' days', kind: 'same' },
    ]);
    expect(d.shared).toBe(5);
    expect(d.most).toBe(6);
  });
});

describe('inline pairing', () => {
  it('merges a lightly reworded paragraph into one inline line', () => {
    const d = diffLines('keep\nThe grace period is 14 days\nend', 'keep\nThe grace period is 7 days\nend');
    const removed = d.find((l) => l.kind === 'removed')!;
    const added = d.find((l) => l.kind === 'added')!;
    expect(removed.merged).toBe(true);
    expect(added.inline?.some((p) => p.kind === 'removed' && p.text === '14')).toBe(true);
    expect(added.inline?.some((p) => p.kind === 'added' && p.text === '7')).toBe(true);
  });

  it('keeps the threshold at about 40% shared words', () => {
    expect(INLINE_MIN_SHARED).toBe(0.4);
    // 2 of 5 words shared: inline.
    const at = diffLines('a b c d e', 'a b x y z');
    expect(at.find((l) => l.kind === 'added')?.inline).toBeDefined();
    // 1 of 5 shared: old and new blocks.
    const under = diffLines('a b c d e', 'a x y z w');
    expect(under.find((l) => l.kind === 'added')?.inline).toBeUndefined();
    expect(under.find((l) => l.kind === 'removed')?.merged).toBeUndefined();
  });
});

describe('list markers', () => {
  it('does not show a change when only the list marker changed', () => {
    const d = diffLines('Intro\n1. First thing\n2. Second thing', 'Intro\n- First thing\n- Second thing');
    expect(d.map((l) => l.kind)).toEqual(['same', 'same', 'same']);
    expect(d.map((l) => l.text)).toEqual(['Intro', '- First thing', '- Second thing']);
    expect(d.map((l) => !!l.markerOnly)).toEqual([false, true, true]);
  });

  it('treats an unchanged marker as plain unchanged', () => {
    const d = diffLines('- a\n- b', '- a\n- b');
    expect(d.some((l) => l.markerOnly)).toBe(false);
  });

  it('shows a reworded item without a struck marker', () => {
    const d = diffLines('1. The grace period is 14 days', '- The grace period is 7 days');
    expect(d.map((l) => l.kind)).toEqual(['removed', 'added']);
    const added = d[1]!;
    expect(added.inline?.[0]).toEqual({ text: '- ', kind: 'same' });
    expect(added.inline?.filter((p) => p.kind !== 'same').map((p) => p.text)).toEqual(['14', '7']);
    expect(added.parts?.filter((p) => p.changed).map((p) => p.text)).toEqual(['7']);
  });

  it('still reports real added items when markers changed too', () => {
    const d = diffLines('- a\n- b', '* a\n* b\n* c');
    expect(d.map((l) => l.kind)).toEqual(['same', 'same', 'added']);
  });

  it('marks a paragraph whose list only changed style', () => {
    const blocks = diffBlocks(diffLines('Intro\n\n1. a\n2. b\n\nEnd', 'Intro\n\n- a\n- b\n\nEnd'));
    expect(blocks.map((b) => b.styleOnly)).toEqual([false, true, false]);
    expect(blocks.every((b) => !b.changed)).toBe(true);
  });

  it('does not call a list style-only when its words changed too', () => {
    const blocks = diffBlocks(diffLines('1. a\n2. b', '- a\n- c'));
    expect(blocks[0]!.changed).toBe(true);
    expect(blocks[0]!.styleOnly).toBe(false);
  });
});
