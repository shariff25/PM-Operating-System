import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { copy } from '../../copy';
import { parseRoute } from '../../url';
import { renderApp } from '../../test/util';
import { suggestTools, toolIssues } from './Policy';

const A = copy.admin;
const T = A.policy;
const B = T.bots;
const E = T.editor;
const P = A.propose;

const cards = () => [...document.querySelectorAll<HTMLElement>('.bot-card')];
const toggleOf = (card: HTMLElement) => within(card).queryByRole('button', { name: new RegExp(`${B.showDetails}|${B.hideDetails}`) });

async function overview() {
  renderApp({ role: 'admin', hash: '/admin/policy' });
  await screen.findByRole('heading', { name: new RegExp(`^${B.heading}`) });
}

describe('the policy page is split into parts', () => {
  it('reads the part from the address', () => {
    expect(parseRoute('/admin/policy')).toMatchObject({ name: 'admin', section: 'policy' });
    expect(parseRoute('/admin/policy')).not.toHaveProperty('sub');
    expect(parseRoute('/admin/policy/propose')).toMatchObject({ section: 'policy', sub: 'propose' });
    expect(parseRoute('/admin/policy/proposals')).toMatchObject({ section: 'policy', sub: 'proposals' });
    expect(parseRoute('/admin/policy/other')).toEqual({ name: 'home' });
  });

  it('keeps the overview for reading and moves the form to its own part, reflected in the address', async () => {
    const user = userEvent.setup();
    await overview();
    expect(screen.queryByRole('heading', { name: E.heading })).not.toBeInTheDocument();
    const switcher = screen.getByRole('navigation', { name: T.parts.label });
    expect(within(switcher).getAllByRole('link').map((l) => l.textContent)).toEqual([T.parts.overview, T.parts.propose, T.parts.proposals]);
    expect(within(switcher).getByRole('link', { name: T.parts.overview })).toHaveAttribute('aria-current', 'page');

    await user.click(within(switcher).getByRole('link', { name: T.parts.propose }));
    expect(await screen.findByRole('heading', { name: E.heading })).toBeInTheDocument();
    expect(window.location.hash).toBe('#/admin/policy/propose');
    expect(screen.queryByRole('heading', { name: new RegExp(`^${B.heading}`) })).not.toBeInTheDocument();

    await user.click(within(switcher).getByRole('link', { name: T.parts.proposals }));
    expect(await screen.findByRole('heading', { name: new RegExp(`^${A.proposals.heading}`) })).toBeInTheDocument();
    expect(window.location.hash).toBe('#/admin/policy/proposals');
    expect(screen.queryByRole('heading', { name: E.heading })).not.toBeInTheDocument();
  });

  it('shows an auditor no form: the propose address falls back to the overview', async () => {
    renderApp({ role: 'auditor', hash: '/admin/policy/propose' });
    await screen.findByRole('heading', { name: new RegExp(`^${B.heading}`) });
    const switcher = screen.getByRole('navigation', { name: T.parts.label });
    expect(within(switcher).queryByRole('link', { name: T.parts.propose })).not.toBeInTheDocument();
  });

  it('keeps a half-made change when the admin looks at the overview and comes back', async () => {
    const user = userEvent.setup();
    renderApp({ role: 'admin', hash: '/admin/policy/propose' });
    await user.click(await screen.findByRole('tab', { name: E.tabs.skills }));
    const input = screen.getByLabelText(E.toolsOf('docs'));
    await user.type(input, ' slack.search');
    window.location.hash = '/admin/policy';
    await screen.findByRole('heading', { name: new RegExp(`^${B.heading}`) });
    window.location.hash = '/admin/policy/propose';
    await screen.findByRole('heading', { name: E.heading });
    expect(screen.getByLabelText(E.toolsOf('docs'))).toHaveValue('docs.search docs.read slack.search');
  });
});

describe('compact bot cards', () => {
  it('shows the header, role, owner, skills and tools, and keeps the rest behind a Show details button', async () => {
    const user = userEvent.setup();
    await overview();
    const card = cards().find((c) => within(c).queryByRole('heading', { name: 'docs' }))!;
    expect(within(card).getByText(/\d+ tools?, \d+ destinations?, writes to \d+ places?/)).toBeInTheDocument();
    expect(within(card).getByText(B.role)).toBeInTheDocument();
    expect(within(card).getByText(B.owner)).toBeInTheDocument();
    expect(within(card).getByText(B.tools)).toBeInTheDocument();
    for (const w of [B.talksTo, B.reads, B.writes]) expect(within(card).getByText(w)).not.toBeVisible();

    const toggle = toggleOf(card)!;
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    await user.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    for (const w of [B.talksTo, B.reads, B.writes]) expect(within(card).getByText(w)).toBeVisible();
    await user.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(within(card).getByText(B.talksTo)).not.toBeVisible();
  });

  it('opens a card with a broken rule by default, and keeps its red flag when it is collapsed', async () => {
    const user = userEvent.setup();
    await overview();
    const flagged = cards().find((c) => within(c).queryByRole('heading', { name: 'pm-ben' }))!;
    expect(toggleOf(flagged)).toHaveAttribute('aria-expanded', 'true');
    expect(within(flagged).getByText(B.ruleBroken)).toBeInTheDocument();
    await user.click(toggleOf(flagged)!);
    expect(toggleOf(flagged)).toHaveAttribute('aria-expanded', 'false');
    expect(within(flagged).getByText(B.ruleBroken)).toBeVisible();
    // The quiet cards start closed.
    const quiet = cards().filter((c) => c !== flagged);
    expect(quiet.length).toBeGreaterThan(0);
    for (const c of quiet) expect(toggleOf(c)).toHaveAttribute('aria-expanded', 'false');
  });

  it('expands and collapses every card at once', async () => {
    const user = userEvent.setup();
    await overview();
    await user.click(screen.getByRole('button', { name: B.expandAll }));
    for (const c of cards()) expect(toggleOf(c)).toHaveAttribute('aria-expanded', 'true');
    await user.click(screen.getByRole('button', { name: B.collapseAll }));
    for (const c of cards()) expect(toggleOf(c)).toHaveAttribute('aria-expanded', 'false');
  });
});

