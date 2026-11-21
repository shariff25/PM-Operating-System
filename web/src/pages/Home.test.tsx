import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import type { Task } from '../api';
import { MOCK_CATALOG } from '../api/mockBots';
import { copy } from '../copy';
import { renderApp } from '../test/util';

const ask = async (user: ReturnType<typeof userEvent.setup>, text: string) => {
  await user.type(await screen.findByLabelText(copy.home.askLabel), text);
  await user.keyboard('{Enter}');
};

describe('home launchpad', () => {
  it('asks with Enter and opens the request as a thread with the result as rendered markdown', async () => {
    const user = userEvent.setup();
    renderApp();
    await ask(user, 'what changed in billing');
    await screen.findByText(copy.task.status.done);
    expect(window.location.hash).toMatch(/^#\/a\/tsk_/);
    const thread = screen.getByRole('region', { name: copy.activity.threadLabel });
    expect(within(thread).getByText('what changed in billing', { selector: '.msg-bubble' })).toBeInTheDocument();
    expect(within(thread).getByText('PAY-212').tagName).toBe('STRONG');
    expect(within(thread).getAllByRole('listitem').length).toBeGreaterThan(2);
  });

  it('adds a new line with Shift+Enter instead of sending', async () => {
    const user = userEvent.setup();
    renderApp();
    const box = await screen.findByLabelText(copy.home.askLabel);
    await user.type(box, 'first line{Shift>}{Enter}{/Shift}second line');
    expect(box).toHaveValue('first line\nsecond line');
    expect(window.location.hash).not.toMatch(/^#\/a\//);
  });

  it('fills the composer from a suggestion chip', async () => {
    const user = userEvent.setup();
    renderApp();
    // The chips are the selected bot's examples: Ana last asked Chief.
    const examples = MOCK_CATALOG.find((b) => b.key === 'chief')!.examples;
    const chips = await screen.findByRole('list', { name: copy.launch.suggestionsLabel });
    await waitFor(() => expect(within(chips).getAllByRole('button')).toHaveLength(3));
    await user.click(await within(chips).findByRole('button', { name: examples[2] }));
    expect(screen.getByLabelText(copy.home.askLabel)).toHaveValue(examples[2]);
    expect(screen.getByLabelText(copy.home.askLabel)).toHaveFocus();
  });

  it('shows the app\'s own suggestions while the catalog can\'t be read', async () => {
    const user = userEvent.setup();
    renderApp({ catalog: 'unavailable' });
    const chips = await screen.findByRole('list', { name: copy.launch.suggestionsLabel });
    expect(within(chips).getAllByRole('button')).toHaveLength(3);
    await user.click(within(chips).getByRole('button', { name: copy.launch.suggestions[2] }));
    expect(screen.getByLabelText(copy.home.askLabel)).toHaveValue(copy.launch.suggestions[2]);
  });

  it('says what needs the PM, then shows the numbers, the decisions and who is waiting', async () => {
    renderApp();
    // three drafts to decide and two people waiting
    await screen.findByRole('heading', { level: 1, name: /Five things need you\.$/ });
    const stats = screen.getByRole('region', { name: copy.today.statsLabel });
    for (const label of Object.values(copy.today.stats).slice(0, 4)) expect(stats).toHaveTextContent(label);
    const decide = screen.getByRole('region', { name: copy.today.decideHeading });
    const links = within(decide).getAllByRole('link').filter((l) => l.getAttribute('href')?.startsWith('#/d/'));
    expect(links).toHaveLength(3);
    expect(links.some((l) => l.textContent?.includes('PRD: TCB grace period setting'))).toBe(true);
    expect(within(decide).getByRole('link', { name: new RegExp(copy.today.allDrafts) })).toHaveAttribute('href', '#/drafts');
    const waiting = screen.getByRole('region', { name: copy.today.waitingHeading });
    await within(waiting).findByText('asked about the pricing page', { exact: false });
    expect(within(waiting).getAllByRole('button', { name: /Draft a reply/ })).toHaveLength(2);
    expect(screen.getByRole('region', { name: copy.today.changedHeading })).toHaveTextContent('PAY-212 moved to Blocked');
  });

  it('counts in words up to ten', () => {
    expect(copy.today.needs(0)).toBe('Nothing needs you right now.');
    expect(copy.today.needs(1)).toBe('One thing needs you.');
    expect(copy.today.needs(3)).toBe('Three things need you.');
    expect(copy.today.needs(10)).toBe('Ten things need you.');
    expect(copy.today.needs(12)).toBe('12 things need you.');
  });

  it('shows a running request with its progress at the foot of the decisions', async () => {
    const running: Task = {
      id: 'tsk_run', bot: 'pm-ana', text: 'weekly launch summary', status: 'working', result: null, error: null, draft_ids: [],
      created_at: new Date().toISOString(), updated_at: new Date().toISOString(), progress: { done: 2, total: 3 },
    };
    renderApp({
      wrap: (api) => {
        const list = api.listTasks.bind(api);
        api.listTasks = async () => [running, ...(await list())];
        return api;
      },
    });
    const decide = await screen.findByRole('region', { name: copy.today.decideHeading });
    const row = await within(decide).findByRole('link', { name: new RegExp(copy.today.working('weekly launch summary')) });
    expect(row).toHaveAttribute('href', '#/a/tsk_run');
    expect(within(row).getByRole('progressbar', { name: copy.task.progressLabel })).toHaveAttribute('aria-valuenow', '2');
  });

  it('drops the follow-up cards and the Follow-ups page when the brief answers 501 not_built', async () => {
    renderApp({ briefBuilt: false });
    const decide = await screen.findByRole('region', { name: copy.today.decideHeading });
    await within(decide).findByText('PRD: TCB grace period setting');
    expect(screen.queryByRole('region', { name: copy.today.waitingHeading })).not.toBeInTheDocument();
    expect(screen.queryByRole('region', { name: copy.today.changedHeading })).not.toBeInTheDocument();
    const stats = screen.getByRole('region', { name: copy.today.statsLabel });
    expect(stats).toHaveTextContent(copy.today.stats.running);
    expect(stats).not.toHaveTextContent(copy.today.stats.waiting);
    await screen.findByRole('heading', { level: 1, name: /Three things need you\.$/ });
    expect(screen.queryByRole('link', { name: new RegExp(`^${copy.nav.followups}`) })).not.toBeInTheDocument();
  });

  it('lists recent activity with its status and opens a request from it', async () => {
    const user = userEvent.setup();
    renderApp();
    const recent = await screen.findByRole('list', { name: copy.launch.recent });
    const row = await within(recent).findByRole('link', { name: /What changed in billing this week/ });
    expect(row).toHaveTextContent(copy.activity.status.done);
    await user.click(row);
    await screen.findByText('Billing this week');
    expect(window.location.hash).toBe('#/a/tsk_seed_week');
  });

  it('keeps the task id out of the thread header and shows it only under the steps', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/a/tsk_seed_week' });
    await screen.findByText('Billing this week');
    expect(screen.queryByText('tsk_seed_week')).not.toBeInTheDocument();
    await user.click(await screen.findByRole('button', { name: new RegExp(copy.activity.showSteps) }));
    expect(screen.getByText('tsk_seed_week')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: copy.activity.copyReference })).toBeInTheDocument();
  });

  it('greets a first sign-in that already has drafts with the guided start', async () => {
    // Nothing offered besides Nudge: the three-step start (the four-step one is in Market.test.tsx).
    renderApp({ firstVisit: true, catalog: 'nudge_only' });
    expect(await screen.findByRole('heading', { name: copy.tour.hello('Ana') })).toBeInTheDocument();
    expect(screen.getByText(copy.tour.lede)).toBeInTheDocument();
  });

  it('shows the empty states when there are no drafts and nothing asked yet', async () => {
    renderApp({ noDrafts: true, firstVisit: false });
    await screen.findByText(copy.emptyDrafts);
    await screen.findByText(copy.launch.noActivity);
    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent(copy.today.needs(2)); // two people still wait in the brief
  });

  it('shows the sign-in page when signed out, and signs in with a handle', async () => {
    const user = userEvent.setup();
    renderApp({ signedIn: false });
    await user.type(await screen.findByLabelText(copy.signIn.handleLabel), 'nobody');
    await user.click(screen.getByRole('button', { name: copy.signIn.submit }));
    expect(await screen.findByRole('alert')).toHaveTextContent(/not on the team/);
    await user.clear(screen.getByLabelText(copy.signIn.handleLabel));
    await user.type(screen.getByLabelText(copy.signIn.handleLabel), 'ana');
    await user.click(screen.getByRole('button', { name: copy.signIn.submit }));
    await screen.findByLabelText(copy.home.askLabel);
  });
});

