import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { ApiError } from '../api';
import type { Api, Bots, TeamChange } from '../api';
import { ECHO_LINE } from '../api/mockBots';
import { copy } from '../copy';
import { renderApp } from '../test/util';

// The bot marketplace's screens (slices F2 and F3): "Pick your team" in the guided start, the note
// for people who were here before, the Bots page, the picker in the ask box, and attribution.

const B = copy.bots;
const Pk = B.pick;

type User = ReturnType<typeof userEvent.setup>;

/** Record every team change and every mark, and pass them on. */
function spy(record: { changes: TeamChange[]; marks: string[]; asks: [string, string | undefined][] }) {
  return (api: Api) => {
    const change = api.changeTeam.bind(api);
    const mark = api.markHintSeen.bind(api);
    const ask = api.createTask.bind(api);
    api.changeTeam = (c) => {
      record.changes.push(structuredClone(c));
      return change(c);
    };
    api.markHintSeen = (m) => {
      record.marks.push(m);
      return mark(m);
    };
    api.createTask = (text, persona) => {
      record.asks.push([text, persona]);
      return ask(text, persona);
    };
    return api;
  };
}
const fresh = () => ({ changes: [] as TeamChange[], marks: [] as string[], asks: [] as [string, string | undefined][] });

const findStep = () => screen.findByRole('heading', { level: 1, name: Pk.title });
const card = (name: string) => screen.getByRole('listitem', { name });
const navLink = (name: string) => screen.queryAllByRole('link', { name: new RegExp(`^${name}`) })[0] ?? null;
const goHome = async (user: User) => {
  await user.click(screen.getAllByRole('link', { name: copy.nav.home })[0]!);
  await screen.findByLabelText(copy.home.askLabel);
};

