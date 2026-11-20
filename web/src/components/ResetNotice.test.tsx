import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import type { Api, PasswordResetNotice } from '../api';
import { copy } from '../copy';
import { renderApp } from '../test/util';
import { resetBy } from './ResetNotice';

const R = copy.password.resetNotice;

/** The app with a reset notice on Ana's status until she dismisses it; counts the dismissals. */
function withNotice(notice: PasswordResetNotice, hash = '/') {
  const seen = { dismissed: 0 };
  renderApp({
    providers: ['password', 'dev'],
    hash,
    wrap: (api: Api) => {
      let live: PasswordResetNotice | null = notice;
      const status = api.passwordStatus.bind(api);
      api.passwordStatus = async () => ({ ...(await status()), reset_notice: live });
      api.passwordDismissResetNotice = async () => {
        seen.dismissed += 1;
        live = null;
      };
      return api;
    },
  });
  return seen;
}

describe('the reset notice', () => {
  it('names who reset the sign-in and when, until it is dismissed', async () => {
    const seen = withNotice({ by: 'ada', by_name: 'Ada Lovelace', at: '2026-11-20T08:00:00Z' });
    const note = await screen.findByTestId('reset-notice');
    expect(note).toHaveTextContent('Your sign-in was reset by Ada Lovelace on');
    expect(note).toHaveTextContent('If this was not you, tell an admin.');
    await userEvent.click(screen.getByRole('button', { name: R.dismiss }));
    await waitFor(() => expect(screen.queryByTestId('reset-notice')).toBeNull());
    expect(seen.dismissed).toBe(1);
  });

  it('shows on the security page too', async () => {
    withNotice({ by: 'host', by_name: null, at: '2026-11-20T08:00:00Z' }, '/security');
    expect(await screen.findByTestId('reset-notice')).toHaveTextContent(`reset by ${R.host} on`);
  });

  it('shows nothing without a notice', async () => {
    renderApp({ providers: ['password', 'dev'] });
    await screen.findByRole('main');
    expect(screen.queryByTestId('reset-notice')).toBeNull();
  });

  it('says plainly who did it', () => {
    expect(resetBy({ by: 'host', by_name: null, at: '' })).toBe(R.host);
    expect(resetBy({ by: 'ada', by_name: '  ', at: '' })).toBe('ada');
    expect(resetBy({ by: 'ada', by_name: 'Ada', at: '' })).toBe('Ada');
  });
});
