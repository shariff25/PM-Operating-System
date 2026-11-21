import { screen, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import type { Api, Bots } from '../api';
import realBots from '../api/fixtures/realBots.json';
import { copy } from '../copy';
import { renderApp } from '../test/util';

// The security gate's H1 (gate-fix-4): the Preview label ("not yet benchmarked") reaches every
// screen from pmos-web's *real* `GET /api/bots` answer, not only from the sample data's.
// `../api/fixtures/realBots.json` is that answer, exactly: the Rust route test
// `the_real_bots_answer_carries_the_preview_label_the_app_shows` (crates/web/tests/bots.rs)
// builds it from the repository's catalog through the tool server's own projection and fails
// when this file differs.

const B = copy.bots;
const REAL = realBots as unknown as Bots;

/** The sample client, answering `bots()` with the real answer (optionally changed). */
function withReal(change?: (b: Bots) => void) {
  return (api: Api) => {
    api.bots = async () => {
      const b = structuredClone(REAL);
      change?.(b);
      return b;
    };
    return api;
  };
}

describe('the real Bots answer', () => {
  it('has the fields the screens read, and Chief, Sprout and Echo are Previews', () => {
    const by = (k: string) => REAL.bots.find((b) => b.key === k)!;
    for (const k of ['chief', 'sprout', 'echo']) expect(by(k).preview, k).toBe(true);
    expect(by('nudge').preview).toBe(false);
    for (const b of REAL.bots) {
      for (const field of ['key', 'display_name', 'title', 'mascot', 'always_on', 'preview', 'summary', 'examples', 'can_do', 'reads', 'drafts', 'needs', 'on_team', 'added_at', 'added_via', 'new', 'drafts_this_month', 'last_used_at', 'routines']) {
        expect(b, `${b.key}.${field}`).toHaveProperty(field);
      }
    }
  });

  it('the Bots page tags each Preview from it', async () => {
    renderApp({ hash: '/bots', wrap: withReal() });
    const details = await screen.findByRole('button', { name: B.page.details('Chief') });
    expect(within(details.closest('li')!).getByText(B.preview)).toBeInTheDocument();
    for (const name of ['Sprout', 'Echo']) {
      const row = screen.getByRole('button', { name: B.page.details(name) }).closest('li')!;
      expect(within(row).getByText(B.preview), name).toBeInTheDocument();
    }
  });

  it('the draft page tags "Drafted by Chief" as a Preview', async () => {
    renderApp({ hash: '/d/apr_jira', wrap: withReal() });
    const line = (await screen.findByText(B.draftedBy('Chief'))).closest('.drafted-by')!;
    expect(within(line as HTMLElement).getByText(B.preview)).toBeInTheDocument();
    expect(line.textContent).toContain(B.previewNote);
  });

  it('the thread header tags a Preview bot\'s answer', async () => {
    renderApp({ hash: '/a/tsk_seed_week', wrap: withReal() });
    const answer = await screen.findByRole('article', { name: 'Chief' });
    const head = answer.querySelector('.msg-head') as HTMLElement;
    expect(within(head).getByText(B.preview)).toBeInTheDocument();
  });

  it('a benchmarked bot (`preview: false`) carries no tag', async () => {
    const benchmarked = withReal((b) => {
      for (const card of b.bots) card.preview = false;
    });
    const one = renderApp({ hash: '/d/apr_jira', wrap: benchmarked });
    const line = (await screen.findByText(B.draftedBy('Chief'))).closest('.drafted-by')!;
    expect(within(line as HTMLElement).queryByText(B.preview)).not.toBeInTheDocument();
    one.unmount();
    renderApp({ hash: '/a/tsk_seed_week', wrap: benchmarked });
    const answer = await screen.findByRole('article', { name: 'Chief' });
    expect(within(answer.querySelector('.msg-head') as HTMLElement).queryByText(B.preview)).not.toBeInTheDocument();
  });
});