describe('thread', () => {
  it('shows only milestone events in the steps, never queued, claimed, started or counts', async () => {
    const user = userEvent.setup();
    renderApp();
    await ask(user, 'what changed in billing');
    await screen.findByText(copy.task.status.done);
    await user.click(screen.getByRole('button', { name: new RegExp(copy.activity.showSteps) }));
    const steps = screen.getByRole('list', { name: copy.activity.stepsLabel });
    expect(within(steps).getAllByRole('listitem').map((li) => li.textContent)).toEqual([
      'Reading Jira',
      'Reading Confluence',
      'Putting the answer together',
    ]);
    for (const hidden of ['Queued', 'Claimed by pm-ana', 'Started', '1 of 3 done', '2 of 3 done']) {
      expect(screen.queryByText(hidden)).not.toBeInTheDocument();
    }
  });

  it('shows one live status line with the latest milestone and the progress while working', async () => {
    const working: Task = {
      id: 'tsk_live',
      bot: 'pm-ana',
      text: 'what is blocked',
      status: 'working',
      result: null,
      error: null,
      draft_ids: [],
      created_at: new Date().toISOString(),
      updated_at: new Date().toISOString(),
      progress: { done: 2, total: 3 },
      retrying: false,
      events: [
        { at: '2026-11-16T09:00:01Z', kind: 'milestone', text: 'Reading Jira' },
        { at: '2026-11-16T09:00:03Z', kind: 'milestone', text: 'Drafting in Confluence' },
      ],
    };
    renderApp({
      hash: '/a/tsk_live',
      wrap: (api) => {
        api.getTask = async () => structuredClone(working);
        return api;
      },
    });
    await waitFor(() => expect(document.querySelector('.live-line')).toHaveTextContent('Drafting in Confluence'));
    expect(document.querySelector('.live-line')).toHaveAttribute('role', 'status');
    expect(screen.getAllByRole('status').filter((s) => s.classList.contains('live-line'))).toHaveLength(1);
    expect(document.querySelector('.live-line')).toHaveTextContent('2 of 3');
    const bar = screen.getByRole('progressbar', { name: copy.task.progressLabel });
    expect(bar).toHaveAttribute('aria-valuenow', '2');
    expect(bar).toHaveAttribute('aria-valuemax', '3');
    expect(screen.queryByText(copy.modelBusy)).not.toBeInTheDocument();
  });

  it('shows no bar and no count when the task has no progress, and never "0 of 0"', async () => {
    const base: Task = {
      id: 'tsk_live', bot: 'pm-ana', text: 'what is blocked', status: 'working', result: null, error: null, draft_ids: [],
      created_at: new Date().toISOString(), updated_at: new Date().toISOString(), progress: { done: 0, total: 0 }, events: [],
    };
    renderApp({ hash: '/a/tsk_live', wrap: (api) => { api.getTask = async () => structuredClone(base); return api; } });
    await waitFor(() => expect(document.querySelector('.live-line')).toHaveTextContent(copy.task.status.working));
    expect(screen.queryByRole('progressbar')).not.toBeInTheDocument();
    expect(screen.queryByText('0 of 0')).not.toBeInTheDocument();
  });

  it('says the model is catching its breath only while retrying is true', async () => {
    let retrying = true;
    const base: Task = {
      id: 'tsk_live', bot: 'pm-ana', text: 'what is blocked', status: 'working', result: null, error: null, draft_ids: [],
      created_at: new Date().toISOString(), updated_at: new Date().toISOString(), progress: null, events: [],
    };
    renderApp({
      hash: '/a/tsk_live',
      wrap: (api) => {
        api.getTask = async () => structuredClone({ ...base, retrying });
        return api;
      },
    });
    await screen.findByText(copy.modelBusy);
    retrying = false;
    await waitFor(() => expect(screen.queryByText(copy.modelBusy)).not.toBeInTheDocument());
  });

  it('keeps the same space for the status block when a bar and a retry note appear', async () => {
    // The note and the bar live in reserved rows, so they exist before they have content.
    const base: Task = {
      id: 'tsk_live', bot: 'pm-ana', text: 'q', status: 'working', result: null, error: null, draft_ids: [],
      created_at: new Date().toISOString(), updated_at: new Date().toISOString(), progress: null, events: [],
    };
    renderApp({ hash: '/a/tsk_live', wrap: (api) => { api.getTask = async () => structuredClone(base); return api; } });
    await waitFor(() => expect(document.querySelector('.live-sub')).not.toBeNull());
    expect(document.querySelector('.live-sub .progress')).not.toBeNull();
    expect(document.querySelector('.live-sub .live-note')).not.toBeNull();
  });

  it('shows an empty state with Try again when a task finished with nothing to show', async () => {
    const done: Task = {
      id: 'tsk_none', bot: 'pm-ana', text: 'q', status: 'done', result: null, error: null, draft_ids: [],
      created_at: new Date().toISOString(), updated_at: new Date().toISOString(), progress: null, events: [],
    };
    renderApp({ hash: '/a/tsk_none', wrap: (api) => { api.getTask = async () => structuredClone(done); return api; } });
    await screen.findByText(copy.task.noResult);
    expect(screen.getByRole('button', { name: copy.task.tryAgain })).toBeInTheDocument();
  });

  it('shows a produced draft as a card with its title, kind and Open draft', async () => {
    const user = userEvent.setup();
    renderApp();
    await ask(user, 'draft a PRD for grace periods');
    await screen.findByText(copy.task.status.needs_approval);
    const card = (await screen.findByText('PRD: grace periods')).closest('.draft-card') as HTMLElement;
    expect(within(card).getByText(copy.home.kindLabel('confluence_page'))).toBeInTheDocument();
    await user.click(within(card).getByRole('button', { name: copy.activity.openDraft }));
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(window.location.hash).toMatch(/^#\/d\/apr_/);
  });

  it('says plainly why a task failed and offers Try again', async () => {
    const user = userEvent.setup();
    renderApp();
    await ask(user, 'this will fail');
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(/I could not reach Jira just now/);
    const failedHash = window.location.hash;
    await user.click(within(alert).getByRole('button', { name: copy.task.tryAgain }));
    await screen.findByText(copy.task.status.done);
    expect(window.location.hash).not.toBe(failedHash);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('is reachable from Activity', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/activity' });
    const list = await screen.findByRole('list', { name: copy.activity.title });
    await user.click(await within(list).findByRole('link', { name: /Summarize the DPA thread/ }));
    expect(await screen.findByRole('alert')).toHaveTextContent(/could not reach Slack/);
  });

  it('says so when a request does not exist', async () => {
    renderApp({ hash: '/a/tsk_nope' });
    await screen.findByText(copy.activity.notFound);
  });
});

describe('drafts page', () => {
  const tab = (name: string) => screen.getByRole('tab', { name: new RegExp(`^${name}`) });

  it('filters with tabs that show counts', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/drafts' });
    const tabs = await screen.findByRole('tablist', { name: copy.draftsPage.tabsLabel });
    await within(tabs).findByText('3');
    expect(tab(copy.draftsPage.tabs.pending)).toHaveAttribute('aria-selected', 'true');
    expect(tab(copy.draftsPage.tabs.pending)).toHaveTextContent('3');
    expect(tab(copy.draftsPage.tabs.approved)).toHaveTextContent('1');
    expect(tab(copy.draftsPage.tabs.expired)).toHaveTextContent('1');
    expect(tab(copy.draftsPage.tabs.all)).toHaveTextContent('5');

    const panel = () => screen.getByRole('tabpanel');
    expect(within(panel()).getAllByRole('link')).toHaveLength(3);
    await user.click(tab(copy.draftsPage.tabs.expired));
    expect(within(panel()).getAllByRole('link')).toHaveLength(1);
    expect(panel()).toHaveTextContent('Direct message');

    // arrow keys move between the tabs and select them
    tab(copy.draftsPage.tabs.expired).focus();
    await user.keyboard('{ArrowRight}');
    expect(tab(copy.draftsPage.tabs.all)).toHaveAttribute('aria-selected', 'true');
    expect(tab(copy.draftsPage.tabs.all)).toHaveFocus();
    await user.keyboard('{Home}');
    expect(tab(copy.draftsPage.tabs.pending)).toHaveAttribute('aria-selected', 'true');
  });

  it('lists drafts that need a decision first, then expired ones, then decided ones', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/drafts' });
    await screen.findByText('PRD: TCB grace period setting');
    await user.click(tab(copy.draftsPage.tabs.all));
    const links = within(screen.getByRole('tabpanel')).getAllByRole('link');
    const badges = links.map((l) => l.querySelector('.pill')!.textContent);
    expect(badges).toEqual(['Needs you', 'Needs you', 'Needs you', 'Expired', 'Approved']);
    expect(links[3]).toHaveTextContent('Direct message');
    expect(links[4]).toHaveTextContent('Weekly update');
    expect(within(screen.getByRole('tabpanel')).getByText(/\d+ h left/)).toBeInTheDocument();
  });

  it('labels a draft whose time has passed as Expired even while the server still says pending', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/drafts' });
    await screen.findByText('PRD: TCB grace period setting');
    expect(screen.queryByText('Direct message')).not.toBeInTheDocument();
    await user.click(tab(copy.draftsPage.tabs.expired));
    const row = screen.getByText('Direct message').closest('a')!;
    expect(within(row).getByText('Expired')).toHaveClass('pill-expired');
    expect(within(row).queryByText('Needs you')).not.toBeInTheDocument();
  });

  it('shows the light empty state for an empty filter', async () => {
    renderApp({ hash: '/drafts', noDrafts: true, firstVisit: false });
    await screen.findByText(copy.emptyDrafts);
  });
});

