import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import type { Api } from '../api';
import { copy } from '../copy';
import { renderApp } from '../test/util';

// Slice R2: who writes the daily brief. Chief's Settings on the Bots page has the switch "Chief
// writes my daily brief"; Today's brief line says who wrote the brief that is in.

const B = copy.bots;
const S = B.settings.brief;

/** Record every brief change, and pass it on. */
function spy(calls: string[]) {
  return (api: Api) => {
    const set = api.setBriefWriter.bind(api);
    api.setBriefWriter = (p) => {
      calls.push(p);
      return set(p);
    };
    return api;
  };
}

const teamList = async () => {
  const h = await screen.findByRole('heading', { level: 2, name: new RegExp(`^${B.page.onTeam}`) });
  return within(h.closest('section')!).getByRole('list');
};

async function openSettings(user: ReturnType<typeof userEvent.setup>, name: string) {
  const team = await teamList();
  await user.click(within(team).getByRole('button', { name: B.page.settingsLabel(name) }));
  return screen.findByRole('dialog', { name: B.settings.title(name) });
}

describe('Chief writes my daily brief', () => {
  it('says what changes, switches the brief to Chief and back, from the mouse and the keyboard', async () => {
    const user = userEvent.setup();
    const calls: string[] = [];
    renderApp({ hash: '/bots', wrap: spy(calls) });
    const dialog = await openSettings(user, 'Chief');
    const sw = within(dialog).getByRole('switch', { name: S.label('Chief') });
    expect(sw).toHaveAttribute('aria-checked', 'false');
    // Before it is used it says who writes it now and what would change.
    expect(sw).toHaveAccessibleDescription(`${S.nowNudge} ${S.what('Chief')} ${S.preview('Chief')}`);
    // Chief runs one routine of its own; the brief is Nudge's.
    expect(within(dialog).queryByText(B.settings.briefRoutine)).not.toBeInTheDocument();

    await user.click(sw);
    await waitFor(() => expect(sw).toHaveAttribute('aria-checked', 'true'));
    expect(calls).toEqual(['chief']);
    expect(within(dialog).getByRole('status')).toHaveTextContent(S.movedOn('Chief'));
    expect(sw).toHaveFocus();
    expect(sw).toHaveAccessibleDescription(`${S.nowThis('Chief')} ${S.whatOn('Chief')} ${S.preview('Chief')}`);
    // The brief now shows among Chief's routines, by name, not by its prompt.
    expect(within(dialog).getByText(B.settings.briefRoutine)).toBeInTheDocument();
    expect(within(dialog).queryByText(/\[daily-brief\]/)).not.toBeInTheDocument();

    // Space works it too: back to Nudge.
    await user.keyboard(' ');
    await waitFor(() => expect(sw).toHaveAttribute('aria-checked', 'false'));
    expect(calls).toEqual(['chief', 'nudge']);
    expect(within(dialog).getByRole('status')).toHaveTextContent(S.movedOff);
    // Escape closes the dialog and gives focus back to Settings.
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(within(await teamList()).getByRole('button', { name: B.page.settingsLabel('Chief') })).toHaveFocus();
  });

  it('is only in the Settings of a bot that writes briefs', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/bots', team: ['chief', 'sprout'] });
    const dialog = await openSettings(user, 'Sprout');
    expect(within(dialog).queryByRole('switch')).not.toBeInTheDocument();
    expect(within(dialog).queryByRole('heading', { name: S.heading })).not.toBeInTheDocument();
  });

  it('without a brief routine it says how to start one, and offers no switch', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/bots', briefWriter: 'none' });
    const dialog = await openSettings(user, 'Chief');
    expect(within(dialog).getByText(S.none('Chief'))).toBeInTheDocument();
    expect(within(dialog).queryByRole('switch')).not.toBeInTheDocument();
  });

  it('with Chief already writing it, the switch is on and the brief is among its routines', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/bots', briefWriter: 'chief' });
    const dialog = await openSettings(user, 'Chief');
    const sw = within(dialog).getByRole('switch', { name: S.label('Chief') });
    expect(sw).toHaveAttribute('aria-checked', 'true');
    expect(within(dialog).getByText(B.settings.briefRoutine)).toBeInTheDocument();
  });

  it('a refused change says why, inside the dialog, and leaves the switch as it was', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/bots', teamError: 'unavailable' });
    const dialog = await openSettings(user, 'Chief');
    const sw = within(dialog).getByRole('switch', { name: S.label('Chief') });
    await user.click(sw);
    expect(await within(dialog).findByRole('alert')).toHaveTextContent(B.errors.codes.unavailable!);
    expect(sw).toHaveAttribute('aria-checked', 'false');
    expect(within(dialog).getByRole('status')).toBeEmptyDOMElement();
  });

  it('a lost answer never says nothing changed', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/bots', teamError: 'unknown_outcome' });
    const dialog = await openSettings(user, 'Chief');
    await user.click(within(dialog).getByRole('switch', { name: S.label('Chief') }));
    const alert = await within(dialog).findByRole('alert');
    expect(alert).toHaveTextContent(S.lost);
    expect(alert.textContent).not.toMatch(/nothing changed/i);
  });

  it('removing Chief says the brief goes back to Nudge whichever is chosen, and it does', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/bots', briefWriter: 'chief' });
    const team = await teamList();
    await user.click(within(team).getByRole('button', { name: B.page.removeLabel('Chief') }));
    const dialog = await screen.findByRole('alertdialog', { name: B.remove.title('Chief') });
    expect(within(dialog).getByText(B.remove.briefBack('Chief'))).toBeInTheDocument();
    await user.click(within(dialog).getByRole('radio', { name: new RegExp(B.remove.pause) }));
    await user.click(within(dialog).getByRole('button', { name: B.remove.confirm }));
    await screen.findByText(B.page.removed('Chief'));
    // Add Chief back: the brief is Nudge's, not paused with Chief.
    const available = screen.getByRole('heading', { level: 2, name: new RegExp(`^${B.page.available}`) }).closest('section')!;
    await user.click(within(available).getByRole('button', { name: B.page.addLabel('Chief') }));
    await screen.findByText(B.page.added('Chief'));
    const settings = await openSettings(user, 'Chief');
    const sw = within(settings).getByRole('switch', { name: S.label('Chief') });
    expect(sw).toHaveAttribute('aria-checked', 'false');
    expect(sw).toHaveAccessibleDescription(new RegExp(`^${S.nowNudge}`));
  });
});

describe("Today's brief says who wrote it", () => {
  it('names Chief, with its face, when Chief wrote the brief that is in', async () => {
    renderApp({ briefWriter: 'chief' });
    const line = (await screen.findByText(copy.today.briefReady, { exact: false })).closest('p')!;
    const by = within(line).getByText(copy.today.briefBy('Chief'));
    expect(by.closest('[data-bot]')).toHaveAttribute('data-bot', 'chief');
    expect(line.querySelector('svg[data-mascot]')).toHaveAttribute('data-mascot', 'chief');
  });

  it('names Nudge for Nudge\'s brief, and nobody for an older brief', async () => {
    const view = renderApp({});
    const line = (await screen.findByText(copy.today.briefReady, { exact: false })).closest('p')!;
    expect(within(line).getByText(copy.today.briefBy('Nudge'))).toBeInTheDocument();
    view.unmount();
    renderApp({ briefWriter: 'chief', briefDelivery: 'stale' });
    await screen.findByText(/No brief yet today/);
    expect(screen.queryByText(copy.today.briefBy('Chief'))).not.toBeInTheDocument();
  });
});
