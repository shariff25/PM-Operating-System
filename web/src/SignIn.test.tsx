import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import type { Api, AuthProvider } from './api';
import { createMockClient } from './api/mock';
import { App } from './App';
import { copy } from './copy';
import { loginHref, signedOutReason } from './url';

function open(providers: AuthProvider[] | 'fail' | undefined, hash = '') {
  window.location.hash = hash;
  render(
    <App
      pollMs={5}
      createApi={(onSignedOut) => {
        const api: Api = createMockClient({
          latency: 0,
          stepMs: 0,
          onSignedOut,
          providers: providers === 'fail' ? undefined : providers,
        });
        if (providers === 'fail') api.providers = () => Promise.reject(new Error('down'));
        return api;
      }}
    />,
  );
}

describe('sign-in providers', () => {
  it('shows only the handle form by default, as the main action', async () => {
    open(undefined);
    expect(await screen.findByLabelText(copy.signIn.handleLabel)).toBeInTheDocument();
    expect(screen.queryByRole('link', { name: copy.signIn.microsoft })).toBeNull();
    expect(screen.getByRole('button', { name: copy.signIn.submit })).toHaveClass('btn-primary');
  });

  it('shows only the Microsoft link when only entra is on', async () => {
    open(['entra']);
    const link = await screen.findByRole('link', { name: copy.signIn.microsoft });
    expect(link).toHaveAttribute('href', '/api/auth/login');
    expect(screen.queryByLabelText(copy.signIn.handleLabel)).toBeNull();
  });

  it('shows both, with the handle form secondary', async () => {
    open(['entra', 'dev']);
    expect(await screen.findByRole('link', { name: copy.signIn.microsoft })).toHaveClass('btn-primary');
    expect(screen.getByLabelText(copy.signIn.handleLabel)).toBeInTheDocument();
    expect(screen.getByText(copy.signIn.devHeading)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: copy.signIn.submit })).not.toHaveClass('btn-primary');
  });

  it('says sign-in is not set up when nothing is listed or the call fails', async () => {
    open([]);
    expect(await screen.findByText(copy.signIn.notSetUp)).toBeInTheDocument();
    expect(screen.queryByLabelText(copy.signIn.handleLabel)).toBeNull();
  });

  it('says sign-in is not set up when the call fails', async () => {
    open('fail');
    expect(await screen.findByText(copy.signIn.notSetUp)).toBeInTheDocument();
  });

  it('asks Microsoft to return to the page the PM wanted', async () => {
    open(['entra'], '/d/apr_1');
    const link = await screen.findByRole('link', { name: copy.signIn.microsoft });
    expect(link).toHaveAttribute('href', `/api/auth/login?return_to=${encodeURIComponent('/#/d/apr_1')}`);
  });

  it('signs in with a handle when dev is on', async () => {
    open(['dev']);
    await userEvent.type(await screen.findByLabelText(copy.signIn.handleLabel), 'ana');
    await userEvent.click(screen.getByRole('button', { name: copy.signIn.submit }));
    await screen.findByLabelText(copy.home.askLabel);
  });
});

describe('loginHref and signedOutReason', () => {
  it('adds return_to only for a real page', () => {
    expect(loginHref('/')).toBe('/api/auth/login');
    expect(loginHref('/signed-out?reason=failed')).toBe('/api/auth/login');
    expect(loginHref('/a/tsk_1')).toBe('/api/auth/login?return_to=%2F%23%2Fa%2Ftsk_1');
  });

  it('reads the reason', () => {
    expect(signedOutReason('/signed-out?reason=expired')).toBe('expired');
    expect(signedOutReason('/signed-out')).toBe('');
    expect(signedOutReason('/drafts')).toBeNull();
  });
});

describe('signed-out page', () => {
  const cases: [string, string][] = [
    ['not_a_member', copy.signedOut.reasons.not_a_member!],
    ['cancelled', copy.signedOut.reasons.cancelled!],
    ['expired', copy.signedOut.reasons.expired!],
    ['unavailable', copy.signedOut.reasons.unavailable!],
    ['wrong_account', copy.signedOut.reasons.wrong_account!],
    ['failed', copy.signedOut.failed],
    ['whatever', copy.signedOut.failed],
  ];

  it.each(cases)('explains %s', async (reason, message) => {
    open(['entra'], `/signed-out?reason=${reason}`);
    expect(await screen.findByText(message)).toBeInTheDocument();
  });

  it('never prints a raw reason, even a hostile one', async () => {
    open(['entra'], '/signed-out?reason=%3Cb%3Eoops%3C%2Fb%3E');
    expect(await screen.findByText(copy.signedOut.failed)).toBeInTheDocument();
    expect(document.body.textContent).not.toContain('oops');
  });

  it('does not treat inherited object keys as reasons', async () => {
    open(['entra'], '/signed-out?reason=constructor');
    expect(await screen.findByText(copy.signedOut.failed)).toBeInTheDocument();
  });

  it('Try again goes back to sign-in', async () => {
    open(['entra'], '/signed-out?reason=expired');
    await userEvent.click(await screen.findByRole('button', { name: copy.signedOut.tryAgain }));
    expect(await screen.findByRole('link', { name: copy.signIn.microsoft })).toBeInTheDocument();
  });
});
