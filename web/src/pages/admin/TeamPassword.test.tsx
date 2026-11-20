import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { copy } from '../../copy';
import { renderApp } from '../../test/util';

const T = copy.admin.team;
const SC = T.setupCode;
const table = () => screen.findByRole('table', { name: T.tableLabel });
const rowOf = async (name: string | RegExp) => within(await table()).getByRole('row', { name });

async function openMenu(who: string) {
  await userEvent.click(await screen.findByRole('button', { name: T.menu.button(who) }));
  return screen.getByRole('menu', { name: T.menu.button(who) });
}

function admin(opts: Parameters<typeof renderApp>[0] = {}) {
  renderApp({ role: 'admin', hash: '/admin/team', providers: ['entra', 'password'], ...opts });
}

describe('team: password sign-in', () => {
  it('shows the password state beside the Microsoft state, booleans only', async () => {
    admin();
    const ana = await rowOf(/Ana Silva/);
    expect(within(ana).getByText(T.signIn.passwordSr)).toBeInTheDocument();
    const ravi = await rowOf(/Ravi Kumar/);
    expect(within(ravi).getByText(T.signIn.codeWaitingSetup)).toBeInTheDocument();
    await table();
    const dana = document.getElementById('person-dana')!;
    expect(within(dana).getByText(T.signIn.codeWaitingReset)).toBeInTheDocument();
    const ben = await rowOf(/Ben Carter/);
    expect(within(ben).queryByText(T.signIn.password)).toBeNull();
    expect(screen.getByRole('columnheader', { name: T.columns.signIn })).toBeInTheDocument();
  });

  it('labels the action by whether a password is set, and never offers it to oneself', async () => {
    admin();
    expect(within(await openMenu('Ravi Kumar')).getByRole('menuitem', { name: SC.menuSetup })).toBeInTheDocument();
    await userEvent.keyboard('{Escape}');
    expect(within(await openMenu('Ana Silva')).getByRole('menuitem', { name: SC.menuReset })).toBeInTheDocument();
    await userEvent.keyboard('{Escape}');
    const self = await openMenu('Ada Okafor');
    expect(within(self).queryByRole('menuitem', { name: SC.menuSetup })).toBeNull();
    expect(within(self).queryByRole('menuitem', { name: SC.menuReset })).toBeNull();
    await userEvent.keyboard('{Escape}');
    // Not for someone deactivated, someone whose sign-in name isn't confirmed, or the evals account.
    for (const who of ['Tom Reyes', 'Sam Lee', 'Evals account']) {
      const m = await openMenu(who);
      expect(within(m).queryByRole('menuitem', { name: /Create a (setup|reset) code/ })).toBeNull();
      await userEvent.keyboard('{Escape}');
    }
  });

  it('is not offered while password sign-in is off', async () => {
    admin({ providers: ['entra'] });
    const m = await openMenu('Ravi Kumar');
    expect(within(m).queryByRole('menuitem', { name: SC.menuSetup })).toBeNull();
  });

  it('creates a setup code and shows it once, with its expiry and how to pass it on', async () => {
    admin();
    await userEvent.click(within(await openMenu('Ravi Kumar')).getByRole('menuitem', { name: SC.menuSetup }));
    const d = screen.getByRole('dialog', { name: SC.headingSetup('Ravi Kumar') });
    expect(within(d).getByText(SC.introSetup('Ravi Kumar'))).toBeInTheDocument();
    expect(within(d).getByText(SC.replaces)).toBeInTheDocument();
    await userEvent.click(within(d).getByRole('button', { name: SC.submit }));
    const made = await screen.findByRole('dialog', { name: SC.doneSetup('Ravi Kumar') });
    expect(within(made).getByText(/^[A-Z0-9]{5}(-[A-Z0-9]{5}){3}$/)).toBeInTheDocument();
    expect(within(made).getByText(/This is the only time it is shown/)).toHaveTextContent(/Works once, until \d+ \w{3}, \d\d:\d\d UTC/);
    expect(within(made).getByText(SC.handOff)).toBeInTheDocument();
    expect(within(made).getByText(SC.whatTheyDo('Ravi Kumar'))).toBeInTheDocument();
    await waitFor(() => expect(within(made).getByRole('button', { name: SC.copy })).toHaveFocus());
    expect(within(made).queryByRole('button', { name: T.cancel })).toBeNull();
    await userEvent.click(within(made).getByRole('button', { name: SC.done }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(screen.queryByText(/^[A-Z0-9]{5}(-[A-Z0-9]{5}){3}$/)).toBeNull();
  });

  it('warns that a reset replaces everything and that the old password works until then', async () => {
    admin();
    await userEvent.click(within(await openMenu('Ana Silva')).getByRole('menuitem', { name: SC.menuReset }));
    const d = screen.getByRole('dialog', { name: SC.headingReset('Ana Silva') });
    expect(within(d).getByText(SC.introReset('Ana Silva'))).toBeInTheDocument();
    expect(within(d).getByText(SC.stillWorks)).toBeInTheDocument();
    await userEvent.click(within(d).getByRole('button', { name: SC.submit }));
    expect(await screen.findByRole('dialog', { name: SC.doneReset('Ana Silva') })).toBeInTheDocument();
  });

  it('says an Admin or Auditor gets their code from the host, with the command', async () => {
    admin();
    await userEvent.click(within(await openMenu('Raj Mehta')).getByRole('menuitem', { name: SC.menuSetup }));
    const d = screen.getByRole('dialog', { name: SC.headingHost('Raj Mehta') });
    expect(within(d).getByText(SC.hostOnly('Raj Mehta'))).toBeInTheDocument();
    expect(within(d).getByText(SC.hostCommand('raj'))).toBeInTheDocument();
    expect(within(d).queryByRole('button', { name: SC.submit })).toBeNull();
  });

  it.each(['host_only', 'self_change', 'evals_account', 'sign_in_unconfirmed', 'not_active', 'not_configured'])(
    'says what to do instead when refused with %s',
    async (code) => {
      admin({ usersError: code });
      await userEvent.click(within(await openMenu('Ravi Kumar')).getByRole('menuitem', { name: SC.menuSetup }));
      const d = screen.getByRole('dialog');
      await userEvent.click(within(d).getByRole('button', { name: SC.submit }));
      const alert = await within(d).findByRole('alert');
      expect(alert).toHaveTextContent(SC.errors[code]!);
      expect(alert.textContent).not.toContain(code);
    },
  );

  it('the auditor sees the password state and no action', async () => {
    renderApp({ role: 'auditor', hash: '/admin/team', providers: ['entra', 'password'] });
    const ana = await rowOf(/Ana Silva/);
    expect(within(ana).getByText(T.signIn.passwordSr)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^Actions for/ })).toBeNull();
  });
});
