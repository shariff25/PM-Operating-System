import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { copy } from '../copy';
import { renderApp } from '../test/util';

const visible = () => [...document.querySelectorAll('[data-hint]')].map((e) => e.getAttribute('data-hint'));
const dismiss = async (user: ReturnType<typeof userEvent.setup>, name: string) =>
  user.click((document.querySelector(`[data-hint="${name}"]`) as HTMLElement).querySelector('button')!);

describe('hints behind modal layers', () => {
  it('hides the hint while the palette is open and brings it back after', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd' });
    await screen.findByText(copy.hints.sources);
    await user.keyboard('{Control>}k{/Control}');
    await screen.findByRole('dialog');
    await waitFor(() => expect(visible()).toEqual([]));
    await user.keyboard('{Escape}');
    await waitFor(() => expect(visible()).toEqual(['sources']));
  });

  it('hides the approve hint while the approve dialog is open', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd' });
    await screen.findByText(copy.hints.sources);
    await dismiss(user, 'sources');
    await screen.findByText(copy.hints.ask);
    await dismiss(user, 'ask');
    await screen.findByText(copy.hints.approve('confluence_page'));
    await user.click(document.getElementById('approve-btn')!);
    await screen.findByRole('dialog');
    await waitFor(() => expect(visible()).toEqual([]));
    await user.keyboard('{Escape}');
    await waitFor(() => expect(visible()).toEqual(['approve']));
  });
});