describe('Pick your team: the first step of the guided start', () => {
  it('a first visit goes to the step once: step 1 of 4, Nudge always on, the others to add', async () => {
    renderApp({ noDrafts: true });
    expect(await findStep()).toBeInTheDocument();
    expect(window.location.hash).toBe('#/start/team');
    expect(screen.getByText(`${Pk.eyebrow} · ${Pk.step(1, 4)}`)).toBeInTheDocument();
    const nudge = card('Nudge');
    expect(within(nudge).getByRole('button', { name: `Nudge: ${B.alwaysOnTeam}` })).toBeDisabled();
    const chief = card('Chief');
    expect(within(chief).getByText('Chief of Staff')).toHaveClass('bot-role-chief');
    expect(within(chief).getByText(`${B.preview}`, { exact: false })).toBeInTheDocument();
    expect(within(chief).getByRole('button', { name: `Chief: ${Pk.add}` })).toHaveAttribute('aria-pressed', 'false');
    // Sprout needs a source PM OS doesn't have; it can still be added
    const sprout = card('Sprout');
    const need = within(sprout).getByText(B.needs('an analytics source'));
    expect(need).toHaveAccessibleDescription('I work from numbers you paste in or that a Confluence page holds, and I say so.');
    expect(within(sprout).getByRole('button', { name: `Sprout: ${Pk.add}` })).toBeEnabled();
    await waitFor(() => expect(screen.getByRole('button', { name: Pk.continue(1) })).toBeEnabled());
  });

  it('Continue saves the chosen bots with via "onboarding", then marks team_picked and returns to step 2 of 4', async () => {
    const user = userEvent.setup();
    const rec = fresh();
    renderApp({ noDrafts: true, wrap: spy(rec) });
    await findStep();
    const toggle = within(card('Chief')).getByRole('button', { name: `Chief: ${Pk.add}` });
    await user.click(toggle);
    expect(within(card('Chief')).getByRole('button', { name: `Chief: ${Pk.added}` })).toHaveAttribute('aria-pressed', 'true');
    // the keyboard works the toggles too
    within(card('Echo')).getByRole('button', { name: `Echo: ${Pk.add}` }).focus();
    await user.keyboard(' ');
    expect(within(card('Echo')).getByRole('button', { name: `Echo: ${Pk.added}` })).toHaveAttribute('aria-pressed', 'true');
    await user.keyboard('{Enter}');
    expect(within(card('Echo')).getByRole('button', { name: `Echo: ${Pk.add}` })).toHaveAttribute('aria-pressed', 'false');
    expect(screen.getByText(Pk.footNames(['Nudge', 'Chief']))).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: Pk.continue(2) }));
    await screen.findByLabelText(copy.home.askLabel);
    expect(rec.changes).toEqual([{ add: ['chief'], via: 'onboarding' }]);
    expect(rec.marks).toContain('team_picked');
    expect(rec.marks).not.toContain('team_skipped');
    const welcome = await screen.findByRole('region', { name: copy.tour.hello('Ana') });
    expect(within(welcome).getByText(copy.tour.progressLabel(1, 4))).toBeInTheDocument();
    expect(within(welcome).getByText(copy.tour.ledeFour)).toBeInTheDocument();
    expect(document.querySelector('[data-step="team"]')).toHaveAttribute('data-done', 'true');
    // the picker now offers Chief
    expect(await screen.findByRole('button', { name: B.picker.button('Nudge') })).toBeInTheDocument();
  });

  it('"Skip for now" marks team_skipped only, and never blocks the rest of the start', async () => {
    const user = userEvent.setup();
    const rec = fresh();
    renderApp({ noDrafts: true, wrap: spy(rec) });
    await findStep();
    await user.click(screen.getByRole('button', { name: Pk.skip }));
    await screen.findByLabelText(copy.home.askLabel);
    expect(rec.marks.filter((m) => m.startsWith('team_'))).toEqual(['team_skipped']);
    expect(rec.changes).toEqual([]);
    expect(window.location.hash).not.toBe('#/start/team');
    const welcome = await screen.findByRole('region', { name: copy.tour.hello('Ana') });
    expect(within(welcome).getByText(copy.tour.progressLabel(1, 4))).toBeInTheDocument();
  });

  it('leaving without picking is not blocked: Today sends you once, then points at the step', async () => {
    const user = userEvent.setup();
    renderApp({ noDrafts: true });
    await findStep();
    await goHome(user);
    expect(window.location.hash).toBe('#/');
    const welcome = await screen.findByRole('region', { name: copy.tour.hello('Ana') });
    expect(within(welcome).getByText(copy.tour.progressLabel(0, 4))).toBeInTheDocument();
    await user.click(within(welcome).getByRole('link', { name: new RegExp(copy.tour.steps.team.action) }));
    expect(await findStep()).toBeInTheDocument();
  });

  it('a failed save keeps the page, says so, and marks nothing', async () => {
    const user = userEvent.setup();
    const rec = fresh();
    renderApp({ noDrafts: true, teamError: 'rate_limited', wrap: spy(rec) });
    await findStep();
    await user.click(within(card('Chief')).getByRole('button', { name: `Chief: ${Pk.add}` }));
    await user.click(screen.getByRole('button', { name: Pk.continue(2) }));
    expect(await screen.findByText(B.errors.codes.rate_limited!)).toBeInTheDocument();
    expect(window.location.hash).toBe('#/start/team');
    expect(rec.marks.filter((m) => m.startsWith('team_'))).toEqual([]);
    // Skip still works
    await user.click(screen.getByRole('button', { name: Pk.skip }));
    await screen.findByLabelText(copy.home.askLabel);
  });

  it('never for a returning person, an auditor, or where nothing besides Nudge is offered', async () => {
    const returning = renderApp({ firstVisit: false });
    await screen.findByLabelText(copy.home.askLabel);
    expect(window.location.hash).toBe('');
    returning.unmount();

    const auditor = renderApp({ role: 'auditor', firstVisit: true });
    await screen.findByRole('region', { name: copy.tour.label });
    expect(window.location.hash).toBe('');
    expect(navLink(copy.nav.bots)).toBeNull();
    auditor.unmount();

    renderApp({ noDrafts: true, catalog: 'nudge_only' });
    await screen.findByRole('region', { name: copy.tour.hello('Ana') });
    expect(window.location.hash).toBe('');
  });

  it('a reviewer or an admin who may ask gets the step too', async () => {
    renderApp({ role: 'reviewer', firstVisit: true });
    expect(await findStep()).toBeInTheDocument();
  });

  it('renders catalog text as plain text, never as markup', async () => {
    const hostile = '<img src=x onerror="alert(1)"> **bold**';
    renderApp({
      noDrafts: true,
      wrap: (api) => {
        const bots = api.bots.bind(api);
        api.bots = async () => {
          const b: Bots = await bots();
          b.bots = b.bots.map((c) => (c.key === 'chief' ? { ...c, summary: hostile, examples: [hostile] } : c));
          return b;
        };
        return api;
      },
    });
    await findStep();
    expect(within(card('Chief')).getByText(hostile)).toBeInTheDocument();
    expect(document.querySelector('.pick-grid img')).toBeNull();
    expect(document.querySelector('.pick-grid strong')).toBeNull();
  });
});

