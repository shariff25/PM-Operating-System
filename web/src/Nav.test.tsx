import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { copy } from './copy';
import { renderApp } from './test/util';
import { parseRoute, taskIdFromPath } from './url';

const nav = () => screen.getByRole('navigation', { name: copy.nav.label });
const navLink = (name: string) => within(nav()).getByRole('link', { name: new RegExp(`^${name}`) });

describe('navigation', () => {
  it('marks the current page with aria-current and moves it when you navigate', async () => {
    const user = userEvent.setup();
    renderApp();
    await screen.findByLabelText(copy.home.askLabel);
    expect(navLink(copy.nav.home)).toHaveAttribute('aria-current', 'page');
    expect(navLink(copy.nav.drafts)).not.toHaveAttribute('aria-current');

    await user.click(navLink(copy.nav.drafts));
    await screen.findByRole('heading', { level: 1, name: copy.draftsPage.title });
    expect(navLink(copy.nav.drafts)).toHaveAttribute('aria-current', 'page');
    expect(navLink(copy.nav.home)).not.toHaveAttribute('aria-current');
    expect(within(nav()).getAllByRole('link').filter((l) => l.getAttribute('aria-current') === 'page')).toHaveLength(1);

    await user.click(navLink(copy.nav.activity));
    await screen.findByRole('heading', { level: 1, name: copy.activity.title });
    expect(navLink(copy.nav.activity)).toHaveAttribute('aria-current', 'page');
  });

  it('keeps Drafts current inside a draft and Activity current inside a thread', async () => {
    renderApp({ hash: '/d/apr_prd' });
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(navLink(copy.nav.drafts)).toHaveAttribute('aria-current', 'page');
    window.location.hash = '/a/tsk_seed_week';
    await screen.findByText('Billing this week');
    expect(navLink(copy.nav.activity)).toHaveAttribute('aria-current', 'page');
  });

  it('shows count badges with words for screen readers', async () => {
    renderApp();
    await waitFor(() => expect(navLink(copy.nav.drafts)).toHaveAccessibleName(`${copy.nav.drafts}, ${copy.nav.draftsCount(3)}`));
    await waitFor(() => expect(navLink(copy.nav.followups)).toHaveAccessibleName(`${copy.nav.followups}, ${copy.nav.followupsCount(2)}`));
  });

  it('collapses the sidebar to icons and back', async () => {
    const user = userEvent.setup();
    renderApp();
    const toggle = await screen.findByRole('button', { name: copy.nav.collapse });
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    await user.click(toggle);
    expect(screen.getByRole('button', { name: copy.nav.expand })).toHaveAttribute('aria-expanded', 'false');
    // labels are still there for screen readers
    expect(navLink(copy.nav.drafts)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: copy.nav.expand }));
  });

  it('sends unknown and malformed routes home', async () => {
    renderApp({ hash: '/nowhere' });
    await screen.findByLabelText(copy.home.askLabel);
    expect(parseRoute('/a/..')).toEqual({ name: 'home' });
    expect(parseRoute('/a/%E0')).toEqual({ name: 'home' });
    expect(parseRoute('/drafts/x')).toEqual({ name: 'home' });
    expect(parseRoute('/a/tsk_1')).toEqual({ name: 'task', id: 'tsk_1' });
    expect(parseRoute('/followups')).toEqual({ name: 'followups' });
    expect(taskIdFromPath('/a/a%2Fb')).toBeNull();
  });

  it('shows home for #/followups when the brief is not built', async () => {
    renderApp({ hash: '/followups', briefBuilt: false });
    await screen.findByLabelText(copy.home.askLabel);
    expect(screen.queryByRole('heading', { name: copy.home.waitingOnYou })).not.toBeInTheDocument();
  });
});

