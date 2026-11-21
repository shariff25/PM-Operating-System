import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import type { Api } from '../../api';
import { MOCK_PASSWORDS, MOCK_RECOVERY_CODES, MOCK_TOTP } from '../../api/mockPassword';
import { copy } from '../../copy';
import { aboutWait, cleanCode, waitClock } from '../../components/password';
import { renderApp } from '../../test/util';

const P = copy.password;
const ANA = 'ana@contoso.example';

function open(opts: Parameters<typeof renderApp>[0] = {}) {
  return renderApp({ signedIn: false, providers: ['entra', 'password', 'dev'], ...opts });
}

async function passwordStep(name = ANA, password: string = MOCK_PASSWORDS.ana) {
  await userEvent.type(await screen.findByLabelText(P.nameLabel), name);
  await userEvent.type(screen.getByLabelText(P.passwordLabel), password);
  await userEvent.click(screen.getByRole('button', { name: P.submit }));
}

const codeField = () => screen.findByLabelText(P.codeLabel);

describe('the sign-in screen with password sign-in', () => {
  it('lists the providers in the server order: Microsoft, then password, then development', async () => {
    open();
    const ms = await screen.findByRole('link', { name: copy.signIn.microsoft });
    const name = screen.getByLabelText(P.nameLabel);
    const handle = screen.getByLabelText(copy.signIn.handleLabel);
    expect(ms.compareDocumentPosition(name) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(name.compareDocumentPosition(handle) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(screen.getByRole('separator')).toHaveTextContent(copy.signIn.or);
    // Microsoft is the main action; the password button and the handle button are secondary.
    expect(screen.getByRole('button', { name: P.submit })).not.toHaveClass('btn-primary');
    expect(screen.getByRole('button', { name: copy.signIn.submit })).not.toHaveClass('btn-primary');
    expect(screen.getByRole('link', { name: P.setupLink })).toHaveAttribute('href', '#/setup');
  });

  it('makes the password the main action when it is the only provider', async () => {
    open({ providers: ['password'] });
    expect(await screen.findByRole('button', { name: P.submit })).toHaveClass('btn-primary');
    expect(screen.queryByRole('link', { name: copy.signIn.microsoft })).toBeNull();
    expect(screen.queryByLabelText(copy.signIn.handleLabel)).toBeNull();
  });

  it('uses the autocomplete values password managers expect, and a password field', async () => {
    open({ providers: ['password'] });
    const name = await screen.findByLabelText(P.nameLabel);
    expect(name).toHaveAttribute('autocomplete', 'username');
    const pw = screen.getByLabelText(P.passwordLabel);
    expect(pw).toHaveAttribute('type', 'password');
    expect(pw).toHaveAttribute('autocomplete', 'current-password');
    await userEvent.click(screen.getByRole('button', { name: P.showLabel }));
    expect(pw).toHaveAttribute('type', 'text');
    await userEvent.click(screen.getByRole('button', { name: P.hideLabel }));
    expect(pw).toHaveAttribute('type', 'password');
  });

  it('says one uniform sentence for a wrong password and for an unknown name, and moves focus to it', async () => {
    open({ providers: ['password'] });
    await passwordStep(ANA, 'not-the-right-one');
    const first = await screen.findByRole('alert');
    expect(first).toHaveTextContent(P.refusals.sign_in_failed);
    expect(first.closest('.pw-note')).toHaveFocus();
    expect(screen.getByLabelText(P.passwordLabel)).toHaveValue('');
    const text1 = first.textContent;

    await userEvent.clear(screen.getByLabelText(P.nameLabel));
    await passwordStep('nobody@contoso.example', 'whatever-whatever');
    await waitFor(() => expect(screen.getByRole('alert').textContent).toBe(text1));
    expect(document.body.textContent).not.toMatch(/sign_in_failed|no such|exist|not found/i);
  });

  it('goes to the code step and never submits a partial code', async () => {
    const view = open({ providers: ['password'] });
    const spy = vi.spyOn(view.api, 'passwordSecondStep');
    await passwordStep();
    const field = await codeField();
    await waitFor(() => expect(field).toHaveFocus());
    expect(field).toHaveAttribute('inputmode', 'numeric');
    expect(field).toHaveAttribute('autocomplete', 'one-time-code');
    await userEvent.type(field, '2468');
    expect(screen.getByRole('button', { name: P.verify })).toBeDisabled();
    expect(screen.getByText(P.codeIncomplete(4))).toBeInTheDocument();
    fireEvent.submit(field.closest('form')!);
    await userEvent.type(field, '{enter}');
    expect(spy).not.toHaveBeenCalled();
    // The sign-in card shows only this step now: no Microsoft link, no other form.
    expect(screen.queryByRole('link', { name: copy.signIn.microsoft })).toBeNull();
  });

  it('takes a pasted code with spaces, and signs in', async () => {
    open({ providers: ['password'] });
    await passwordStep();
    const field = await codeField();
    await userEvent.click(field);
    await userEvent.paste(`${MOCK_TOTP.slice(0, 3)} ${MOCK_TOTP.slice(3)}`);
    expect(field).toHaveValue(MOCK_TOTP);
    await userEvent.click(screen.getByRole('button', { name: P.verify }));
    expect(await screen.findByLabelText(copy.home.askLabel)).toBeInTheDocument();
  });

  it('says how many tries are left, and after the last one starts again from the password', async () => {
    open({ providers: ['password'] });
    await passwordStep();
    for (const left of [2, 1]) {
      await userEvent.type(await codeField(), '111111');
      await userEvent.click(screen.getByRole('button', { name: P.verify }));
      expect(await screen.findByRole('alert')).toHaveTextContent(P.refusals.codeFailed(left));
    }
    await userEvent.type(await codeField(), '111111');
    await userEvent.click(screen.getByRole('button', { name: P.verify }));
    expect(await screen.findByRole('alert')).toHaveTextContent(P.refusals.lastTry);
    expect(screen.getByLabelText(P.nameLabel)).toHaveValue(ANA);
    expect(screen.getByLabelText(P.passwordLabel)).toHaveValue('');
  });

  it('starts again with a plain line when the sign-in expired', async () => {
    open({ providers: ['password'], passwordRefusal: 'sign_in_expired' });
    await passwordStep();
    await userEvent.type(await codeField(), MOCK_TOTP);
    await userEvent.click(screen.getByRole('button', { name: P.verify }));
    expect(await screen.findByRole('alert')).toHaveTextContent(P.refusals.sign_in_expired);
    expect(screen.getByLabelText(P.passwordLabel)).toBeInTheDocument();
  });

  it('starts again when the person was turned off since the password', async () => {
    open({ providers: ['password'], passwordRefusal: 'not_a_member' });
    await passwordStep();
    await userEvent.type(await codeField(), MOCK_TOTP);
    await userEvent.click(screen.getByRole('button', { name: P.verify }));
    expect(await screen.findByRole('alert')).toHaveTextContent(P.refusals.not_a_member);
  });

  it('asks to try again in a moment when sign-in is busy', async () => {
    open({ providers: ['password'], passwordRefusal: 'sign_in_busy' });
    await passwordStep();
    expect(await screen.findByRole('alert')).toHaveTextContent(P.refusals.sign_in_busy);
    // It went no further, and the next try works.
    await userEvent.clear(screen.getByLabelText(P.passwordLabel));
    await userEvent.type(screen.getByLabelText(P.passwordLabel), MOCK_PASSWORDS.ana);
    await userEvent.click(screen.getByRole('button', { name: P.submit }));
    expect(await codeField()).toBeInTheDocument();
  });

  it.each(['users_unavailable', 'not_configured', 'assertion_required'])('says %s in its own words', async (code) => {
    open({ providers: ['password'], passwordRefusal: code });
    await passwordStep();
    expect(await screen.findByRole('alert')).toHaveTextContent(P.refusals[code as 'not_configured']);
  });

  it('waits out too many tries with a countdown, said once, then says it is over', async () => {
    open({ providers: ['password'], passwordRefusal: 'too_many_attempts', retryAfter: 2 });
    await passwordStep();
    const note = await screen.findByTestId('wait-note');
    expect(within(note).getByRole('alert')).toHaveTextContent(P.wait.start(aboutWait(2)));
    // The ticking clock is for the eye only.
    expect(note.querySelector('.pw-wait-clock')).toHaveAttribute('aria-hidden', 'true');
    expect(screen.getByRole('button', { name: P.submit })).toBeDisabled();
    expect(await screen.findByText(P.wait.over, {}, { timeout: 4000 })).toBeInTheDocument();
    expect(screen.queryByTestId('wait-note')).toBeNull();
    expect(screen.getByRole('button', { name: P.submit })).toBeEnabled();
  });

  it('signs in with a recovery code, then says how many are left and where new ones are made', async () => {
    open({ providers: ['password'] });
    await passwordStep();
    await userEvent.click(await screen.findByRole('button', { name: P.useRecovery }));
    const field = await screen.findByLabelText(P.recoveryLabel);
    await waitFor(() => expect(field).toHaveFocus());
    await userEvent.type(field, MOCK_RECOVERY_CODES[0]!.toUpperCase());
    await userEvent.click(screen.getByRole('button', { name: P.verify }));
    expect(await screen.findByText(P.recoveredLeft(9))).toBeInTheDocument();
    expect(screen.getByText(P.recoveredWhere)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: P.makeNew }));
    expect(await screen.findByRole('heading', { level: 1, name: P.security.title })).toBeInTheDocument();
  });

  it('a used recovery code does not work twice', async () => {
    open({ providers: ['password'] });
    await passwordStep();
    await userEvent.click(await screen.findByRole('button', { name: P.useRecovery }));
    await userEvent.type(await screen.findByLabelText(P.recoveryLabel), 'nope-nope-nope-nope');
    await userEvent.click(screen.getByRole('button', { name: P.verify }));
    expect(await screen.findByRole('alert')).toHaveTextContent(P.refusals.recoveryFailed(2));
  });

  it('warns plainly when the last recovery code was used', async () => {
    open({
      providers: ['password'],
      wrap: (api: Api) => {
        const step = api.passwordSecondStep.bind(api);
        api.passwordSecondStep = async (input) => ({ ...(await step(input)), recovery_codes_left: 0 });
        return api;
      },
    });
    await passwordStep();
    await userEvent.click(await screen.findByRole('button', { name: P.useRecovery }));
    await userEvent.type(await screen.findByLabelText(P.recoveryLabel), MOCK_RECOVERY_CODES[1]!);
    await userEvent.click(screen.getByRole('button', { name: P.verify }));
    expect(await screen.findByText(P.recoveredNone)).toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: P.continue }));
    expect(await screen.findByLabelText(copy.home.askLabel)).toBeInTheDocument();
  });

  it('Start over goes back to the password with nothing secret kept', async () => {
    open({ providers: ['password'] });
    await passwordStep();
    await userEvent.type(await codeField(), '123');
    await userEvent.click(screen.getByRole('button', { name: P.startOver }));
    expect(await screen.findByLabelText(P.passwordLabel)).toHaveValue('');
    await waitFor(() => expect(screen.getByLabelText(P.nameLabel)).toHaveFocus());
  });
});

describe('password helpers', () => {
  it('reads six digits out of whatever is pasted', () => {
    expect(cleanCode('123 456')).toBe('123456');
    expect(cleanCode('123-456-789')).toBe('123456');
    expect(cleanCode('abc')).toBe('');
  });
  it('says a wait in words once, and as a clock', () => {
    expect(aboutWait(30)).toBe('30 seconds');
    expect(aboutWait(95)).toBe('about 2 minutes');
    expect(aboutWait(60)).toBe('about 1 minute');
    expect(waitClock(65)).toBe('1:05');
    expect(waitClock(0)).toBe('0:00');
  });
});