describe('the note for people who were here before', () => {
  it('shows once, links to Bots, and its dismiss marks team_announced', async () => {
    const user = userEvent.setup();
    const rec = fresh();
    renderApp({ firstVisit: false, team: [], wrap: spy(rec) });
    const note = await screen.findByRole('region', { name: B.note.title });
    expect(await within(note).findByText(B.note.text(['Chief', 'Sprout', 'Echo']))).toBeInTheDocument();
    expect(within(note).getByRole('link', { name: B.note.action })).toHaveAttribute('href', '#/bots');
    await user.click(within(note).getByRole('button', { name: B.note.dismiss }));
    await waitFor(() => expect(screen.queryByRole('region', { name: B.note.title })).not.toBeInTheDocument());
    expect(rec.marks).toEqual(['team_announced']);
  });

  it('is not shown to someone who already added a bot, nor with the step', async () => {
    const one = renderApp({ firstVisit: false });
    await screen.findByLabelText(copy.home.askLabel);
    expect(screen.queryByRole('region', { name: B.note.title })).not.toBeInTheDocument();
    one.unmount();
    renderApp({ noDrafts: true });
    await findStep();
    expect(screen.queryByRole('region', { name: B.note.title })).not.toBeInTheDocument();
  });
});

const findTeamList = async () => {
  const h = await screen.findByRole('heading', { level: 2, name: new RegExp(`^${B.page.onTeam}`) });
  return h.closest('section')!;
};
const availableList = () => screen.getByRole('heading', { level: 2, name: new RegExp(`^${B.page.available}`) }).closest('section')!;

