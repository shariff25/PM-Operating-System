// @vitest-environment node
import { mkdtempSync, readdirSync, readFileSync, rmSync, statSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { build } from 'vite';

const root = fileURLToPath(new URL('..', import.meta.url));

function files(dir: string): string[] {
  return readdirSync(dir).flatMap((f) => {
    const p = join(dir, f);
    return statSync(p).isDirectory() ? files(p) : [p];
  });
}

// Strings that exist only in the sample-data client and its sample drafts.
const MOCK_MARKERS = ['mock-csrf-token', 'PRD: TCB grace period', 'Vendor call notes', 'pm-ana'];

const outDirs: string[] = [];
const nodeEnv = process.env.NODE_ENV;
// `vite build` runs with NODE_ENV=production; vitest sets it to "test", which would count as dev.
beforeEach(() => {
  process.env.NODE_ENV = 'production';
});
afterEach(() => {
  process.env.NODE_ENV = nodeEnv;
  delete process.env.VITE_API;
  for (const d of outDirs.splice(0)) rmSync(d, { recursive: true, force: true });
});

describe('production build (review finding 3)', () => {
  it('contains no mock client code', async () => {
    delete process.env.VITE_API;
    const outDir = mkdtempSync(join(tmpdir(), 'pmos-dist-'));
    outDirs.push(outDir);
    await build({ root, mode: 'production', logLevel: 'silent', build: { outDir, emptyOutDir: true } });
    const all = files(outDir);
    expect(all.some((f) => f.endsWith('.js'))).toBe(true);
    for (const f of all.filter((p) => /\.(js|html|css|map)$/.test(p))) {
      const text = readFileSync(f, 'utf8');
      for (const marker of MOCK_MARKERS) expect(text, `${marker} in ${f}`).not.toContain(marker);
    }
    // Geist and Geist Mono ship as files on this origin: no font host, no data: fonts.
    const css = all.filter((f) => f.endsWith('.css')).map((f) => readFileSync(f, 'utf8')).join('\n');
    expect(css).toContain('Geist Variable');
    expect(css).toContain('Geist Mono Variable');
    expect(css).not.toMatch(/url\(\s*['"]?(https?:|\/\/|data:)/);
    expect(all.filter((f) => f.endsWith('.woff2')).length).toBeGreaterThan(0);
  }, 120_000);

  it('refuses to build with VITE_API=mock', async () => {
    process.env.VITE_API = 'mock';
    const outDir = mkdtempSync(join(tmpdir(), 'pmos-dist-'));
    outDirs.push(outDir);
    await expect(
      build({ root, mode: 'production', logLevel: 'silent', build: { outDir, emptyOutDir: true } }),
    ).rejects.toThrow(/VITE_API=mock is for development only/);
  }, 120_000);

  it('does not set the mock in .env.example', () => {
    const example = readFileSync(join(root, '.env.example'), 'utf8');
    expect(example).not.toMatch(/^\s*VITE_API\s*=\s*mock/m);
  });

  it('names no third-party host in the page or the styles (review finding 7)', () => {
    const html = readFileSync(join(root, 'index.html'), 'utf8');
    const css = readFileSync(join(root, 'src/styles.css'), 'utf8');
    expect(html).not.toMatch(/fonts\.(googleapis|gstatic)\.com/);
    expect(html + css).not.toMatch(/https?:\/\/(?!www\.w3\.org)[^'")\s]+/);
    expect(css).not.toMatch(/@import|@font-face/);
  });
});
