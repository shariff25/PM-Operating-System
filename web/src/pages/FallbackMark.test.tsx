import { screen, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import type { Api } from '../api';
import { copy } from '../copy';
import { renderApp } from '../test/util';

// Slice MP7 (docs/model-providers-spec.md §4.10, MP-18): a reply a fallback model wrote, and each
// draft card it made, says "Answered by a fallback model: not benchmarked for this bot". A reply
// from the bot's own model is unchanged.

const MARK = copy.bots.fallbackNote;

function answeredByFallback(api: Api): Api {
  const get = api.getTask.bind(api);
  api.getTask = async (id) => ({ ...(await get(id)), fallback_used: true });
  return api;
}

describe('the fallback mark', () => {
  it('is on the reply and on each draft card of a task a fallback model answered', async () => {
    renderApp({ hash: '#/a/tsk_seed_prd', wrap: answeredByFallback });
    const reply = await screen.findByRole('article', { name: (n) => n !== copy.activity.you });
    const marks = await within(reply).findAllByText(MARK);
    // The PRD task made one draft: one mark under the answer, one on its card.
    expect(marks.length).toBeGreaterThanOrEqual(1);
    expect(MARK).toBe('Answered by a fallback model: not benchmarked for this bot');
    const card = reply.querySelector('.draft-card');
    expect(card).not.toBeNull();
    expect(within(card as HTMLElement).getByText(MARK)).toBeInTheDocument();
  });

  it('is absent from a reply the bot’s own model wrote', async () => {
    renderApp({ hash: '#/a/tsk_seed_prd' });
    const reply = await screen.findByRole('article', { name: (n) => n !== copy.activity.you });
    await within(reply).findAllByRole('link');
    expect(within(reply).queryByText(MARK)).toBeNull();
  });
});