describe('the Bots page', () => {
  it('is in the menu for people who may ask, and lists the team, what is available, and the details', async () => {
    renderApp({ hash: '/bots' });
    const team = await findTeamList();
    expect(navLink(copy.nav.bots)).toHaveAttribute('href', '#/bots');
    expect(within(team).getByText(B.alwaysOn)).toBeInTheDocument();
    const chiefRow = within(team).getByRole('button', { name: B.page.details('Chief') }).closest('li')!;
    expect(within(chiefRow).getByRole('button', { name: B.page.settingsLabel('Chief') })).toBeInTheDocument();
    expect(within(chiefRow).getByRole('button', { name: B.page.removeLabel('Chief') })).toBeInTheDocument();
    expect(within(chiefRow).getByText(B.preview)).toBeInTheDocument();
    const avail = availableList();
    expect(within(avail).getByText(B.needs('an analytics source'))).toBeInTheDocument();
    expect(within(avail).getAllByText(B.previewNote, { exact: false }).length).toBeGreaterThan(0);
    // the details of the first bot on the team besides Nudge
    const detail = screen.getByRole('complementary', { name: 'Chief' });
    expect(within(detail).getByText(B.page.draftsSentence(['Jira issues', 'Confluence pages', 'Slack replies']))).toBeInTheDocument();
    expect(within(detail).getByText('Meeting prep')).toBeInTheDocument();
  });

  it('shows a bot\'s details when its name is chosen', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/bots' });
    await findTeamList();
    await user.click(within(availableList()).getByRole('button', { name: B.page.details('Sprout') }));
    const detail = screen.getByRole('complementary', { name: 'Sprout' });
    expect(within(detail).getByText('I work from numbers you paste in or that a Confluence page holds, and I say so.')).toBeInTheDocument();
    expect(within(availableList()).getByRole('button', { name: B.page.details('Sprout') })).toHaveAttribute('aria-pressed', 'true');
  });

  it('Add acts at once with via "bots_page"', async () => {
    const user = userEvent.setup();
    const rec = fresh();
    renderApp({ hash: '/bots', wrap: spy(rec) });
    await findTeamList();
    await user.click(within(availableList()).getByRole('button', { name: B.page.addLabel('Echo') }));
    await screen.findByText(B.page.added('Echo'));
    expect(rec.changes).toEqual([{ add: ['echo'], via: 'bots_page' }]);
    const team = await findTeamList();
    expect(within(team).getByRole('button', { name: B.page.removeLabel('Echo') })).toBeInTheDocument();
  });

  it('removing a bot with routines asks what to do with them, and sends the answer', async () => {
    const user = userEvent.setup();
    const rec = fresh();
    renderApp({ hash: '/bots', wrap: spy(rec) });
    const team = await findTeamList();
    await user.click(within(team).getByRole('button', { name: B.page.removeLabel('Chief') }));
    const dialog = await screen.findByRole('alertdialog', { name: B.remove.title('Chief') });
    const group = within(dialog).getByRole('group', { name: B.remove.routinesHeading('Chief', 1) });
    expect(within(group).getByRole('radio', { name: new RegExp(B.remove.toNudge) })).toBeChecked();
    await user.click(within(group).getByRole('radio', { name: new RegExp(B.remove.pause) }));
    await user.click(within(dialog).getByRole('button', { name: B.remove.confirm }));
    await screen.findByText(B.page.removed('Chief'));
    expect(rec.changes).toEqual([{ remove: ['chief'], via: 'bots_page', routines: 'pause' }]);
    expect(within(availableList()).getByRole('button', { name: B.page.addLabel('Chief') })).toBeInTheDocument();
  });

  it('removing a bot without routines asks nothing about them, and Escape cancels', async () => {
    const user = userEvent.setup();
    const rec = fresh();
    renderApp({ hash: '/bots', team: ['chief', 'sprout'], wrap: spy(rec) });
    const team = await findTeamList();
    const remove = within(team).getByRole('button', { name: B.page.removeLabel('Sprout') });
    await user.click(remove);
    let dialog = await screen.findByRole('alertdialog', { name: B.remove.title('Sprout') });
    expect(within(dialog).queryByRole('group')).not.toBeInTheDocument();
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument();
    expect(remove).toHaveFocus();
    await user.click(remove);
    dialog = await screen.findByRole('alertdialog', { name: B.remove.title('Sprout') });
    await user.click(within(dialog).getByRole('button', { name: B.remove.confirm }));
    await screen.findByText(B.page.removed('Sprout'));
    expect(rec.changes).toEqual([{ remove: ['sprout'], via: 'bots_page' }]);
  });

  it('Settings shows the bot\'s routines, read only', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/bots' });
    const team = await findTeamList();
    await user.click(within(team).getByRole('button', { name: B.page.settingsLabel('Chief') }));
    const dialog = await screen.findByRole('dialog', { name: B.settings.title('Chief') });
    expect(within(dialog).getByText('“What slipped last week?”')).toBeInTheDocument();
    await user.click(within(dialog).getByRole('button', { name: B.settings.close }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('a withdrawn bot shows as no longer offered, with its history and Remove only', async () => {
    renderApp({ hash: '/bots', withdrawnBot: true });
    const h = await screen.findByRole('heading', { level: 2, name: new RegExp(`^${B.page.withdrawnHeading}`) });
    const section = h.closest('section')!;
    const row = within(section).getAllByRole('listitem')[0]!;
    expect(within(row).getByText(B.gone)).toBeInTheDocument();
    expect(within(row).getByText(B.draftsThisMonth(1), { exact: false })).toBeInTheDocument();
    expect(within(row).getByRole('button', { name: B.page.details(B.gone) })).toBeInTheDocument();
    expect(within(row).getByRole('button', { name: B.page.removeGoneLabel })).toBeInTheDocument();
    expect(within(row).getAllByRole('button')).toHaveLength(2);
    expect(row.querySelector('svg[data-mascot]')).toHaveAttribute('data-mascot', 'unknown');
  });

  it.each([
    ['always_on', copy.errors.codes.always_on],
    ['team_full', copy.errors.codes.team_full],
    ['rate_limited', B.errors.codes.rate_limited!],
    ['bot_unavailable', B.errors.codes.bot_unavailable!],
    ['upstream_unavailable', B.errors.codes.upstream_unavailable!],
    ['assertion_required', B.errors.codes.assertion_required!],
    ['unavailable', B.errors.codes.unavailable!],
    ['not_allowed', B.errors.codes.not_allowed!],
    ['not_a_member', copy.errors.codes.not_a_member],
    ['bad_request', copy.errors.codes.bad_request],
    ['users_unavailable', copy.errors.codes.users_unavailable],
  ])('says what a refused change (%s) means, and nothing changes', async (code, text) => {
    const user = userEvent.setup();
    renderApp({ hash: '/bots', teamError: code });
    await findTeamList();
    await user.click(within(availableList()).getByRole('button', { name: B.page.addLabel('Echo') }));
    expect(await screen.findByRole('alert')).toHaveTextContent(text);
    expect(within(availableList()).getByRole('button', { name: B.page.addLabel('Echo') })).toBeInTheDocument();
  });

  it('after a lost answer it never says nothing changed, and offers to reload', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/bots', teamError: 'unknown_outcome' });
    await findTeamList();
    await user.click(within(availableList()).getByRole('button', { name: B.page.addLabel('Echo') }));
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(B.errors.codes.unknown_outcome!);
    expect(alert).not.toHaveTextContent(/Nothing changed/);
    // the page was read again: the change had gone through
    await waitFor(async () => expect(within(await findTeamList()).getByRole('button', { name: B.page.removeLabel('Echo') })).toBeInTheDocument());
    await user.click(within(alert).getByRole('button', { name: B.page.reload }));
    await waitFor(() => expect(screen.queryByRole('alert')).not.toBeInTheDocument());
  });

  it('a removal whose answer was lost says so inside the dialog', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/bots', teamError: 'unknown_outcome' });
    const team = await findTeamList();
    await user.click(within(team).getByRole('button', { name: B.page.removeLabel('Chief') }));
    const dialog = await screen.findByRole('alertdialog');
    await user.click(within(dialog).getByRole('button', { name: B.remove.confirm }));
    expect(await within(dialog).findByText(B.errors.codes.unknown_outcome!)).toBeInTheDocument();
  });

  it('when the team can\'t be read it says so, and the ask box offers Nudge only', async () => {
    const one = renderApp({ hash: '/bots', catalog: 'unavailable' });
    expect(await screen.findByText(B.page.teamUnavailable)).toBeInTheDocument();
    expect(await screen.findByText(B.errors.codes.unavailable!)).toBeInTheDocument();
    one.unmount();
    renderApp({ catalog: 'unavailable' });
    expect(await screen.findByText(B.picker.unavailableNote)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^Asking/ })).not.toBeInTheDocument();
  });

  it('is not there for an auditor', async () => {
    renderApp({ role: 'auditor', hash: '/bots' });
    await screen.findByRole('navigation', { name: copy.nav.label });
    expect(navLink(copy.nav.bots)).toBeNull();
    expect(screen.queryByRole('heading', { level: 1, name: B.page.title })).not.toBeInTheDocument();
  });
});

