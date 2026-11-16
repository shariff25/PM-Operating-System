import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import type { Api } from '../api';
import { copy } from '../copy';
import { renderApp } from '../test/util';

const menuItems = () => [...document.querySelectorAll<HTMLElement>('[role="menu"] [role^="menuitem"]')];
const visible = () => [...document.querySelectorAll('[data-hint]')].map((e) => e.getAttribute('data-hint'));
const dismiss = async (user: ReturnType<typeof userEvent.setup>, name: string) =>
  user.click((document.querySelector(`[data-hint="${name}"]`) as HTMLElement).querySelector('button')!);

describe('hints', () => {
  it('shows one hint at a time, in the order sources, ask, approve', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd' });
    await screen.findByText(copy.hints.sources);
    expect(visible()).toEqual(['sources']);

    await dismiss(user, 'sources');
    await screen.findByText(copy.hints.ask);
    expect(visible()).toEqual(['ask']);

    await dismiss(user, 'ask');
    await screen.findByText(copy.hints.approve);
    expect(visible()).toEqual(['approve']);

    await dismiss(user, 'approve');
    await waitFor(() => expect(visible()).toEqual([]));
  });

  it('records each dismissal with the server and never brings a dismissed hint back', async () => {
    const user = userEvent.setup();
    const seen = vi.fn();
    const first = renderApp({
      hash: '/d/apr_prd',
      wrap: (a: Api) => {
        const orig = a.markHintSeen.bind(a);
        a.markHintSeen = (n) => { seen(n); return orig(n); };
        return a;
      },
    });
    await screen.findByText(copy.hints.sources);
    await dismiss(user, 'sources');
    await screen.findByText(copy.hints.ask);
    expect(seen).toHaveBeenCalledWith('sources');
    expect((await first.api.me()).hints_seen).toEqual(['sources']);

    // leave and come back: the dismissed hint stays gone, the next one is still waiting
    await user.click(screen.getByRole('link', { name: copy.nav.home }));
    await screen.findByLabelText(copy.home.askLabel);
    window.location.hash = '/d/apr_jira';
    await screen.findByText(copy.hints.ask);
    expect(screen.queryByText(copy.hints.sources)).not.toBeInTheDocument();
    expect(visible()).toEqual(['ask']);
  });

  it('hides hints when nudges are off and brings them back with Show hints again', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd' });
    await screen.findByText(copy.hints.sources);

    await user.click(screen.getByRole('button', { name: /Menu for/ }));
    await user.click(screen.getByRole('menuitemcheckbox', { name: /Hints and nudges/ }));
    await waitFor(() => expect(visible()).toEqual([]));

    await user.click(screen.getByRole('menuitemcheckbox', { name: /Hints and nudges/ }));
    await screen.findByText(copy.hints.sources);

    // dismiss two, then reset: the order starts again at the first
    await dismiss(user, 'sources');
    await dismiss(user, 'ask');
    await screen.findByText(copy.hints.approve);
    await user.click(screen.getByRole('button', { name: /Menu for/ }));
    await user.click(screen.getByRole('menuitem', { name: copy.menu.showHints }));
    await screen.findByText(copy.hints.sources);
    expect(visible()).toEqual(['sources']);
  });

  it('signs out from the menu', async () => {
    const user = userEvent.setup();
    renderApp();
    await user.click(await screen.findByRole('button', { name: /Menu for/ }));
    await user.click(screen.getByRole('menuitem', { name: copy.menu.signOut }));
    await screen.findByLabelText(copy.signIn.handleLabel);
  });
});

describe('user menu keyboard', () => {
  it('opens with ArrowDown, moves with the arrows, and Escape returns focus to the button', async () => {
    const user = userEvent.setup();
    renderApp();
    const button = await screen.findByRole('button', { name: /Menu for/ });
    expect(button).toHaveAttribute('aria-haspopup', 'menu');
    button.focus();
    await user.keyboard('{ArrowDown}');
    expect(button).toHaveAttribute('aria-expanded', 'true');
    expect(screen.getByRole('menu')).toBeInTheDocument();
    const items = menuItems();
    // theme (system, light, dark), nudges, show hints again, sign out
    expect(items).toHaveLength(6);
    expect(items[0]).toHaveFocus();
    await user.keyboard('{ArrowDown}');
    expect(items[1]).toHaveFocus();
    await user.keyboard('{End}');
    expect(items[5]).toHaveFocus();
    await user.keyboard('{ArrowDown}');
    expect(items[0]).toHaveFocus();
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('menu')).not.toBeInTheDocument();
    expect(button).toHaveFocus();
  });

  it('opens with Enter, and ArrowUp goes to the last item', async () => {
    const user = userEvent.setup();
    renderApp();
    const button = await screen.findByRole('button', { name: /Menu for/ });
    button.focus();
    await user.keyboard('{Enter}');
    expect(menuItems()[0]).toHaveFocus();
    await user.keyboard('{ArrowUp}');
    expect(menuItems()[5]).toHaveFocus();
  });

  it('closes when you click elsewhere', async () => {
    const user = userEvent.setup();
    renderApp();
    await user.click(await screen.findByRole('button', { name: /Menu for/ }));
    expect(screen.getByRole('menu')).toBeInTheDocument();
    await user.click(screen.getByRole('heading', { level: 1 }));
    expect(screen.queryByRole('menu')).not.toBeInTheDocument();
  });
});

