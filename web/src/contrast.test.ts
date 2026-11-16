// @vitest-environment node
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const css = readFileSync(new URL('./styles.css', import.meta.url), 'utf8');

function tokens(block: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const m of block.matchAll(/--([a-z0-9-]+):\s*(#[0-9a-fA-F]{6})\s*;/g)) out[m[1]!] = m[2]!;
  return out;
}

const light = tokens(css.slice(0, css.indexOf('@media (prefers-color-scheme: dark)')));
const darkStart = css.indexOf('@media (prefers-color-scheme: dark)');
const dark = { ...light, ...tokens(css.slice(darkStart, css.indexOf('* { box-sizing')) ) };

function lum(hex: string): number {
  const c = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
  const [r, g, b] = c.map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4)) as [number, number, number];
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}
function ratio(a: string, b: string): number {
  const [hi, lo] = [lum(a), lum(b)].sort((x, y) => y - x) as [number, number];
  return (hi + 0.05) / (lo + 0.05);
}

// Text on its background. Each pair is used for words in the app.
const PAIRS: [string, string][] = [
  ['ink', 'bg'], ['ink', 'surface'], ['muted', 'bg'], ['muted', 'surface'], ['muted', 'accent-soft'],
  ['accent', 'surface'], ['accent', 'bg'], ['accent', 'accent-soft'], ['accent-ink', 'accent'],
  ['warn-ink', 'warn-bg'], ['good-ink', 'good-bg'], ['bad-ink', 'bad-bg'],
  ['disabled-ink', 'disabled-bg'], ['ink', 'accent-soft'], ['warn-ink', 'surface'], ['bad-ink', 'surface'],
  // The redesign: primary buttons, the sidebar, selected rows, hints, toasts and quiet pills.
  ['primary-ink', 'primary'], ['ink', 'nav-active'], ['muted', 'nav-active'], ['ink', 'surface-2'],
  ['muted', 'surface-2'], ['coach-ink', 'coach-bg'], ['inverse-ink', 'inverse-bg'], ['warn-ink', 'bg'],
  ['good-ink', 'surface'], ['good-ink', 'bg'], ['bad-ink', 'bg'], ['accent', 'nav-active'], ['surface', 'bad-ink'],
];

describe('dark tokens', () => {
  it('are the same for the system preference and for the menu choice', () => {
    const media = tokens(css.slice(darkStart, css.indexOf(":root[data-theme='dark']")));
    const chosen = tokens(css.slice(css.indexOf(":root[data-theme='dark']"), css.indexOf('* { box-sizing')));
    expect(Object.keys(media).length).toBeGreaterThan(20);
    expect(chosen).toEqual(media);
  });
});

describe.each([['light', light], ['dark', dark]] as const)('%s theme contrast', (_name, t) => {
  it.each(PAIRS)('%s on %s is at least 4.5:1', (fg, bg) => {
    expect(t[fg], fg).toBeDefined();
    expect(t[bg], bg).toBeDefined();
    expect(ratio(t[fg]!, t[bg]!), `${fg} on ${bg}`).toBeGreaterThanOrEqual(4.5);
  });

  it('input borders and focus rings are at least 3:1 against the surface', () => {
    expect(ratio(t['border-strong']!, t['surface']!)).toBeGreaterThanOrEqual(3);
    expect(ratio(t['focus']!, t['bg']!)).toBeGreaterThanOrEqual(3);
  });
});
