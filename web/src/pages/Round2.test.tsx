import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Api, Draft } from '../api';
import { stripMarkers } from '../citations';
import { copy } from '../copy';
import { renderApp } from '../test/util';

const phone = () => {
  window.matchMedia = vi.fn().mockImplementation((query: string) => ({
    matches: query.includes('max-width'),
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  })) as unknown as typeof window.matchMedia;
};
afterEach(() => {
  delete (window as { matchMedia?: unknown }).matchMedia;
});

const dismissHint = async (user: ReturnType<typeof userEvent.setup>) =>
  user.click(within(document.querySelector('[data-hint]') as HTMLElement).getByRole('button', { name: copy.hintDismiss }));

describe('coach marks stay out of the way', () => {
  it('desktop: each hint sits in the flow next to what it describes, never over it', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd' });
    const sources = await screen.findByRole('region', { name: copy.draft.sourcesHeading });
    const hint = await screen.findByRole('note', { name: copy.hintLabel });
    expect(hint).toHaveAttribute('data-hint', 'sources');
    // under the sources heading, above the first source card
    expect(sources.contains(hint)).toBe(true);
    expect(hint.nextElementSibling).toHaveClass('sources');
    expect(hint).not.toHaveClass('coach-banner');

    await dismissHint(user);
    await waitFor(() => expect(document.querySelector('[data-hint]')).toHaveAttribute('data-hint', 'ask'));
    // right above the change box under the document
    const ask = document.querySelector('[data-hint]') as HTMLElement;
    expect(ask.nextElementSibling).toBe(screen.getByRole('form', { name: copy.draft.askHeading }));
  });

  it('desktop: the approve hint tops the side panel, not on the approve button', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd' });
    await screen.findByRole('note');
    await dismissHint(user);
    await dismissHint(user);
    await waitFor(() => expect(document.querySelector('[data-hint]')).toHaveAttribute('data-hint', 'approve'));
    const hint = document.querySelector('[data-hint]') as HTMLElement;
    const approve = screen.getByRole('button', { name: /^Approve to/ });
    expect(hint.contains(approve)).toBe(false);
    expect(approve.contains(hint)).toBe(false);
    expect(hint.parentElement).toBe(screen.getByRole('complementary', { name: copy.panel.label }));
    expect(hint.previousElementSibling).toBeNull();
    expect(approve).toBeEnabled();
    await dismissHint(user);
    await waitFor(() => expect(document.querySelector('[data-hint]')).toBeNull());
  });

  it('phone: one short line as a banner under the header, above the actions and the tabs', async () => {
    phone();
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd' });
    const hint = await screen.findByRole('note', { name: copy.hintLabel });
    expect(hint).toHaveClass('coach-banner');
    expect(hint).toHaveTextContent(copy.hintsShort.sources);
    const workspace = hint.parentElement as HTMLElement;
    expect(workspace).toHaveClass('workspace-phone');
    // only the screen-reader heading comes before it
    expect([...workspace.children].filter((c) => !c.classList.contains('sr-only'))[0]).toBe(hint);
    expect(screen.getByRole('tablist').contains(hint)).toBe(false);
    // it records each dismissal and moves on, still one at a time
    await dismissHint(user);
    await waitFor(() => expect(document.querySelector('[data-hint]')).toHaveAttribute('data-hint', 'ask'));
    expect(document.querySelectorAll('[data-hint]')).toHaveLength(1);
  });
});