describe('theme', () => {
  it('switches between system, light and dark from the user menu, and remembers the choice', async () => {
    const user = userEvent.setup();
    window.localStorage.clear();
    renderApp();
    await user.click(await screen.findByRole('button', { name: /Menu for/ }));
    expect(screen.getByRole('menuitemradio', { name: copy.theme.system })).toHaveAttribute('aria-checked', 'true');
    expect(document.documentElement.dataset.theme).toBeUndefined();

    await user.click(screen.getByRole('menuitemradio', { name: copy.theme.dark }));
    expect(document.documentElement.dataset.theme).toBe('dark');
    expect(window.localStorage.getItem('pmos-theme')).toBe('dark');
    expect(screen.getByRole('menuitemradio', { name: copy.theme.dark })).toHaveAttribute('aria-checked', 'true');

    await user.click(screen.getByRole('menuitemradio', { name: copy.theme.light }));
    expect(document.documentElement.dataset.theme).toBe('light');

    await user.click(screen.getByRole('menuitemradio', { name: copy.theme.system }));
    expect(document.documentElement.dataset.theme).toBeUndefined();
    expect(window.localStorage.getItem('pmos-theme')).toBeNull();
  });

  it('moves between the theme choices with the arrow keys', async () => {
    const user = userEvent.setup();
    renderApp();
    const button = await screen.findByRole('button', { name: /Menu for/ });
    button.focus();
    await user.keyboard('{ArrowDown}');
    await user.keyboard('{ArrowRight}');
    expect(screen.getByRole('menuitemradio', { name: copy.theme.light })).toHaveFocus();
    await user.keyboard('{ArrowLeft}');
    expect(screen.getByRole('menuitemradio', { name: copy.theme.system })).toHaveFocus();
  });
});

describe('hints on a decided draft', () => {
  const none = async () => {
    await waitFor(() => expect(document.querySelector('[data-hint]')).toBeNull());
    expect(screen.queryByText(copy.hints.sources)).not.toBeInTheDocument();
    expect(screen.queryByText(copy.hintDismiss)).not.toBeInTheDocument();
  };
  const withStatus = (status: 'rejected' | 'failed') => (a: Api) => {
    const orig = a.getDraft.bind(a);
    a.getDraft = async (id) => ({ ...(await orig(id)), status });
    return a;
  };

  it('shows none on an approved draft', async () => {
    renderApp({ hash: '/d/apr_done' });
    await screen.findByLabelText('Outcome');
    await none();
  });

  it('shows none on an expired draft', async () => {
    renderApp({ hash: '/d/apr_slack_old' });
    await screen.findByText(copy.approve.expiredNote);
    await none();
  });

  it('shows none on a rejected or failed draft', async () => {
    const first = renderApp({ hash: '/d/apr_jira', wrap: withStatus('rejected') });
    await screen.findByText(copy.approve.rejectedNote);
    await none();
    first.unmount();
    renderApp({ hash: '/d/apr_jira', wrap: withStatus('failed') });
    await screen.findByText(copy.approve.failedNote);
    await none();
  });

  it('shows none on the phone layout either', async () => {
    window.matchMedia = vi.fn().mockImplementation((query: string) => ({
      matches: query.includes('max-width'),
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
    })) as unknown as typeof window.matchMedia;
    try {
      renderApp({ hash: '/d/apr_done' });
      await screen.findByRole('tablist');
      await none();
    } finally {
      delete (window as { matchMedia?: unknown }).matchMedia;
    }
  });

  it('still shows the first hint on a pending draft', async () => {
    renderApp({ hash: '/d/apr_jira' });
    await screen.findByText(copy.hints.sources);
  });
});
