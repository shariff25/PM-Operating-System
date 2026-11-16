import { screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import type { Api } from './api';
import { copy } from './copy';
import { draftIdFromPath, isSafeId, safeUrl } from './url';
import { renderApp } from './test/util';

describe('route ids (review findings 4 and 6)', () => {
  it('shows the home page, not a blank screen, for a malformed hash', async () => {
    renderApp({ hash: '/d/%E0' });
    await screen.findByLabelText(copy.home.askLabel);
  });

  it.each(['/d/..%2Ftasks%3F', '/d/..', '/d/a%2Fb', '/d/%2e%2e', '/d/', '/d/a/b'])(
    'does not call the API for %s',
    async (hash) => {
      const getDraft = vi.fn();
      renderApp({
        hash,
        wrap: (a: Api) => {
          a.getDraft = getDraft;
          return a;
        },
      });
      await screen.findByLabelText(copy.home.askLabel);
      expect(getDraft).not.toHaveBeenCalled();
    },
  );

  it('opens a normal id', async () => {
    renderApp({ hash: '/d/apr_prd' });
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
  });

  it('draftIdFromPath and isSafeId accept plain tokens only', () => {
    expect(draftIdFromPath('/d/apr_123-x')).toBe('apr_123-x');
    expect(draftIdFromPath('/d/%E0')).toBeNull();
    expect(draftIdFromPath('/d/..')).toBeNull();
    expect(draftIdFromPath('/d/a%3Fb')).toBeNull();
    expect(isSafeId('apr_1')).toBe(true);
    expect(isSafeId('../x')).toBe(false);
    expect(isSafeId('')).toBe(false);
  });
});

describe('safeUrl (review finding 5)', () => {
  it('allows http and https only', () => {
    expect(safeUrl('https://example.com/a')).toBe('https://example.com/a');
    expect(safeUrl('http://example.com')).toBe('http://example.com/');
    for (const bad of ['javascript:alert(1)', ' JaVaScRiPt:alert(1)', 'data:text/html,x', 'vbscript:x', 'mailto:a@b.c', '/api/tasks', '#/d/x', '', null, undefined]) {
      expect(safeUrl(bad as string | null | undefined), String(bad)).toBeNull();
    }
  });
});
