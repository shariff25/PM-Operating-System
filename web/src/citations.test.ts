import { describe, expect, it } from 'vitest';
import { stripFromLine, stripMarkers } from './citations';

describe('stripMarkers', () => {
  it('removes markers from plain text, headings, lists and tables', () => {
    expect(stripMarkers('## Grace period[[s1]]\n\nSeven days[[s1]][[s4]], as agreed.\n\n- Retries stop[[s2]].')).toBe(
      '## Grace period\n\nSeven days, as agreed.\n\n- Retries stop.',
    );
    expect(stripMarkers('| a[[s2]] | b |')).toBe('| a | b |');
  });

  it('leaves a marker inside inline code or a fenced block exactly as written', () => {
    expect(stripMarkers('Write `[[s3]]` to cite[[s3]].')).toBe('Write `[[s3]]` to cite.');
    expect(stripMarkers('Before[[s1]]\n\n```\n[[s1]] stays\n```\n\nAfter[[s2]]')).toBe('Before\n\n```\n[[s1]] stays\n```\n\nAfter');
    expect(stripMarkers('~~~md\n[[s9]]\n~~~')).toBe('~~~md\n[[s9]]\n~~~');
    expect(stripMarkers('A ``code with ` and [[s1]]`` b[[s1]]')).toBe('A ``code with ` and [[s1]]`` b');
  });

  it('only matches the exact form with one to three digits', () => {
    expect(stripMarkers('[[s1234]] [[s]] [[x1]] [[s1]')).toBe('[[s1234]] [[s]] [[x1]] [[s1]');
  });

  it('returns text without markers untouched', () => {
    const t = 'Nothing to strip.\n\n```\ncode\n```';
    expect(stripMarkers(t)).toBe(t);
  });
});

describe('stripFromLine', () => {
  it('removes markers from a title and closes the gap', () => {
    expect(stripFromLine('PRD[[s1]] for grace period')).toBe('PRD for grace period');
    expect(stripFromLine('PRD [[s1]] for it')).toBe('PRD for it');
    expect(stripFromLine('Plain')).toBe('Plain');
  });
});