describe('command palette', () => {
  const palette = () => screen.getByRole('dialog', { name: copy.palette.label });
  const input = () => screen.getByRole('combobox', { name: copy.palette.inputLabel });

  it('opens with Ctrl+K, traps focus, closes with Escape and returns focus', async () => {
    const user = userEvent.setup();
    renderApp();
    const box = await screen.findByLabelText(copy.home.askLabel);
    box.focus();
    await user.keyboard('{Control>}k{/Control}');
    expect(palette()).toBeInTheDocument();
    expect(input()).toHaveFocus();
    await user.tab();
    expect(input()).toHaveFocus();
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(box).toHaveFocus();
  });

  it('opens from the top bar button', async () => {
    const user = userEvent.setup();
    renderApp();
    await user.click(await screen.findByRole('button', { name: new RegExp(copy.nav.askButton) }));
    expect(palette()).toBeInTheDocument();
  });

  it('asks the bot from anywhere and opens the thread', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/drafts' });
    await screen.findByRole('heading', { level: 1, name: copy.draftsPage.title });
    await user.keyboard('{Meta>}k{/Meta}');
    await user.type(input(), 'what changed in billing');
    const first = within(palette()).getAllByRole('option')[0]!;
    expect(first).toHaveTextContent(copy.palette.ask('what changed in billing'));
    expect(first).toHaveAttribute('aria-selected', 'true');
    await user.keyboard('{Enter}');
    await screen.findByText(copy.task.status.done);
    expect(window.location.hash).toMatch(/^#\/a\/tsk_/);
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('jumps to a page and to a draft by title with the keyboard', async () => {
    const user = userEvent.setup();
    renderApp();
    await screen.findByText('PRD: TCB grace period setting');
    await user.keyboard('{Control>}k{/Control}');
    await user.type(input(), 'activ');
    await user.keyboard('{Enter}');
    await screen.findByRole('heading', { level: 1, name: copy.activity.title });

    await user.keyboard('{Control>}k{/Control}');
    await user.type(input(), 'grace period banner');
    // the ask option comes first; arrow down to the draft
    await user.keyboard('{ArrowDown}');
    expect(within(palette()).getByRole('option', { selected: true })).toHaveTextContent('Show grace period banner to workspace admins');
    expect(input()).toHaveAttribute('aria-activedescendant', 'pal-d-apr_jira');
    await user.keyboard('{Enter}');
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(window.location.hash).toBe('#/d/apr_jira');
  });
});

describe('phone layout', () => {
  afterEach(() => {
    // jsdom has no matchMedia; remove the stub again
    delete (window as { matchMedia?: unknown }).matchMedia;
  });

  it('shows a bottom tab bar with the same items and the user menu behind the avatar', async () => {
    window.matchMedia = vi.fn().mockImplementation((query: string) => ({
      matches: query.includes('max-width'),
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
    })) as unknown as typeof window.matchMedia;
    const user = userEvent.setup();
    renderApp();
    await screen.findByLabelText(copy.home.askLabel);
    const bar = nav();
    expect(bar).toHaveClass('tabbar');
    expect(within(bar).getAllByRole('link').map((l) => l.textContent?.split(',')[0])).toEqual([
      copy.nav.home,
      copy.nav.drafts,
      copy.nav.followups,
      copy.nav.activity,
    ]);
    expect(document.querySelector('.sidebar')).toBeNull();
    await user.click(within(bar).getByRole('link', { name: new RegExp(`^${copy.nav.drafts}`) }));
    expect(within(nav()).getByRole('link', { name: new RegExp(`^${copy.nav.drafts}`) })).toHaveAttribute('aria-current', 'page');
    await user.click(screen.getByRole('button', { name: /Menu for/ }));
    expect(screen.getByRole('menu')).toBeInTheDocument();
  });

  it('shows the draft panel as tabs with the document as the first', async () => {
    window.matchMedia = vi.fn().mockImplementation((query: string) => ({
      matches: query.includes('max-width'),
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
    })) as unknown as typeof window.matchMedia;
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd' });
    const tabs = await screen.findByRole('tablist', { name: copy.panel.tabsLabel });
    expect(within(tabs).getAllByRole('tab').map((t) => t.textContent)).toEqual([copy.panel.doc, `${copy.panel.sources}5`, copy.panel.ask, copy.panel.details]);
    await user.click(within(tabs).getByRole('tab', { name: copy.panel.doc }));
    expect(screen.getByRole('article', { name: copy.draft.formattedLabel })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Approve to Confluence' })).toBeInTheDocument();
  });
});
