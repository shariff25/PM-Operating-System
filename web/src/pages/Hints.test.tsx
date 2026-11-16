import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { copy } from '../copy';
import { renderApp } from '../test/util';

describe('hints', () => {
  it('shows each hint in place once, and records the dismissal', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd' });
    await screen.findByText(copy.hints.sources);
    expect(screen.getByText(copy.hints.ask)).toBeInTheDocument();
    expect(screen.getByText(copy.hints.approve)).toBeInTheDocument();

    const sources = document.querySelector('[data-hint="sources"]') as HTMLElement;
    await user.click(sources.querySelector('button')!);
    await waitFor(() => expect(screen.queryByText(copy.hints.sources)).not.toBeInTheDocument());
    expect(screen.getByText(copy.hints.ask)).toBeInTheDocument();
  });

  it('does not bring a dismissed hint back on the next visit', async () => {
    const user = userEvent.setup();
    const first = renderApp({ hash: '/d/apr_prd' });
    await screen.findByText(copy.hints.ask);
    await user.click((document.querySelector('[data-hint="ask"]') as HTMLElement).querySelector('button')!);
    await waitFor(() => expect(screen.queryByText(copy.hints.ask)).not.toBeInTheDocument());
    const me = await first.api.me();
    expect(me.hints_seen).toEqual(['ask']);
  });

  it('hides hints when nudges are off and brings them back with Show hints again', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd' });
    await screen.findByText(copy.hints.approve);

    await user.click(screen.getByRole('button', { name: /Menu for/ }));
    await user.click(screen.getByRole('switch', { name: copy.menu.nudges }));
    await waitFor(() => expect(screen.queryByText(copy.hints.approve)).not.toBeInTheDocument());
    expect(screen.queryByText(copy.hints.ask)).not.toBeInTheDocument();
    expect(screen.queryByText(copy.hints.sources)).not.toBeInTheDocument();

    await user.click(screen.getByRole('switch', { name: copy.menu.nudges }));
    await screen.findByText(copy.hints.approve);

    // dismiss one, then reset
    await user.click((document.querySelector('[data-hint="approve"]') as HTMLElement).querySelector('button')!);
    await waitFor(() => expect(screen.queryByText(copy.hints.approve)).not.toBeInTheDocument());
    await user.click(screen.getByRole('button', { name: copy.menu.showHints }));
    await screen.findByText(copy.hints.approve);
  });

  it('signs out from the menu', async () => {
    const user = userEvent.setup();
    renderApp();
    await user.click(await screen.findByRole('button', { name: /Menu for/ }));
    await user.click(screen.getByRole('button', { name: copy.menu.signOut }));
    await screen.findByLabelText(copy.signIn.handleLabel);
  });
});