describe('typing tool names', () => {
  it('finds unknown and doubled names against the names PM OS already uses', () => {
    const known = new Set(['jira.search', 'docs.read']);
    expect(toolIssues(['jira.serach', 'docs.read', 'docs.read'], known)).toEqual({ unknown: ['jira.serach'], twice: ['docs.read'] });
    // With nothing to compare against, nothing is flagged.
    expect(toolIssues(['anything'], new Set())).toEqual({ unknown: [], twice: [] });
  });

  it('completes only the word being typed, and not words already listed', () => {
    const known = ['docs.read', 'docs.search', 'jira.search'];
    expect(suggestTools('docs.read jira.s', known)).toEqual(['jira.search']);
    expect(suggestTools('doc', known)).toEqual(['docs.read', 'docs.search']);
    expect(suggestTools('docs.read d', known)).toEqual(['docs.search']);
    expect(suggestTools('docs.read ', known)).toEqual([]);
    expect(suggestTools('jira.search', known)).toEqual([]);
  });

  async function skillsTab() {
    const user = userEvent.setup();
    renderApp({ role: 'admin', hash: '/admin/policy/propose' });
    await user.click(await screen.findByRole('tab', { name: E.tabs.skills }));
    const input = screen.getByLabelText(E.toolsOf('docs'));
    await user.clear(input);
    return { user, input };
  }

  it('underlines an unknown name with a plain message, and the change cannot be checked', async () => {
    const { user, input } = await skillsTab();
    await user.type(input, 'docs.read jira.serach');
    expect(input).toHaveAttribute('aria-invalid', 'true');
    const note = screen.getByText((_, el) => el?.tagName === 'LI' && el.textContent === E.toolUnknown('jira.serach'));
    expect(note).toHaveTextContent("jira.serach isn't a tool PM OS knows");
    expect(within(note).getByText('jira.serach')).toHaveClass('tool-bad');
    expect(screen.getByRole('button', { name: P.check })).toBeDisabled();
    expect(screen.getByText(E.toolsBlocked)).toBeInTheDocument();
    // Fixing the name lifts the block.
    await user.clear(input);
    await user.type(input, 'docs.read jira.search');
    expect(input).not.toHaveAttribute('aria-invalid');
    expect(screen.queryByText(E.toolsBlocked)).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: P.check })).toBeEnabled();
  });

  it('flags a name listed twice without blocking the check', async () => {
    const { user, input } = await skillsTab();
    await user.type(input, 'docs.read docs.read jira.search');
    expect(screen.getByText((_, el) => el?.tagName === 'LI' && el.textContent === E.toolTwice('docs.read'))).toHaveTextContent('docs.read is listed twice');
    expect(screen.getByRole('button', { name: P.check })).toBeEnabled();
  });

  it('suggests a completion and takes it from the keyboard', async () => {
    const { user, input } = await skillsTab();
    await user.type(input, 'docs.read jira.s');
    const list = await screen.findByRole('listbox', { name: E.toolSuggestions });
    expect(within(list).getAllByRole('option').map((o) => o.textContent)).toEqual(['jira.search']);
    expect(input).toHaveAttribute('aria-expanded', 'true');
    await user.keyboard('{ArrowDown}');
    expect(within(list).getByRole('option', { name: 'jira.search' })).toHaveAttribute('aria-selected', 'true');
    await user.keyboard('{Enter}');
    await waitFor(() => expect(input).toHaveValue('docs.read jira.search '));
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: P.check })).toBeEnabled();
  });

  it('does not trap Tab or take over Enter when no suggestion is picked, and Escape closes the list', async () => {
    const { user, input } = await skillsTab();
    await user.type(input, 'jira.s');
    await screen.findByRole('listbox');
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    expect(input).toHaveValue('jira.s');
    await user.type(input, 'e');
    await screen.findByRole('listbox');
    await user.keyboard('{Tab}');
    expect(input).toHaveValue('jira.se');
    expect(input).not.toHaveFocus();
  });
});