describe('follow-ups page', () => {
  it('shows both lists, and drafting a reply opens the request with its draft', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/followups' });
    await screen.findByRole('heading', { name: copy.home.waitingOnYou });
    expect(screen.getByRole('heading', { name: copy.home.youWaitOn })).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: copy.home.changed })).toBeInTheDocument();
    await user.click(screen.getAllByRole('button', { name: /Draft a reply/ })[0]!);
    await screen.findByRole('button', { name: copy.activity.openDraft });
    expect(screen.getByText(/I made a draft for you/)).toBeInTheDocument();
    expect(window.location.hash).toMatch(/^#\/a\//);
  });

  it('removes a follow-up with Not mine', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/followups' });
    await screen.findByText('asked about the pricing page');
    await user.click(screen.getAllByRole('button', { name: /Not mine/ })[0]!);
    await waitFor(() => expect(screen.queryByText('asked about the pricing page')).not.toBeInTheDocument());
    expect(screen.getByText(copy.toast.dismissed)).toBeInTheDocument();
  });

  it('shows only http and https follow-up links, and plain text for anything else', async () => {
    const user = userEvent.setup();
    renderApp({
      hash: '/followups',
      wrap: (api) => {
        const orig = api.brief.bind(api);
        api.brief = async () => {
          const b = await orig();
          b.waiting_on_you[0]!.url = 'javascript:alert(1)';
          b.changed[0]!.url = 'data:text/html,x';
          return b;
        };
        return api;
      },
    });
    await screen.findByText('asked about the pricing page');
    expect(document.querySelector('a[href^="javascript:"], a[href^="data:"]')).toBeNull();
    expect(screen.getAllByText(/not a web link/).length).toBe(2);
    await user.click(screen.getAllByRole('button', { name: /Not mine/ })[0]!);
  });
});
