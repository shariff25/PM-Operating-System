import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import type { Api } from '../api';
import { copy } from '../copy';
import { renderApp } from '../test/util';

const card = () => screen.queryByRole('region', { name: copy.tour.hello('Ana') });
const findCard = (name = 'Ana') => screen.findByRole('region', { name: copy.tour.hello(name) });
const nudges = (root: ParentNode = document) => [...root.querySelectorAll('svg[data-nudge]')].map((e) => e.getAttribute('data-nudge'));
const stepDone = (key: string) => document.querySelector(`[data-step="${key}"]`)?.getAttribute('data-done');
const goHome = async (user: ReturnType<typeof userEvent.setup>) => {
  await user.click(screen.getAllByRole('link', { name: copy.nav.home })[0]!);
  await screen.findByLabelText(copy.home.askLabel);
};
const navLink = (name: string) => screen.getAllByRole('link', { name: new RegExp(`^${name}`) })[0]!;
const openMenu = async (user: ReturnType<typeof userEvent.setup>) => user.click(await screen.findByRole('button', { name: /Menu for/ }));
const approvePractice = async (user: ReturnType<typeof userEvent.setup>) => {
  await user.click(await screen.findByRole('button', { name: 'Approve to Confluence' }));
  await user.click(screen.getByRole('button', { name: copy.approve.confirm('confluence_page') }));
  await screen.findByText(copy.approve.practiceApproved);
};

