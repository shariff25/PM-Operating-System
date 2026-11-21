import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { MOCK_SETUP_CODES, MOCK_TOTP } from '../../api/mockPassword';
import { copy } from '../../copy';
import { renderApp } from '../../test/util';
import { qrModules } from './QrCode';
import { weakText } from './rules';

const P = copy.password;
const S = P.setup;

function open(opts: Parameters<typeof renderApp>[0] = {}) {
  return renderApp({ signedIn: false, providers: ['password'], hash: '/setup', ...opts });
}

async function begin(name = 'ravi@contoso.example', code: string = MOCK_SETUP_CODES.ravi) {
  await userEvent.type(await screen.findByLabelText(P.nameLabel), name);
  await userEvent.type(screen.getByLabelText(S.codeLabel), code);
  await userEvent.click(screen.getByRole('button', { name: S.begin }));
}

async function choose(password: string) {
  const field = await screen.findByLabelText(S.newPassword);
  await userEvent.clear(field);
  await userEvent.type(field, password);
  await userEvent.click(screen.getByRole('button', { name: S.savePassword }));
}

const step = () => screen.getByText(/^Step \d of 4$|^Done$/).textContent;

describe('password setup', () => {
  it('opens from the sign-in screen', async () => {
    renderApp({ signedIn: false, providers: ['password'] });
    await userEvent.click(await screen.findByRole('link', { name: P.setupLink }));
    expect(await screen.findByRole('heading', { level: 1, name: S.title })).toBeInTheDocument();
  });

  it('walks the four steps in order, then shows the ten recovery codes once and waits for "saved"', async () => {
    open();
    expect(await screen.findByLabelText(P.nameLabel)).toHaveFocus();
    expect(step()).toBe('Step 1 of 4');
    expect(screen.getByText(S.leaveNote)).toBeInTheDocument();
    expect(screen.getByLabelText(S.codeLabel)).toHaveAttribute('autocomplete', 'off');
    // Case, spaces and hyphens don't matter in a setup code.
    await begin('ravi@contoso.example', MOCK_SETUP_CODES.ravi.toLowerCase().replace(/-/g, ' '));

    expect(await screen.findByRole('heading', { level: 1, name: S.passwordTitle })).toBeInTheDocument();
    expect(step()).toBe('Step 2 of 4');
    expect(screen.getByText(S.as('ravi'))).toBeInTheDocument();
    for (const rule of P.rules(12, 128)) expect(screen.getByText(rule)).toBeInTheDocument();
    expect(screen.queryByRole('meter')).toBeNull();
    const pw = screen.getByLabelText(S.newPassword);
    expect(pw).toHaveAttribute('autocomplete', 'new-password');
    await waitFor(() => expect(pw).toHaveFocus());
    await choose('a calm river at dawn');

    expect(await screen.findByRole('heading', { level: 1, name: S.authTitle })).toBeInTheDocument();
    expect(step()).toBe('Step 3 of 4');
    const qr = screen.getByRole('img', { name: S.qrLabel('ravi@contoso.example') });
    expect(qr.tagName.toLowerCase()).toBe('svg');
    expect(screen.getByText(S.apps)).toBeInTheDocument();
    const key = screen.getByText(/^[A-Z2-7]{4}( [A-Z2-7]{4}){7}$/);
    expect(key.tagName.toLowerCase()).toBe('code');
    await userEvent.click(screen.getByRole('button', { name: S.added }));

    expect(await screen.findByRole('heading', { level: 1, name: S.confirmTitle })).toBeInTheDocument();
    expect(step()).toBe('Step 4 of 4');
    await userEvent.type(screen.getByLabelText(P.codeLabel), '000000');
    await userEvent.click(screen.getByRole('button', { name: S.confirm }));
    expect(await screen.findByRole('alert')).toHaveTextContent(S.codeFailed(4));
    await userEvent.type(screen.getByLabelText(P.codeLabel), MOCK_TOTP);
    await userEvent.click(screen.getByRole('button', { name: S.confirm }));

    expect(await screen.findByRole('heading', { level: 1, name: S.codesTitle })).toBeInTheDocument();
    const list = screen.getByRole('list', { name: P.codes.label });
    expect(within(list).getAllByRole('listitem')).toHaveLength(10);
    // The QR code and its key are gone with the step.
    expect(screen.queryByRole('img', { name: /QR code/ })).toBeNull();
    expect(screen.queryByText(S.resetDone)).toBeNull();
    const go = screen.getByRole('button', { name: S.continue });
    expect(go).toBeDisabled();
    await userEvent.click(screen.getByRole('checkbox', { name: P.codes.saved }));
    expect(go).toBeEnabled();
    await userEvent.click(go);
    expect(await screen.findByLabelText(copy.home.askLabel)).toBeInTheDocument();
    expect(window.location.hash).toBe('#/');
  });

  it('says a reset replaces everything, before and after', async () => {
    open();
    await begin('dana@contoso.example', MOCK_SETUP_CODES.dana);
    expect(await screen.findByText(S.resetNote)).toBeInTheDocument();
    await choose('a calm river at dawn');
    await userEvent.click(await screen.findByRole('button', { name: S.added }));
    await userEvent.type(await screen.findByLabelText(P.codeLabel), MOCK_TOTP);
    await userEvent.click(screen.getByRole('button', { name: S.confirm }));
    expect(await screen.findByText(S.resetDone)).toBeInTheDocument();
  });

  it('says one uniform sentence for a wrong code and an unknown name', async () => {
    open();
    await begin('ravi@contoso.example', 'AAAAA-BBBBB-CCCCC-DDDDD');
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(S.setup_failed);
    await waitFor(() => expect(alert.closest('.pw-note')).toHaveFocus());
    const text = alert.textContent;
    await userEvent.clear(screen.getByLabelText(P.nameLabel));
    await begin('nobody@contoso.example', MOCK_SETUP_CODES.ravi);
    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe(text));
  });

  it.each([
    ['too_short', 'short one'],
    ['matches_name', 'ravi@contoso.example'],
    ['common', 'password1234'],
  ])('words a refused password (%s) in its own sentence', async (reason, password) => {
    open();
    await begin();
    await choose(password);
    expect(await screen.findByRole('alert')).toHaveTextContent(weakText(reason, { min_length: 12, max_length: 128 }));
    expect(step()).toBe('Step 2 of 4');
  });

  it('words too_long from the server', async () => {
    open({ passwordRefusal: 'weak_password:too_long' });
    await begin();
    await choose('a calm river at dawn');
    expect(await screen.findByRole('alert')).toHaveTextContent(P.weak.too_long(128));
  });

  it('starts again cleanly when the setup expired', async () => {
    open({ passwordRefusal: 'setup_expired' });
    await begin();
    await choose('a calm river at dawn');
    expect(await screen.findByRole('alert')).toHaveTextContent(S.setup_expired);
    expect(step()).toBe('Step 1 of 4');
    expect(screen.getByLabelText(P.nameLabel)).toHaveValue('ravi@contoso.example');
    expect(screen.getByLabelText(S.codeLabel)).toHaveValue('');
    // The same code works again.
    await userEvent.type(screen.getByLabelText(S.codeLabel), MOCK_SETUP_CODES.ravi);
    await userEvent.click(screen.getByRole('button', { name: S.begin }));
    expect(await screen.findByRole('heading', { level: 1, name: S.passwordTitle })).toBeInTheDocument();
  });

  it('starts again cleanly when the steps got out of order', async () => {
    open({ passwordRefusal: 'wrong_step' });
    await begin();
    await choose('a calm river at dawn');
    expect(await screen.findByRole('alert')).toHaveTextContent(S.wrong_step);
    expect(step()).toBe('Step 1 of 4');
  });

  it.each(['host_only', 'sign_in_unconfirmed', 'not_a_member', 'evals_account'])('says what to do for %s at the first step', async (code) => {
    open({ passwordRefusal: code });
    await begin();
    expect(await screen.findByRole('alert')).toHaveTextContent(S[code as 'host_only']);
  });

  it('makes a new QR code on request and says to scan it again', async () => {
    open();
    await begin();
    await choose('a calm river at dawn');
    const first = (await screen.findByText(/^[A-Z2-7]{4}( [A-Z2-7]{4}){7}$/)).textContent;
    await userEvent.click(screen.getByRole('button', { name: S.newQr }));
    expect(await screen.findByText(S.newQrDone)).toBeInTheDocument();
    expect(screen.getByText(/^[A-Z2-7]{4}( [A-Z2-7]{4}){7}$/).textContent).not.toBe(first);
  });

  // The re-check of eb24d28: the authenticator secret is shown, never kept. It is in no web
  // storage, no cookie the page can read, the address bar or the history's state, before or
  // after the setup finishes; nor is the setup code or the new password.
  it('keeps the authenticator secret out of web storage, cookies and history', async () => {
    open();
    await screen.findByLabelText(P.nameLabel);
    const before = window.history.length;
    await begin();
    await choose('a calm river at dawn');
    const shown = (await screen.findByText(/^[A-Z2-7]{4}( [A-Z2-7]{4}){7}$/)).textContent!;
    const secret = shown.replace(/ /g, '');
    const qr = screen.getByRole('img', { name: S.qrLabel('ravi@contoso.example') });
    expect(qr.outerHTML).not.toContain(secret);
    const leaks = () => {
      const everywhere = [
        ...Object.keys(localStorage).map((k) => `${k}=${localStorage.getItem(k)}`),
        ...Object.keys(sessionStorage).map((k) => `${k}=${sessionStorage.getItem(k)}`),
        document.cookie,
        window.location.href,
        JSON.stringify(window.history.state ?? null),
      ].join('\n');
      return [secret, secret.toLowerCase(), 'otpauth', MOCK_SETUP_CODES.ravi, 'a calm river at dawn'].filter((s) =>
        everywhere.includes(s),
      );
    };
    expect(leaks()).toEqual([]);
    await userEvent.click(screen.getByRole('button', { name: S.newQr }));
    await screen.findByText(S.newQrDone);
    await userEvent.click(screen.getByRole('button', { name: S.added }));
    await userEvent.type(await screen.findByLabelText(P.codeLabel), MOCK_TOTP);
    await userEvent.click(screen.getByRole('button', { name: S.confirm }));
    expect(await screen.findByRole('heading', { level: 1, name: S.codesTitle })).toBeInTheDocument();
    expect(leaks()).toEqual([]);
    // No history entry was pushed for a step: Back can't bring a secret back.
    expect(window.history.length).toBe(before);
  });

  it('starts again with a new code when the setup code is spent', async () => {
    open({ passwordRefusal: 'setup_code_spent' });
    await begin();
    expect(await screen.findByRole('alert')).toHaveTextContent(S.setup_code_spent);
    expect(step()).toBe('Step 1 of 4');
  });

  it('leaves part-way with Start over, which keeps nothing secret', async () => {
    open();
    await begin();
    await screen.findByLabelText(S.newPassword);
    await userEvent.click(screen.getByRole('button', { name: S.startOver }));
    expect(await screen.findByLabelText(S.codeLabel)).toHaveValue('');
    expect(step()).toBe('Step 1 of 4');
  });
});

describe('the QR code', () => {
  it('encodes an otpauth address with finder patterns in three corners', () => {
    const m = qrModules('otpauth://totp/PM%20OS%3Aana%40contoso.example?secret=JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP&issuer=PM%20OS&algorithm=SHA1&digits=6&period=30');
    const n = m.length;
    expect(n).toBeGreaterThanOrEqual(21);
    expect((n - 17) % 4).toBe(0);
    const finder = (r: number, c: number) => [0, 6].every((d) => m[r]![c + d] && m[r + d]![c]) && !m[r + 1]![c + 1] && m[r + 3]![c + 3];
    expect(finder(0, 0)).toBe(true);
    expect(finder(0, n - 7)).toBe(true);
    expect(finder(n - 7, 0)).toBe(true);
  });
});