describe('phone header actions', () => {
  it('collapse into the primary button and one More menu with Edit and Reject', async () => {
    phone();
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_jira' });
    const approve = await screen.findByRole('button', { name: copy.approve.button('jira_issue') });
    const more = screen.getByRole('button', { name: new RegExp(copy.panel.moreShort) });
    expect(more).toHaveAttribute('aria-haspopup', 'menu');
    // nothing else in the header: no loose Edit or Reject button
    expect(screen.queryByRole('button', { name: copy.draft.edit })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: copy.reject.button })).not.toBeInTheDocument();
    expect(approve).toBeInTheDocument();

    await user.click(more);
    const menu = screen.getByRole('menu', { name: copy.panel.more });
    const items = within(menu).getAllByRole('menuitem');
    expect(items.map((i) => i.textContent)).toEqual([copy.draft.edit, copy.reject.button]);
    expect(items[0]).toHaveFocus();
    await user.keyboard('{ArrowDown}');
    expect(items[1]).toHaveFocus();
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('menu', { name: copy.panel.more })).not.toBeInTheDocument();
    expect(more).toHaveFocus();
  });

  it('Edit from the menu opens the editor, and Reject from the menu opens the reasons', async () => {
    phone();
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_jira' });
    await user.click(await screen.findByRole('button', { name: new RegExp(copy.panel.moreShort) }));
    await user.click(screen.getByRole('menuitem', { name: copy.reject.button }));
    expect(await screen.findByRole('group', { name: copy.reject.heading })).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: copy.reject.cancel }));

    await user.click(screen.getByRole('button', { name: new RegExp(copy.panel.moreShort) }));
    await user.click(screen.getByRole('menuitem', { name: copy.draft.edit }));
    expect(await screen.findByLabelText(copy.draft.bodyLabel)).toBeInTheDocument();
  });

  it('desktop keeps Edit and Reject as buttons', async () => {
    renderApp({ hash: '/d/apr_jira' });
    expect(await screen.findByRole('button', { name: copy.draft.edit })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: copy.reject.button })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: new RegExp(`^${copy.panel.moreShort}$`) })).not.toBeInTheDocument();
  });
});

describe('before and after', () => {
  it('highlights changed words inside a line and shows unchanged paragraphs as the formatted document', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd' });
    await user.type(await screen.findByLabelText(copy.draft.askLabel), 'shorter');
    await user.click(screen.getByRole('button', { name: copy.draft.askButton }));
    const region = await screen.findByRole('region', { name: copy.draft.suggestionHeading });
    const list = within(region).getByRole('list', { name: copy.draft.beforeAfter });

    // changed words are marked on both sides
    const del = [...list.querySelectorAll('del.ba-word')].map((e) => e.textContent);
    const ins = [...list.querySelectorAll('ins.ba-word')].map((e) => e.textContent);
    expect(del.join(' ')).toMatch(/failure/);
    expect(ins.join(' ')).toMatch(/only/);
    // a reworded paragraph is shown once, with the old and new words in the same line
    const line = list.querySelector('.ba-changed .ba-text');
    expect(line?.querySelector('del')).not.toBeNull();
    expect(line?.querySelector('ins')).not.toBeNull();
    expect(line?.textContent).toContain('Customers on annual plans');
    // not by colour alone: the words carry a spoken cue
    expect(line?.querySelector('del .sr-only')?.textContent).toMatch(/removed/);
    expect(line?.querySelector('ins .sr-only')?.textContent).toMatch(/added/);

    // a removed line drops its markdown marker but keeps its words
    expect(within(list).queryByText('Is 14 days too generous', { exact: false })).not.toBeNull();
    // unchanged paragraphs read as the formatted document, not as raw markdown lines
    const same = [...list.querySelectorAll('.dd-same')];
    expect(same.length).toBeGreaterThan(0);
    expect(same.some((b) => b.querySelector('h2, li, strong'))).toBe(true);
    expect(same.some((b) => b.textContent?.includes('## '))).toBe(false);
  });
});

