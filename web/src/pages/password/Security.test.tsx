import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import type { Api } from '../../api';
import { MOCK_PASSWORDS, MOCK_TOTP } from '../../api/mockPassword';
import { copy } from '../../copy';
import { renderApp } from '../../test/util';

const P = copy.password;
const S = P.security;

async function openMenu() {
  await userEvent.click(await screen.findByRole('button', { name: copy.menu.button('Ana Silva') }));
  return screen.findByRole('menu');
}

function openPage(opts: Parameters<typeof renderApp>[0] = {}) {
  renderApp({ providers: ['password', 'dev'], hash: '/security', ...opts });
}

const changeForm = () => screen.getByRole('region', { name: S.changeTitle });
const codesCard = () => screen.getByRole('region', { name: S.codesTitle });

describe('Sign-in and security', () => {
  it('is in the user menu for someone who signs in with a password', async () => {
    renderApp({ providers: ['password', 'dev'] });
    const menu = await openMenu();
    await userEvent.click(await within(menu).findByRole('menuitem', { name: copy.menu.security }));
    expect(await screen.findByRole('heading', { level: 1, name: S.title })).toBeInTheDocument();
    expect(window.location.hash).toBe('#/security');
  });

  it('is not in the menu when password sign-in is off', async () => {
    renderApp({ providers: ['dev'] });
    const menu = await openMenu();
    expect(within(menu).getByRole('menuitem', { name: copy.menu.signOut })).toBeInTheDocument();
    expect(within(menu).queryByRole('menuitem', { name: copy.menu.security })).toBeNull();
  });

  it('shows the state and how many recovery codes are left', async () => {
    openPage();
    expect(await screen.findByText(S.passwordOn)).toBeInTheDocument();
    expect(screen.getByText(S.recoveryLeft(10))).toBeInTheDocument();
    expect(screen.queryByText(S.lowWarn(2))).toBeNull();
  });

  it.each([2, 1, 0])('warns gently with %i recovery codes left', async (n) => {
    openPage({
      wrap: (api: Api) => {
        const status = api.passwordStatus.bind(api);
        api.passwordStatus = async () => ({ ...(await status()), recovery_codes_left: n });
        return api;
      },
    });
    expect(await screen.findByText(n === 0 ? S.noneWarn : S.lowWarn(n))).toBeInTheDocument();
  });

  it('says so to someone without a password', async () => {
    openPage({ role: 'reviewer' });
    expect(await screen.findByText(S.noPassword)).toBeInTheDocument();
    expect(screen.queryByRole('region', { name: S.changeTitle })).toBeNull();
  });

  it('changes the password, saying other sessions end', async () => {
    openPage();
    await screen.findByRole('region', { name: S.changeTitle });
    const form = changeForm();
    expect(within(form).getByText(S.changeEnds)).toBeInTheDocument();
    expect(within(form).getByLabelText(S.current)).toHaveAttribute('autocomplete', 'current-password');
    expect(within(form).getByLabelText(S.next)).toHaveAttribute('autocomplete', 'new-password');
    await userEvent.type(within(form).getByLabelText(S.current), MOCK_PASSWORDS.ana);
    await userEvent.type(within(form).getByLabelText(S.code), MOCK_TOTP);
    await userEvent.type(within(form).getByLabelText(S.next), 'a new and longer one');
    await userEvent.click(within(form).getByRole('button', { name: S.change }));
    expect(await within(form).findByText(S.changed)).toBeInTheDocument();
    expect(within(form).getByLabelText(S.current)).toHaveValue('');
  });

  it('says one plain sentence when the current password or the code is wrong', async () => {
    openPage();
    await screen.findByRole('region', { name: S.changeTitle });
    const form = changeForm();
    await userEvent.type(within(form).getByLabelText(S.current), 'not-my-password');
    await userEvent.type(within(form).getByLabelText(S.code), MOCK_TOTP);
    await userEvent.type(within(form).getByLabelText(S.next), 'a new and longer one');
    await userEvent.click(within(form).getByRole('button', { name: S.change }));
    const alert = await within(form).findByRole('alert');
    expect(alert).toHaveTextContent(S.reauth_failed);
    await waitFor(() => expect(alert.closest('.pw-note')).toHaveFocus());
    expect(within(form).getByLabelText(S.current)).toHaveValue('');
  });

  it('words a refused new password', async () => {
    openPage();
    await screen.findByRole('region', { name: S.changeTitle });
    const form = changeForm();
    await userEvent.type(within(form).getByLabelText(S.current), MOCK_PASSWORDS.ana);
    await userEvent.type(within(form).getByLabelText(S.code), MOCK_TOTP);
    await userEvent.type(within(form).getByLabelText(S.next), 'password1234');
    await userEvent.click(within(form).getByRole('button', { name: S.change }));
    expect(await within(form).findByRole('alert')).toHaveTextContent(P.weak.common);
  });

  it('never says either way when the answer was lost', async () => {
    openPage({ passwordRefusal: 'unknown_outcome' });
    await screen.findByRole('region', { name: S.changeTitle });
    const form = changeForm();
    await userEvent.type(within(form).getByLabelText(S.current), MOCK_PASSWORDS.ana);
    await userEvent.type(within(form).getByLabelText(S.code), MOCK_TOTP);
    await userEvent.type(within(form).getByLabelText(S.next), 'a new and longer one');
    await userEvent.click(within(form).getByRole('button', { name: S.change }));
    const alert = await within(form).findByRole('alert');
    expect(alert).toHaveTextContent(S.unknown);
    expect(alert.textContent).not.toMatch(/nothing changed/i);
  });

  it('makes new recovery codes, shown once, with the "saved" box', async () => {
    openPage();
    await screen.findByRole('region', { name: S.codesTitle });
    expect(within(codesCard()).getByText(S.codesReplace)).toBeInTheDocument();
    await userEvent.type(within(codesCard()).getByLabelText(S.code), MOCK_TOTP);
    await userEvent.click(within(codesCard()).getByRole('button', { name: S.make }));
    expect(await within(codesCard()).findByRole('heading', { name: S.newTitle })).toHaveFocus();
    expect(within(within(codesCard()).getByRole('list', { name: P.codes.label })).getAllByRole('listitem')).toHaveLength(10);
    const done = within(codesCard()).getByRole('button', { name: S.done });
    expect(done).toBeDisabled();
    await userEvent.click(within(codesCard()).getByRole('checkbox', { name: P.codes.saved }));
    await userEvent.click(done);
    await waitFor(() => expect(within(codesCard()).queryByRole('list', { name: P.codes.label })).toBeNull());
  });

  it('refuses new codes with a wrong authenticator code in one sentence', async () => {
    openPage();
    await screen.findByRole('region', { name: S.codesTitle });
    await userEvent.type(within(codesCard()).getByLabelText(S.code), '111111');
    await userEvent.click(within(codesCard()).getByRole('button', { name: S.make }));
    expect(await within(codesCard()).findByRole('alert')).toHaveTextContent(S.reauth_failed);
  });
});
