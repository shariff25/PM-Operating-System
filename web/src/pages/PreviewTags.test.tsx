import { render, screen, waitFor, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import type { Api } from '../api';
import type { BotIdentity } from '../bots';
import { BotByIdentity } from '../components/BotParts';
import { copy } from '../copy';
import { renderApp } from '../test/util';

// Security review N1: every place that names a bot carries the Preview tag when the bot is a
// Preview, not only the draft page and the thread: the rows of Today, Drafts, Activity and the
// review Inbox, the review page, the brief's "Written by", and the closed picker button.
// Nudge never carries it; a bot the app cannot check (no catalog) does.

const B = copy.bots;

/** The Preview tag's text inside an element, or null. */
const tagIn = (el: HTMLElement) => within(el).queryByText(B.preview);

describe('Preview on the lists', () => {
  it('Today: a decide row for Chief says Preview, one for Nudge does not', async () => {
    renderApp();
    const chiefs = await screen.findAllByText(B.byLabel('Chief'));
    for (const c of chiefs) {
      const row = c.closest('a.decide-row') as HTMLElement | null;
      if (row) expect(tagIn(row)).toBeInTheDocument();
    }
    expect(chiefs.some((c) => c.closest('a.decide-row'))).toBe(true);
    const nudges = await screen.findAllByText(B.byLabel('Nudge'));
    const rows = nudges.map((n) => n.closest('a.decide-row') as HTMLElement | null).filter((r) => r !== null);
    expect(rows.length).toBeGreaterThan(0);
    for (const row of rows) expect(tagIn(row)).not.toBeInTheDocument();
  });

  it('Drafts: Chief rows say Preview, Nudge rows do not', async () => {
    renderApp({ hash: '/drafts' });
    const chiefs = await screen.findAllByText(B.byLabel('Chief'));
    for (const c of chiefs) expect(tagIn(c.closest('a')!)).toBeInTheDocument();
    const nudges = await screen.findAllByText(B.byLabel('Nudge'));
    for (const n of nudges) expect(tagIn(n.closest('a')!)).not.toBeInTheDocument();
  });

  it('Activity: a Chief request says Preview, a Nudge one does not', async () => {
    renderApp({ hash: '/activity' });
    const chiefs = await screen.findAllByText(B.byLabel('Chief'));
    for (const c of chiefs) expect(tagIn(c.closest('a')!)).toBeInTheDocument();
    const nudges = await screen.findAllByText(B.byLabel('Nudge'));
    for (const n of nudges) expect(tagIn(n.closest('a')!)).not.toBeInTheDocument();
  });

  it('the review Inbox and the review page say Preview for Sprout, not for Nudge', async () => {
    const one = renderApp({ role: 'reviewer', hash: '/reviews' });
    const sprouts = await screen.findAllByText(B.byLabel('Sprout'));
    for (const s of sprouts) expect(tagIn(s.closest('li')!)).toBeInTheDocument();
    const nudges = await screen.findAllByText(B.byLabel('Nudge'));
    for (const n of nudges) expect(tagIn(n.closest('li')!)).not.toBeInTheDocument();
    one.unmount();
  });

  it('a restricted review page (metadata only) says Preview for a Preview bot', async () => {
    renderApp({
      role: 'reviewer',
      hash: '/reviews/apr_dana_launch',
      wrap: (api: Api) => {
        const real = api.getReview.bind(api);
        api.getReview = async (id) => ({ ...(await real(id)), persona: 'chief', persona_offered: true });
        return api;
      },
    });
    const field = (await screen.findAllByText(B.byLabel('Chief')))[0]!.closest('dd') as HTMLElement;
    expect(tagIn(field)).toBeInTheDocument();
  });
});

describe('Preview on the brief and the picker', () => {
  it('"Written by Chief" says Preview; "Written by Nudge" does not', async () => {
    const one = renderApp({ briefWriter: 'chief' });
    const line = (await screen.findByText(copy.today.briefReady, { exact: false })).closest('p')!;
    expect(within(line).getByText(copy.today.briefBy('Chief'))).toBeInTheDocument();
    expect(tagIn(line)).toBeInTheDocument();
    one.unmount();
    renderApp({});
    const nudgeLine = (await screen.findByText(copy.today.briefReady, { exact: false })).closest('p')!;
    expect(within(nudgeLine).getByText(copy.today.briefBy('Nudge'))).toBeInTheDocument();
    expect(tagIn(nudgeLine)).not.toBeInTheDocument();
  });

  it('the closed picker button says Preview when Chief is chosen', async () => {
    renderApp();
    const button = await screen.findByRole('button', { name: B.picker.button('Chief') });
    expect(tagIn(button)).toBeInTheDocument();
  });

  it('with no catalog to check, a named bot is shown as a Preview, never hidden', async () => {
    renderApp({
      hash: '/drafts',
      wrap: (api: Api) => {
        api.bots = async () => {
          throw new Error('down');
        };
        return api;
      },
    });
    const by = await screen.findAllByText(B.byLabel(B.unchecked));
    await waitFor(() => expect(by.length).toBeGreaterThan(0));
    for (const b of by) expect(tagIn(b.closest('a')!)).toBeInTheDocument();
  });

  it('a bot no longer offered is named plainly, without the tag', () => {
    const who: BotIdentity = { kind: 'gone', name: B.gone, mascot: 'unknown', preview: false };
    const { container } = render(<BotByIdentity who={who} />);
    expect(screen.getByText(B.byLabel(B.gone))).toBeInTheDocument();
    expect(tagIn(container)).not.toBeInTheDocument();
  });

  it('BotByIdentity tags a Preview and not a benchmarked bot', () => {
    const { container, rerender } = render(<BotByIdentity who={{ kind: 'bot', name: 'Echo', mascot: 'echo', preview: true }} />);
    expect(tagIn(container)).toBeInTheDocument();
    rerender(<BotByIdentity who={{ kind: 'bot', name: 'Echo', mascot: 'echo', preview: false }} />);
    expect(tagIn(container)).not.toBeInTheDocument();
  });
});