describe('the drafts table says where each draft goes', () => {
  it('shows the kind, and the channel for Slack drafts that carry one in the title', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/drafts' });
    await screen.findByRole('heading', { level: 1, name: copy.draftsPage.title });
    await user.click(screen.getByRole('tab', { name: /^All/ }));
    const rows = await screen.findAllByRole('link', { name: /Reply to Ben/ });
    expect(rows[0]).toHaveTextContent(`${copy.home.kindLabel('slack_reply')} · #partner-pricing`);
    const prd = screen.getByRole('link', { name: /PRD: TCB grace period/ });
    expect(prd).toHaveTextContent(copy.home.kindLabel('confluence_page'));
    expect(prd.textContent).not.toContain('#');
    expect(within(rows[0]!).getByText('Slack', { selector: '.sys-tag' })).toBeInTheDocument();
  });
});

describe('empty and first-run states', () => {
  it('Drafts with none: an icon, one line and one action that opens the palette ready to ask', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/drafts', noDrafts: true, firstVisit: false });
    const line = await screen.findByText(copy.emptyDrafts);
    const box = line.closest('.empty') as HTMLElement;
    expect(box.querySelector('svg')).not.toBeNull();
    await user.click(within(box).getByRole('button', { name: copy.emptyActions.askDraft }));
    expect(await screen.findByRole('dialog', { name: copy.palette.label })).toBeInTheDocument();
    expect(screen.getByRole('combobox')).toHaveValue(copy.emptyActions.askDraftText);
  });

  it('a filter with nothing in it offers Show all drafts', async () => {
    const user = userEvent.setup();
    renderApp({
      hash: '/drafts',
      wrap: (api) => {
        const list = api.listDrafts.bind(api);
        api.listDrafts = async () => (await list()).filter((d) => d.status === 'pending');
        return api;
      },
    });
    await user.click(await screen.findByRole('tab', { name: /^Approved/ }));
    const box = screen.getByText(copy.draftsPage.emptyApproved).closest('.empty') as HTMLElement;
    expect(box.querySelector('svg')).not.toBeNull();
    await user.click(within(box).getByRole('button', { name: copy.emptyActions.showAll }));
    expect(screen.getByRole('tab', { name: /^All/ })).toHaveAttribute('aria-selected', 'true');
  });

  it('Follow-ups with nothing: one empty state with an action', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/followups', emptyBrief: true });
    const line = await screen.findByText(copy.followups.emptyAll);
    const box = line.closest('.empty') as HTMLElement;
    expect(box.querySelector('svg')).not.toBeNull();
    await user.click(within(box).getByRole('button', { name: copy.emptyActions.askOpen }));
    expect(screen.getByRole('combobox')).toHaveValue(copy.emptyActions.askOpenText);
  });

  it('Activity with nothing: one line and an action', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/activity', noDrafts: true, firstVisit: false });
    const box = (await screen.findByText(copy.activity.empty)).closest('.empty') as HTMLElement;
    expect(box.querySelector('svg')).not.toBeNull();
    await user.click(within(box).getByRole('button', { name: copy.emptyActions.askFirst }));
    expect(await screen.findByRole('dialog', { name: copy.palette.label })).toBeInTheDocument();
  });

  it('a pending draft with no sources offers Ask me to add sources, which fills in the change box and puts the cursor there', async () => {
    const user = userEvent.setup();
    renderApp({
      hash: '/d/apr_jira',
      wrap: (api) => {
        const get = api.getDraft.bind(api);
        api.getDraft = async (id): Promise<Draft> => ({ ...(await get(id)), sources: [] });
        return api;
      },
    });
    const box = (await screen.findByText(copy.noSources)).closest('.empty') as HTMLElement;
    await user.click(within(box).getByRole('button', { name: copy.draft.askSources }));
    const change = await screen.findByLabelText(copy.draft.askLabel);
    expect(change).toHaveValue(copy.draft.askSourcesPrefill);
    await waitFor(() => expect(change).toHaveFocus());
  });
});

