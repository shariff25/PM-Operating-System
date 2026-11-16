import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Draft } from '../api';
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
  it('desktop: the sources and ask hints sit between the tabs and the panel, never inside a tab', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd' });
    const tabs = await screen.findByRole('tablist', { name: copy.panel.tabsLabel });
    const hint = await screen.findByRole('note', { name: copy.hintLabel });
    expect(hint).toHaveAttribute('data-hint', 'sources');
    expect(tabs.contains(hint)).toBe(false);
    expect(tabs.nextElementSibling).toBe(hint);
    expect(hint.nextElementSibling).toHaveAttribute('role', 'tabpanel');
    expect(hint).not.toHaveClass('coach-banner');

    await dismissHint(user);
    await waitFor(() => expect(document.querySelector('[data-hint]')).toHaveAttribute('data-hint', 'ask'));
    expect(tabs.contains(document.querySelector('[data-hint]'))).toBe(false);
  });

  it('desktop: the approve hint is above the tabs, not on the approve button', async () => {
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
    expect(hint.nextElementSibling).toBe(screen.getByRole('tablist', { name: copy.panel.tabsLabel }));
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
  it('highlights changed words inside a line and folds unchanged paragraphs', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd' });
    await user.click(await screen.findByRole('tab', { name: copy.panel.ask }));
    await user.type(screen.getByLabelText(copy.draft.askLabel), 'shorter');
    await user.click(screen.getByRole('button', { name: copy.draft.askButton }));
    const region = await screen.findByRole('region', { name: copy.draft.suggestionHeading });
    const list = within(region).getByRole('list', { name: copy.draft.beforeAfter });

    // changed words are marked on both sides
    const del = [...list.querySelectorAll('del.ba-word')].map((e) => e.textContent);
    const ins = [...list.querySelectorAll('ins.ba-word')].map((e) => e.textContent);
    expect(del.join(' ')).toMatch(/failure/);
    expect(ins.join(' ')).toMatch(/only/);
    // the rest of the changed line is plain, not struck through
    expect(list.querySelector('.ba-removed .ba-text')?.textContent).toContain('Customers on annual plans');

    // unchanged paragraphs are one folded row that opens
    const fold = within(list).getByRole('button', { name: /unchanged paragraph/ });
    expect(fold).toHaveAttribute('aria-expanded', 'false');
    expect(within(list).queryByText('Is 14 days too generous', { exact: false })).not.toBeNull(); // removed, so shown
    expect(list.querySelectorAll('.ba-fold .ba-same')).toHaveLength(0);
    await user.click(fold);
    expect(fold).toHaveAttribute('aria-expanded', 'true');
    expect(list.querySelectorAll('.ba-fold .ba-same').length).toBeGreaterThan(0);
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
    expect(screen.getByText(copy.draftsPage.columns.kind)).toBeInTheDocument();
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

  it('a pending draft with no sources offers Ask me to add sources, which opens the ask tab with the words filled in', async () => {
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
    expect(await screen.findByLabelText(copy.draft.askLabel)).toHaveValue(copy.draft.askSourcesPrefill);
  });
});

describe('sources as the backend sends them', () => {
  it('shows an id title as it is, and a source without a link has no Open link', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd' });
    const panel = await screen.findByRole('tabpanel');
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