describe('the guided start on Today', () => {
  it('welcomes a first visit with Nudge waving, two sentences and three steps at 0 of 3', async () => {
    const marked = vi.fn();
    renderApp({
      noDrafts: true,
      wrap: (a: Api) => {
        const orig = a.markHintSeen.bind(a);
        a.markHintSeen = (n) => { marked(n); return orig(n); };
        return a;
      },
    });
    const welcome = await findCard();
    expect(within(welcome).getByText(copy.tour.lede)).toBeInTheDocument();
    expect(within(welcome).getByText(copy.tour.progressLabel(0, 3))).toBeInTheDocument();
    expect(within(welcome).getAllByRole('listitem')).toHaveLength(3);
    expect(nudges(welcome)).toEqual(['hello']);
    expect(welcome.querySelector('svg[data-nudge]')).toHaveAttribute('aria-hidden', 'true');
    // the guided start is recorded, so opening a draft (which ends the first visit) does not end it
    await waitFor(() => expect(marked).toHaveBeenCalledWith('tour_started'));
    // the practice draft is made, not opened: the page stays on Today
    expect(await screen.findByRole('link', { name: /Practice: why PMs should take the stairs/ })).toBeInTheDocument();
    expect(window.location.hash).toBe('');
  });

  it('marks each step done by doing it, then shows "Approved!" once and ends', async () => {
    const user = userEvent.setup();
    const { api } = renderApp({ noDrafts: true });
    await findCard();

    // 1. ask for something
    await user.type(screen.getByLabelText(copy.home.askLabel), 'what changed in billing');
    await user.click(screen.getByRole('button', { name: copy.home.askButton }));
    await waitFor(() => expect(window.location.hash).toMatch(/^#\/a\//));
    await goHome(user);
    await findCard();
    expect(screen.getByText(copy.tour.progressLabel(1, 3))).toBeInTheDocument();
    expect(stepDone('ask')).toBe('true');

    // 2. open a draft: the step's own link goes to the practice draft
    await user.click(await screen.findByRole('link', { name: new RegExp(copy.tour.steps.open.action) }));
    await screen.findByText(copy.welcomeNoDrafts);
    await waitFor(async () => expect((await api.me()).hints_seen).toContain('tour_opened'));

    // 3. approve it: the one "Approved!" moment, with Nudge cheering
    await approvePractice(user);
    const outcome = screen.getByRole('status', { name: 'Outcome' });
    expect(within(outcome).getByText(copy.tour.cheerTitle)).toBeInTheDocument();
    expect(nudges(outcome)).toEqual(['yay']);
    await waitFor(async () => expect((await api.me()).hints_seen).toEqual(expect.arrayContaining(['tour_decided', 'tour_cheered'])));

    // back on Today the guided start is over
    await goHome(user);
    await waitFor(() => expect(card()).not.toBeInTheDocument());
  });

  it('shows "Approved!" only once: a later first approval is just an approval', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_jira', firstVisit: false, hintsSeen: ['tour_started', 'tour_cheered', 'sources', 'ask', 'approve'] });
    await user.click(await screen.findByRole('button', { name: 'Approve to Jira' }));
    await user.click(screen.getByRole('button', { name: copy.approve.confirm('jira_issue') }));
    await screen.findByRole('status', { name: 'Outcome' });
    expect(screen.queryByText(copy.tour.cheerTitle)).not.toBeInTheDocument();
  });

  it('gives a returning person who approves no "Approved!" moment', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_jira', firstVisit: false, hintsSeen: ['sources', 'ask', 'approve'] });
    await user.click(await screen.findByRole('button', { name: 'Approve to Jira' }));
    await user.click(screen.getByRole('button', { name: copy.approve.confirm('jira_issue') }));
    await screen.findByRole('status', { name: 'Outcome' });
    expect(screen.queryByText(copy.tour.cheerTitle)).not.toBeInTheDocument();
  });

  it('keeps progress across a reload, because it comes from the server', async () => {
    renderApp({ firstVisit: false, hintsSeen: ['tour_started', 'tour_asked'] });
    await findCard();
    expect(screen.getByText(copy.tour.progressLabel(1, 3))).toBeInTheDocument();
    expect(stepDone('ask')).toBe('true');
    expect(stepDone('open')).toBe('false');
    // the next step offers its way in, to the practice draft
    expect(await screen.findByRole('link', { name: new RegExp(copy.tour.steps.open.action) })).toHaveAttribute('href', expect.stringMatching(/^#\/d\/prc_/));
  });

  it('a step done before its turn still counts', async () => {
    renderApp({ firstVisit: false, hintsSeen: ['tour_started', 'tour_decided'] });
    await findCard();
    expect(screen.getByText(copy.tour.progressLabel(1, 3))).toBeInTheDocument();
    expect(stepDone('decide')).toBe('true');
  });

  it('"Skip the tour" ends it at once, for good', async () => {
    const user = userEvent.setup();
    const { api } = renderApp({ noDrafts: true });
    await findCard();
    await user.click(screen.getByRole('button', { name: copy.tour.skip }));
    await waitFor(() => expect(card()).not.toBeInTheDocument());
    expect((await api.me()).hints_seen).toContain('tour_skipped');
    // and it does not come back by itself
    await user.click(navLink(copy.nav.drafts));
    await goHome(user);
    expect(card()).not.toBeInTheDocument();
  });

  it('"Show hints again" restarts it from step 1', async () => {
    const user = userEvent.setup();
    renderApp({ firstVisit: false, hintsSeen: ['tour_started', 'tour_asked', 'tour_skipped', 'sources'] });
    await screen.findByLabelText(copy.home.askLabel);
    expect(card()).not.toBeInTheDocument();
    await openMenu(user);
    await user.click(screen.getByRole('menuitem', { name: copy.menu.showHints }));
    await findCard();
    expect(screen.getByText(copy.tour.progressLabel(0, 3))).toBeInTheDocument();
  });

  it('a returning person sees no guided start', async () => {
    renderApp({ firstVisit: false });
    await screen.findByLabelText(copy.home.askLabel);
    await screen.findAllByRole('link', { name: /PRD: TCB grace period setting/ });
    expect(card()).not.toBeInTheDocument();
  });

  it('is hidden with "Hints and nudges" off', async () => {
    renderApp({ noDrafts: true, nudges: false });
    await screen.findByLabelText(copy.home.askLabel);
    await screen.findByRole('link', { name: /Practice: why PMs should take the stairs/ });
    expect(card()).not.toBeInTheDocument();
  });
});

describe('the guided start for each role', () => {
  it('points a reviewer at Reviews', async () => {
    renderApp({ role: 'reviewer', firstVisit: true });
    const welcome = await findCard('Ben');
    expect(within(welcome).getByText(copy.tour.reviewer.text)).toBeInTheDocument();
    expect(within(welcome).getByRole('link', { name: copy.tour.reviewer.action })).toHaveAttribute('href', '#/reviews');
    expect(within(welcome).getAllByRole('listitem')).toHaveLength(3);
  });

  it('gives an admin one extra pointer, to the Team page', async () => {
    renderApp({ role: 'admin', firstVisit: true });
    const welcome = await findCard('Ada');
    expect(within(welcome).getByRole('link', { name: copy.tour.admin.action })).toHaveAttribute('href', '#/admin/team');
    expect(within(welcome).queryByText(copy.tour.reviewer.text)).not.toBeInTheDocument();
  });

  it('gives an auditor a one-line, read-only welcome with no steps', async () => {
    const user = userEvent.setup();
    renderApp({ role: 'auditor', firstVisit: true });
    const line = await screen.findByRole('region', { name: copy.tour.label });
    expect(within(line).getByText(copy.tour.auditor.text)).toBeInTheDocument();
    expect(within(line).queryByRole('list')).not.toBeInTheDocument();
    expect(within(line).queryByRole('button', { name: copy.tour.skip })).not.toBeInTheDocument();
    await user.click(within(line).getByRole('button', { name: copy.tour.auditor.dismiss }));
    await waitFor(() => expect(screen.queryByRole('region', { name: copy.tour.label })).not.toBeInTheDocument());
  });
});

describe('Nudge around the app', () => {
  it('sits beside each one-time hint, with the same text', async () => {
    renderApp({ hash: '/d/apr_prd' });
    await screen.findByText(copy.hints.sources);
    const hint = document.querySelector('[data-hint="sources"]') as HTMLElement;
    expect(nudges(hint)).toEqual(['nudge']);
  });

  it('is calm in an empty state, and the line is the same', async () => {
    renderApp({ firstVisit: false, emptyBrief: true });
    await screen.findByText(copy.nothingWaiting);
    const empty = screen.getByText(copy.nothingWaiting).closest('.empty') as HTMLElement;
    expect(nudges(empty)).toEqual(['calm']);
  });

  it('is not on Today twice: the calm one waits while the welcome card is up', async () => {
    renderApp({ firstVisit: true, emptyBrief: true });
    await findCard();
    await screen.findByText(copy.nothingWaiting);
    expect(nudges()).toEqual(['hello']);
  });

  it('waves, small, on the sign-in screen, and is not on the signed-out page', async () => {
    const first = renderApp({ signedIn: false });
    await screen.findByLabelText(copy.signIn.handleLabel);
    expect(nudges()).toEqual(['hello']);
    first.unmount();
    renderApp({ signedIn: false, hash: '/signed-out?reason=not_a_member' });
    await screen.findByText(copy.signedOut.reasons.not_a_member!);
    expect(nudges()).toEqual([]);
  });

  it('is not shown in error states', async () => {
    renderApp({ hash: '/d/apr_missing' });
    await screen.findByText(copy.errors.notFound);
    expect(nudges()).toEqual([]);
  });
});

describe('Show Nudge', () => {
  it('off removes the character everywhere, and every text stays', async () => {
    const user = userEvent.setup();
    const { api } = renderApp({ noDrafts: true, emptyBrief: true });
    await findCard();
    expect(nudges().length).toBeGreaterThan(0);

    await openMenu(user);
    const toggle = screen.getByRole('menuitemcheckbox', { name: new RegExp(copy.menu.mascot) });
    expect(toggle).toHaveAttribute('aria-checked', 'true');
    await user.click(toggle);
    await waitFor(() => expect(toggle).toHaveAttribute('aria-checked', 'false'));
    expect((await api.me()).mascot).toBe(false);
    await user.keyboard('{Escape}');

    // the welcome card and its steps are all still there, without the picture
    const welcome = await findCard();
    expect(within(welcome).getByText(copy.tour.lede)).toBeInTheDocument();
    expect(nudges()).toEqual([]);
    // an empty state keeps its line and its plain icon
    await user.click(navLink(copy.nav.drafts));
    await screen.findByRole('tab', { name: /Needs you/ });
    expect(nudges()).toEqual([]);
    // a hint keeps its text
    await goHome(user);
    await user.click(await screen.findByRole('link', { name: /Practice: why PMs should take the stairs/ }));
    await screen.findByText(copy.hints.sources);
    expect(nudges()).toEqual([]);
  });

  it('off keeps hints, with their text, on the draft page', async () => {
    renderApp({ hash: '/d/apr_prd', mascot: false });
    await screen.findByText(copy.hints.sources);
    expect(nudges()).toEqual([]);
  });

  it('turns back on from the same menu', async () => {
    const user = userEvent.setup();
    renderApp({ firstVisit: false, emptyBrief: true, mascot: false });
    await screen.findByText(copy.nothingWaiting);
    expect(nudges()).toEqual([]);
    await openMenu(user);
    await user.click(screen.getByRole('menuitemcheckbox', { name: new RegExp(copy.menu.mascot) }));
    await user.keyboard('{Escape}');
    await waitFor(() => expect(nudges()).toEqual(['calm']));
  });

  it('is separate from "Hints and nudges": turning Nudge off leaves hints on', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd' });
    await screen.findByText(copy.hints.sources);
    await openMenu(user);
    await user.click(screen.getByRole('menuitemcheckbox', { name: new RegExp(copy.menu.mascot) }));
    await user.keyboard('{Escape}');
    await waitFor(() => expect(nudges()).toEqual([]));
    expect(screen.getByText(copy.hints.sources)).toBeInTheDocument();
  });

  it('off still shows the "Approved!" words, without the picture', async () => {
    const user = userEvent.setup();
    renderApp({ noDrafts: true, mascot: false });
    await user.click(await screen.findByRole('link', { name: /Practice: why PMs should take the stairs/ }));
    await approvePractice(user);
    expect(screen.getByText(copy.tour.cheerTitle)).toBeInTheDocument();
    expect(nudges()).toEqual([]);
  });
});