describe('sources as the backend sends them', () => {
  it('shows an id title as it is, and a source without a link has no Open link', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd' });
    const panel = await screen.findByRole('region', { name: copy.draft.sourcesHeading });
    await user.click(within(panel).getByRole('button', { name: /Confluence page 12345/ }));
    expect(within(panel).queryByRole('link', { name: /Open in source/ })).not.toBeInTheDocument();
    expect(within(panel).getByText(copy.draft.sourceNoLink)).toBeInTheDocument();
    // an excerpt is still shown when one is present
    expect(document.querySelector('.source-excerpt')).toBeNull();
  });

  it('still renders an excerpt when the server sends one', async () => {
    renderApp({
      hash: '/d/apr_prd',
      wrap: (api) => {
        const get = api.getDraft.bind(api);
        api.getDraft = async (id): Promise<Draft> => {
          const d = await get(id);
          return { ...d, sources: d.sources.map((s, i) => (i === 0 ? { ...s, excerpt: 'The passage itself' } : s)) };
        };
        return api;
      },
    });
    expect(await screen.findByText(/The passage itself/)).toBeInTheDocument();
  });
});

describe('before and after: lighter fallbacks', () => {
  const OLD = 'Customers on annual plans lose access the moment a payment fails, even when the failure is a card that expired yesterday. Support handled 140 tickets about this last quarter.';
  const withBody = (body: (old: string) => string) => (a: Api) => {
    const orig = a.getDraft.bind(a);
    let old = '';
    a.getDraft = async (id) => {
      const d = await orig(id);
      // A suggestion never carries source markers, so it is built from the text without them.
      old = stripMarkers(d.body);
      return d;
    };
    a.getSuggestion = async () => ({ status: 'ready', instruction: 'x', body: body(old), error: null });
    return a;
  };
  const ask = async (user: ReturnType<typeof userEvent.setup>) => {
    await user.type(await screen.findByLabelText(copy.draft.askLabel), 'rewrite');
    await user.click(screen.getByRole('button', { name: copy.draft.askButton }));
    const region = await screen.findByRole('region', { name: copy.draft.suggestionHeading });
    return within(region).getByRole('list', { name: copy.draft.beforeAfter });
  };

  it('collapses a wholly rewritten paragraph to a toggle, with the new text in full', async () => {
    const user = userEvent.setup();
    const fresh = 'Billing failures should never end a workspace sudden shutdown for anyone, so we are rethinking how access ends.';
    renderApp({ hash: '/d/apr_prd', wrap: withBody((old) => old.replace(OLD, fresh)) });
    const list = await ask(user);
    const toggle = within(list).getByRole('button', { name: copy.draft.diffShowRemoved });
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    const group = document.getElementById(toggle.getAttribute('aria-controls')!)!;
    expect(group).toHaveClass('ba-clamp');
    // the semantics stay: old text in del, new text in ins, each with a spoken label
    expect(group.querySelector('del')?.textContent).toContain('Customers on annual plans');
    expect(group).toHaveTextContent(copy.draft.diffRemoved);
    const added = within(list).getByText(fresh);
    expect(added.tagName).toBe('INS');
    expect(added.closest('p')).toHaveTextContent(copy.draft.diffAdded);

    // keyboard: the toggle is a real button that opens and closes
    toggle.focus();
    await user.keyboard('{Enter}');
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    expect(toggle).toHaveTextContent(copy.draft.diffHideRemoved);
    expect(group).not.toHaveClass('ba-clamp');
    await user.keyboard(' ');
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(toggle).toHaveTextContent(copy.draft.diffShowRemoved);
  });

  it('shows one note instead of every item when only the list marker changed', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd', wrap: withBody((old) => old.replace('- Involuntary churn', '* Involuntary churn').replace('- Payment-failure', '* Payment-failure')) });
    const list = await ask(user);
    expect(within(list).getByText(copy.draft.diffListStyle)).toBeInTheDocument();
    expect(list.querySelectorAll('del, ins')).toHaveLength(0);
    expect(within(list).queryByText(copy.draft.diffNoChange)).not.toBeInTheDocument();
  });
});