const pickerButton = (name: string) => screen.findByRole('button', { name: B.picker.button(name) });

describe('the picker in the ask box', () => {
  it('defaults to the bot asked last, and asks it', async () => {
    const user = userEvent.setup();
    const rec = fresh();
    renderApp({ wrap: spy(rec) });
    await pickerButton('Chief');
    await user.type(screen.getByLabelText(copy.home.askLabel), 'Prep me for the staff meeting{Enter}');
    await waitFor(() => expect(window.location.hash).toMatch(/^#\/a\//));
    expect(rec.asks).toEqual([['Prep me for the staff meeting', 'chief']]);
  });

  it('works from the keyboard: arrows, type-ahead, Enter to choose, Escape to close', async () => {
    const user = userEvent.setup();
    renderApp();
    const button = await pickerButton('Chief');
    button.focus();
    await user.keyboard('{ArrowDown}');
    const list = screen.getByRole('listbox', { name: B.picker.listLabel });
    expect(list).toHaveFocus();
    const options = within(list).getAllByRole('option');
    expect(options.map((o) => o.textContent)).toEqual([expect.stringContaining('Nudge'), expect.stringContaining('Chief')]);
    expect(options[1]).toHaveAttribute('aria-selected', 'true');
    expect(list).toHaveAttribute('aria-activedescendant', options[1]!.id);
    await user.keyboard('{ArrowUp}');
    expect(list).toHaveAttribute('aria-activedescendant', options[0]!.id);
    await user.keyboard('c');
    expect(list).toHaveAttribute('aria-activedescendant', options[1]!.id);
    await user.keyboard('{Home}{Enter}');
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    expect(await pickerButton('Nudge')).toHaveFocus();
    await user.keyboard('{Enter}');
    expect(screen.getByRole('listbox')).toBeInTheDocument();
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    expect(await pickerButton('Nudge')).toHaveFocus();
  });

  it('lists the bots not on the team greyed, with Add: it adds with via "picker" and selects', async () => {
    const user = userEvent.setup();
    const rec = fresh();
    renderApp({ wrap: spy(rec) });
    await user.click(await pickerButton('Chief'));
    const others = screen.getByRole('list', { name: B.picker.notOnTeam });
    expect(within(others).getAllByText(new RegExp(B.picker.notOnTeamShort))).toHaveLength(2);
    expect(screen.getByRole('link', { name: B.picker.manage })).toHaveAttribute('href', '#/bots');
    await user.click(within(others).getByRole('button', { name: B.picker.addLabel('Sprout') }));
    expect(await pickerButton('Sprout')).toBeInTheDocument();
    expect(rec.changes).toEqual([{ add: ['sprout'], via: 'picker' }]);
    expect(screen.getByText(B.picker.added('Sprout'))).toBeInTheDocument();
    // the chips are Sprout's examples now, and a chip fills the box without sending
    const chips = screen.getByRole('list', { name: copy.launch.suggestionsLabel });
    await user.click(within(chips).getByRole('button', { name: 'Design a test for the paywall' }));
    expect(screen.getByLabelText(copy.home.askLabel)).toHaveValue('Design a test for the paywall');
    expect(rec.asks).toEqual([]);
  });

  it('is not shown with only Nudge on the team', async () => {
    renderApp({ team: [] });
    await screen.findByLabelText(copy.home.askLabel);
    await new Promise((r) => setTimeout(r, 20));
    expect(screen.queryByRole('button', { name: /^Asking/ })).not.toBeInTheDocument();
  });

  it.each([
    ['bot_not_on_team', copy.errors.codes.bot_not_on_team],
    ['bot_unavailable', copy.errors.codes.bot_unavailable],
  ])('a refused ask (%s) says so, and falls back to Nudge once the team is read again', async (code, text) => {
    const user = userEvent.setup();
    // Chief left the team elsewhere (another tab, or an admin withdrew it): the ask is refused, and
    // the server's `Me` from then on has Nudge only.
    let gone = false;
    renderApp({
      wrap: (api) => {
        const me = api.me.bind(api);
        api.createTask = async () => {
          gone = true;
          throw new ApiError(409, code, 'Refused.');
        };
        api.me = async () => {
          const m = await me();
          return gone ? { ...m, team: ['nudge'], ask_persona: 'nudge' } : m;
        };
        return api;
      },
    });
    await pickerButton('Chief');
    await user.type(screen.getByLabelText(copy.home.askLabel), 'What slipped?{Enter}');
    expect(await screen.findByText(text)).toBeInTheDocument();
    expect(window.location.hash).toBe('');
    // the text stays in the box, and the picker falls back to Nudge (with only Nudge, no picker)
    expect(screen.getByLabelText(copy.home.askLabel)).toHaveValue('What slipped?');
    await waitFor(() => expect(screen.queryByRole('button', { name: /^Asking/ })).not.toBeInTheDocument());
  });

  it('Echo\'s drafts carry the internal-review line from the back end, once', async () => {
    const user = userEvent.setup();
    renderApp({ team: ['chief', 'echo'] });
    await user.click(await pickerButton('Chief'));
    await user.click(screen.getByRole('option', { name: /Echo/ }));
    await user.type(screen.getByLabelText(copy.home.askLabel), 'Write release notes for 4.2{Enter}');
    await user.click(await screen.findByRole('button', { name: copy.activity.openDraft }));
    await screen.findByText(B.draftedBy('Echo'));
    expect(screen.getAllByText(ECHO_LINE)).toHaveLength(1);
  });
});

describe('attribution', () => {
  it('draft rows on Today and Drafts show the bot that drafted them', async () => {
    renderApp();
    const rows = await screen.findByRole('list', { name: copy.today.decideHeading });
    const jira = within(rows).getByRole('link', { name: /Show grace period banner/ });
    expect(jira).toHaveAccessibleName(expect.stringContaining(B.byLabel('Chief')));
    const prd = within(rows).getByRole('link', { name: /PRD: TCB grace period setting/ });
    expect(prd).toHaveAccessibleName(expect.stringContaining(B.byLabel('Nudge')));
    expect(prd.querySelector('[data-mascot]')).toHaveAttribute('data-mascot', 'nudge');
  });

  it('the draft page says "Drafted by Chief"; a withdrawn bot reads as no longer offered, with the neutral face', async () => {
    const one = renderApp({ hash: '/d/apr_jira' });
    expect(await screen.findByText(B.draftedBy('Chief'))).toBeInTheDocument();
    one.unmount();
    renderApp({ hash: '/d/apr_done', withdrawnBot: true });
    const line = await screen.findByText(B.draftedBy(B.gone));
    expect(line.closest('.drafted-by')!.querySelector('svg')).toHaveAttribute('data-mascot', 'unknown');
  });

  it('a key the catalog does not list is never named from the key', async () => {
    renderApp({
      hash: '/d/apr_jira',
      wrap: (api) => {
        const get = api.getDraft.bind(api);
        api.getDraft = async (id) => ({ ...(await get(id)), persona: 'mystery', persona_offered: true });
        return api;
      },
    });
    expect(await screen.findByText(B.draftedBy(B.gone))).toBeInTheDocument();
    expect(screen.queryByText(/mystery/i)).not.toBeInTheDocument();
  });

  it('Activity shows each request\'s bot', async () => {
    renderApp({ hash: '/activity' });
    const list = await screen.findByRole('list', { name: copy.activity.title });
    const week = within(list).getByRole('link', { name: /What changed in billing this week/ });
    expect(week).toHaveAccessibleName(expect.stringContaining(B.byLabel('Chief')));
  });

  it('a reviewer sees which bot drafted each draft, a restricted one included', async () => {
    const one = renderApp({ role: 'reviewer', hash: '/reviews' });
    const list = await screen.findByRole('list', { name: copy.reviews.title });
    expect(within(list).getByRole('link', { name: /exports/i })).toHaveAccessibleName(expect.stringContaining(B.byLabel('Sprout')));
    one.unmount();
    renderApp({ role: 'reviewer', hash: '/reviews/apr_dana_exports' });
    expect(await screen.findByText(B.draftedBy('Sprout'))).toBeInTheDocument();
  });

  it('with "Show mascots" off, a coloured dot stands in for the face', async () => {
    renderApp({ mascot: false, hash: '/d/apr_jira' });
    const line = await screen.findByText(B.draftedBy('Chief'));
    const by = line.closest('.drafted-by')!;
    expect(by.querySelector('svg')).toBeNull();
    expect(by.querySelector('.bot-dot-chief')).not.toBeNull();
  });
});

describe('the ask box after a team change elsewhere', () => {
  it('a bot removed from the team falls back to Nudge', async () => {
    const spyMe = vi.fn();
    renderApp({
      wrap: (api) => {
        const me = api.me.bind(api);
        api.me = async () => {
          spyMe();
          const m = await me();
          return { ...m, team: ['nudge'], ask_persona: 'chief' };
        };
        return api;
      },
    });
    await screen.findByLabelText(copy.home.askLabel);
    await waitFor(() => expect(spyMe).toHaveBeenCalled());
    expect(screen.queryByRole('button', { name: /^Asking/ })).not.toBeInTheDocument();
  });
});
