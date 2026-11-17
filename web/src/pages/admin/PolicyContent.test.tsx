import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import type { Api } from '../../api';
import { copy } from '../../copy';
import { renderApp } from '../../test/util';

const A = copy.admin;
const P = A.propose;
const nav = () => screen.getByRole('navigation', { name: copy.nav.label });
const BOTS_H = new RegExp(`^${A.policy.bots.heading}`);

/** The policy page on one of its parts. The form to propose a change is on `propose`, the default. */
async function openPolicy(opts: Parameters<typeof renderApp>[0] = {}, part: 'overview' | 'propose' | 'proposals' = 'propose') {
  const view = renderApp({ role: 'admin', hash: part === 'overview' ? '/admin/policy' : `/admin/policy/${part}`, ...opts });
  if (part === 'overview') await screen.findByRole('heading', { name: BOTS_H });
  else if (part === 'propose') await screen.findByRole('heading', { name: A.policy.editor.heading });
  else await screen.findByRole('heading', { name: new RegExp(`^${A.proposals.heading}`) });
  return view;
}

/** Give the docs skill a new tool, the change every policy test proposes. */
async function addJiraTool(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole('tab', { name: A.policy.editor.tabs.skills }));
  const input = screen.getByLabelText(A.policy.editor.toolsOf('docs'));
  await user.clear(input);
  await user.type(input, 'docs.search docs.read jira.search');
}

async function checkAndOpenDialog(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole('button', { name: P.check }));
  await screen.findByRole('heading', { name: P.result.heading });
  await user.click(screen.getByRole('button', { name: P.open }));
  return screen.findByRole('dialog', { name: P.dialog.heading });
}

async function fillAndSubmit(user: ReturnType<typeof userEvent.setup>, dialog: HTMLElement) {
  await user.type(within(dialog).getByLabelText(P.dialog.title), 'Let the docs bot search Jira');
  await user.type(within(dialog).getByLabelText(P.dialog.reason), 'Support asked for it.');
  await user.click(within(dialog).getByRole('button', { name: P.dialog.submit }));
}

/** A file's buttons wait for its text to load. */
async function clickWhenReady(user: ReturnType<typeof userEvent.setup>, name: string) {
  const b = await screen.findByRole('button', { name });
  await waitFor(() => expect(b).toBeEnabled());
  await user.click(b);
}

describe('who sees policy and content', () => {
  it('lists both parts for an admin, in the rail and on the overview', async () => {
    renderApp({ role: 'admin', hash: '/admin' });
    await screen.findByRole('heading', { level: 1, name: A.sections.overview });
    const sub = within(nav()).getByRole('list', { name: A.subnavLabel });
    expect(within(sub).getAllByRole('link').map((a) => a.textContent)).toEqual(['Overview', 'Audit log', 'Team', 'Connectors', 'Policy', 'Content and harness']);
    expect(await screen.findByRole('link', { name: new RegExp(A.overview.links.policy.body) })).toHaveAttribute('href', '#/admin/policy');
    expect(await screen.findByRole('link', { name: new RegExp(A.overview.links.content.body) })).toHaveAttribute('href', '#/admin/content');
  });

  it('gives a PM neither, and a calm page on a direct visit that never asks the server', async () => {
    const calls: string[] = [];
    renderApp({
      role: 'pm',
      hash: '/admin/policy',
      wrap: (api) => {
        const f = api.adminPolicy.bind(api);
        api.adminPolicy = async () => { calls.push('policy'); return f(); };
        return api;
      },
    });
    await screen.findByRole('heading', { level: 1, name: A.noAccess.title });
    expect(within(nav()).queryByRole('link', { name: copy.nav.admin })).not.toBeInTheDocument();
    expect(calls).toEqual([]);
    window.location.hash = '/admin/content';
    await waitFor(() => expect(window.location.hash).toBe('#/admin/content'));
    expect(await screen.findByRole('heading', { level: 1, name: A.noAccess.title })).toBeInTheDocument();
  });

  it('shows an auditor the policy read-only: no editor, no check, and the reason why', async () => {
    renderApp({ role: 'auditor', hash: '/admin/policy' });
    await screen.findByRole('heading', { name: BOTS_H });
    expect(screen.getByText(A.repo.readOnly)).toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: A.policy.editor.heading })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: P.check })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: P.open })).not.toBeInTheDocument();
  });

  it('shows an auditor the content read-only, with personal and restricted files as locked rows, not errors', async () => {
    renderApp({ role: 'auditor', hash: '/admin/content' });
    const tree = await screen.findByRole('navigation', { name: A.content.tree.label });
    expect(screen.getByText(A.repo.readOnly)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: A.content.tree.newFile })).not.toBeInTheDocument();
    expect(within(tree).getAllByText(A.content.tree.lockedReason).length).toBe(3);
    // A locked file is not a button: it cannot be opened from the tree.
    expect(within(tree).queryByRole('button', { name: /Ana's notes/ })).not.toBeInTheDocument();
    expect(within(tree).getByText("Ana's notes for her bot")).toBeInTheDocument();
    // Its lint finding reads as the fixed sentence the server sends.
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    // Opening a readable file still works, without an Edit button.
    await userEvent.setup().click(within(tree).getByRole('button', { name: /^docs.*SKILL\.md/ }));
    await screen.findByText(A.content.file.facts);
    expect(screen.queryByRole('button', { name: A.content.file.edit })).not.toBeInTheDocument();
  });

  it('shows a locked note, not an error, when an auditor follows a link to a locked file', async () => {
    renderApp({ role: 'auditor', hash: `/admin/content?file=${encodeURIComponent('content/context/products/roadmap.md')}` });
    await screen.findByRole('navigation', { name: A.content.tree.label });
    expect(screen.getAllByText(A.content.tree.lockedReason).length).toBeGreaterThan(0);
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('lets an admin open a restricted file', async () => {
    renderApp({ role: 'admin', hash: `/admin/content?file=${encodeURIComponent('content/context/products/roadmap.md')}` });
    expect(await screen.findByRole('heading', { name: 'context/products/roadmap.md' })).toBeInTheDocument();
  });
});

describe('a policy change', () => {
  it('opens a pull request only after a clean check of exactly that change', async () => {
    const user = userEvent.setup();
    await openPolicy();
    const open = () => screen.getByRole('button', { name: P.open });
    expect(open()).toBeDisabled();
    await addJiraTool(user);
    expect(open()).toBeDisabled();
    expect(screen.getByText(P.needsCheck)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: P.check }));
    await screen.findByRole('heading', { name: P.result.heading });
    expect(open()).toBeEnabled();
    // Edit after the check: the result is stale and proposing stops until it is checked again.
    await user.type(screen.getByLabelText(A.policy.editor.toolsOf('docs')), ' slack.search');
    expect(open()).toBeDisabled();
    expect(screen.getAllByText(P.stale).length).toBeGreaterThan(0);
    await user.click(screen.getByRole('button', { name: P.check }));
    await waitFor(() => expect(open()).toBeEnabled());
  });

  it('shows the widening first and loud, names it again in the confirm step, and shows the result', async () => {
    const user = userEvent.setup();
    await openPolicy();
    await addJiraTool(user);
    await user.click(screen.getByRole('button', { name: P.check }));
    const wider = await screen.findByRole('region', { name: P.groups.label(P.groups.wider, 1) });
    expect(within(wider).getByText('docs may now call the tool jira_search')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: P.open }));
    const dialog = await screen.findByRole('dialog', { name: P.dialog.heading });
    expect(within(dialog).getByRole('group', { name: P.dialog.widens(1) })).toHaveTextContent('docs may now call the tool jira_search');
    // Title and reason are required.
    expect(within(dialog).getByRole('button', { name: P.dialog.submit })).toBeDisabled();
    expect(within(dialog).getByLabelText(P.dialog.title)).toHaveAttribute('maxLength', '120');
    expect(within(dialog).getByLabelText(P.dialog.reason)).toHaveAttribute('maxLength', '4000');
    await fillAndSubmit(user, dialog);
    expect(await screen.findByRole('heading', { name: P.done.heading(42) })).toBeInTheDocument();
    expect(screen.getByRole('link', { name: /Open in GitHub/ })).toHaveAttribute('href', 'https://github.com/acme/pmos/pull/42');
    expect(screen.getByText('ada-gh')).toBeInTheDocument();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    // The new pull request is listed, on the Proposals part.
    window.location.hash = '/admin/policy/proposals';
    const list = await screen.findByRole('list', { name: A.proposals.tableLabel('policy') });
    await waitFor(() => expect(within(list).getByText('PM OS policy: Let the docs bot search Jira')).toBeInTheDocument());
  });

  it('shows check errors with their spec path, and never allows proposing them', async () => {
    const user = userEvent.setup();
    await openPolicy();
    await user.click(screen.getByRole('tab', { name: A.policy.editor.tabs.raw }));
    const raw = screen.getByLabelText(A.policy.editor.rawLabel);
    fireEvent.change(raw, { target: { value: '{"bots": {"docs": {"role": "no-such-role"}}}' } });
    await user.click(screen.getByRole('button', { name: P.check }));
    const errors = await screen.findByRole('list', { name: P.result.errors });
    expect(within(errors).getAllByText(/spec\.bots\.docs\.role/).length).toBeGreaterThan(0);
    expect(screen.getByRole('button', { name: P.open })).toBeDisabled();
    expect(screen.getByText(P.fixFirst)).toBeInTheDocument();
  });

  it.each([
    ['github_not_linked'], ['stale_base'], ['policy_mismatch'], ['rate_limited'], ['not_configured'], ['unknown_outcome'],
    ['audit_unavailable'], ['upstream_unavailable'], ['invalid'], ['github_refused'], ['github_unavailable'], ['github_rate_limited'],
  ])('handles %s with its own sentence', async (code) => {
    const user = userEvent.setup();
    await openPolicy({ proposalError: code });
    await addJiraTool(user);
    const dialog = await checkAndOpenDialog(user);
    await fillAndSubmit(user, dialog);
    const alert = await within(dialog).findByRole('alert');
    expect(alert).toHaveTextContent(P.problems[code as keyof typeof P.problems].text);
    if (code === 'stale_base') expect(alert).toHaveTextContent('content/skills/docs/SKILL.md');
    if (code === 'rate_limited') expect(alert).toHaveTextContent(P.waitFor(30));
    if (code === 'unknown_outcome' || code === 'upstream_unavailable') {
      expect(alert).not.toHaveTextContent(/nothing changed/i);
      await user.click(within(alert).getByRole('button', { name: P.checkProposals }));
      await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    }
    if (code === 'invalid') expect(screen.getByRole('button', { name: P.open })).toBeDisabled();
  });

  it('tells an admin who has not linked GitHub exactly how to link it, with the command to copy', async () => {
    const user = userEvent.setup();
    await openPolicy({ proposalError: 'github_not_linked' });
    await addJiraTool(user);
    const dialog = await checkAndOpenDialog(user);
    await fillAndSubmit(user, dialog);
    const alert = await within(dialog).findByRole('alert');
    expect(alert).toHaveTextContent(P.problems.github_not_linked.next);
    const chip = within(alert).getByRole('group', { name: P.linkCommandLabel });
    expect(within(chip).getByText(P.linkCommand).tagName).toBe('CODE');
    expect(within(alert).queryByRole('link')).not.toBeInTheDocument();
    const write = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', { value: { writeText: write }, configurable: true });
    await user.click(within(chip).getByRole('button', { name: P.copyCommand }));
    expect(write).toHaveBeenCalledWith(P.linkCommand);
    expect(await within(chip).findByRole('button', { name: P.copiedCommand })).toBeInTheDocument();
  });

  it('turns GitHub not linked into a call to action and an empty proposals list', async () => {
    await openPolicy({ protection: 'not_linked' }, 'proposals');
    expect(screen.getByText(A.repo.notLinkedTitle)).toBeInTheDocument();
    expect(await screen.findByText(/Link your GitHub account to see proposals/)).toBeInTheDocument();
  });

  it('warns when the branch requires no review, or protection could not be read', async () => {
    const first = await openPolicy({ protection: 'no_reviews' }, 'overview');
    expect(screen.getByText(A.repo.unprotectedTitle)).toBeInTheDocument();
    expect(screen.getByText(A.repo.zeroReviews('main'))).toBeInTheDocument();
    first.unmount();
    await openPolicy({ protection: 'unverified' }, 'overview');
    expect(screen.getByText(A.repo.unverified('main'))).toBeInTheDocument();
  });

  it('hides the pull-request button when proposals are off, but still lets an admin check', async () => {
    const user = userEvent.setup();
    await openPolicy({ proposalsOff: true });
    expect(screen.getByText(A.repo.proposalsOff)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: P.open })).not.toBeInTheDocument();
    await addJiraTool(user);
    await user.click(screen.getByRole('button', { name: P.check }));
    expect(await screen.findByRole('heading', { name: P.result.heading })).toBeInTheDocument();
    expect(await screen.findByText(A.proposals.off)).toBeInTheDocument();
  });

  it('keeps the change when the admin leaves the page and comes back, and warns before the tab closes', async () => {
    const user = userEvent.setup();
    await openPolicy();
    const quiet = new Event('beforeunload', { cancelable: true });
    window.dispatchEvent(quiet);
    expect(quiet.defaultPrevented).toBe(false);
    await addJiraTool(user);
    const leave = new Event('beforeunload', { cancelable: true });
    window.dispatchEvent(leave);
    expect(leave.defaultPrevented).toBe(true);
    window.location.hash = '/admin/team';
    await screen.findByRole('heading', { level: 1, name: A.sections.team });
    window.location.hash = '/admin/policy/propose';
    await screen.findByRole('heading', { name: A.policy.editor.heading });
    expect(screen.getByText('skills.docs.tools')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: A.policy.editor.discard }));
    expect(screen.getByText(A.policy.editor.noChanges)).toBeInTheDocument();
    const after = new Event('beforeunload', { cancelable: true });
    window.dispatchEvent(after);
    expect(after.defaultPrevented).toBe(false);
  });

  it('edits through the raw JSON view, and names a field the server would refuse', async () => {
    const user = userEvent.setup();
    await openPolicy();
    await user.click(screen.getByRole('tab', { name: A.policy.editor.tabs.raw }));
    const raw = screen.getByLabelText(A.policy.editor.rawLabel);
    fireEvent.change(raw, { target: { value: '{"bots": {"docs": {"hosts": []}}}' } });
    expect(screen.getByRole('alert')).toHaveTextContent(A.policy.editor.rawUnknown('bots.docs.hosts'));
    expect(screen.getByRole('button', { name: P.check })).toBeDisabled();
    fireEvent.change(raw, { target: { value: '{"bots": {"docs": {"owner": "ana"}}}' } });
    expect(screen.getByText('bots.docs.owner')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: P.check })).toBeEnabled();
  });
});

describe('a content change', () => {
  const sha = 'content/skills/docs/SKILL.md';

  it('collects several files in the tray, checks them together with the policy effect, and proposes them', async () => {
    const user = userEvent.setup();
    renderApp({ role: 'admin', hash: `/admin/content?file=${encodeURIComponent(sha)}` });
    await screen.findByText(A.content.file.facts);
    await user.click(screen.getByRole('button', { name: A.content.file.edit }));
    const editor = screen.getByLabelText(A.content.file.editorLabel(sha));
    fireEvent.change(editor, { target: { value: (editor as HTMLTextAreaElement).value.replace('tools: "docs.search docs.read"', 'tools: "docs.search docs.read slack.search"') } });
    // Delete another file, and add a new one.
    window.location.hash = `/admin/content?file=${encodeURIComponent('content/templates/one-pager.md')}`;
    await screen.findByRole('heading', { name: 'templates/one-pager.md' });
    await clickWhenReady(user, A.content.file.delete);
    await user.click(screen.getByRole('button', { name: A.content.tree.newFile }));
    const path = screen.getByLabelText(A.content.add.path);
    await user.clear(path);
    await user.type(path, 'content/playbooks/handoff.md');
    await user.click(screen.getByRole('button', { name: A.content.add.add }));
    const tray = screen.getByRole('region', { name: A.content.tray.heading });
    expect(within(tray).getByText(A.content.tray.count(3))).toBeInTheDocument();
    expect(within(within(tray).getByRole('list', { name: A.content.tray.listLabel })).getAllByRole('listitem')).toHaveLength(3);
    await user.click(within(tray).getByRole('button', { name: P.checkMany }));
    await within(tray).findByRole('heading', { name: P.result.heading });
    const wider = within(tray).getByRole('region', { name: /^Wider/ });
    expect(within(wider).getByText('docs may now call the tool slack_search')).toBeInTheDocument();
    await user.click(within(tray).getByRole('button', { name: P.open }));
    const dialog = await screen.findByRole('dialog', { name: P.dialog.heading });
    expect(within(dialog).getByText(P.dialog.prTitle('content', ''))).toBeInTheDocument();
    await fillAndSubmit(user, dialog);
    expect(await screen.findByRole('heading', { name: P.done.heading(42) })).toBeInTheDocument();
    expect(screen.queryByRole('list', { name: A.content.tray.listLabel })).not.toBeInTheDocument();
  });

  it('shows lint errors with path and line, and keeps proposing closed', async () => {
    const user = userEvent.setup();
    renderApp({ role: 'admin', hash: `/admin/content?file=${encodeURIComponent(sha)}` });
    await screen.findByText(A.content.file.facts);
    await user.click(screen.getByRole('button', { name: A.content.file.edit }));
    const editor = screen.getByLabelText(A.content.file.editorLabel(sha));
    fireEvent.change(editor, { target: { value: (editor as HTMLTextAreaElement).value.replace('name: docs', 'name: [docs') } });
    await user.click(screen.getByRole('button', { name: P.checkMany }));
    const errors = await screen.findByRole('list', { name: P.result.errors });
    expect(within(errors).getByText(P.result.line(2))).toBeInTheDocument();
    expect(within(errors).getAllByText(sha).length).toBeGreaterThan(0);
    expect(screen.getByRole('button', { name: P.open })).toBeDisabled();
  });

  it('refuses a new file outside the content tree before anything is sent', async () => {
    const user = userEvent.setup();
    renderApp({ role: 'admin', hash: '/admin/content' });
    await user.click(await screen.findByRole('button', { name: A.content.tree.newFile }));
    const path = screen.getByLabelText(A.content.add.path);
    await user.clear(path);
    await user.type(path, 'policies/bots/x.yaml');
    await user.click(screen.getByRole('button', { name: A.content.add.add }));
    expect(screen.getByRole('alert')).toHaveTextContent(A.content.add.outside);
    expect(screen.queryByRole('region', { name: A.content.tray.heading })).not.toBeInTheDocument();
  });

  it('discards the tray, and warns before the tab closes only while it holds something', async () => {
    const user = userEvent.setup();
    renderApp({ role: 'admin', hash: `/admin/content?file=${encodeURIComponent('content/templates/one-pager.md')}` });
    await clickWhenReady(user, A.content.file.delete);
    const e = new Event('beforeunload', { cancelable: true });
    window.dispatchEvent(e);
    expect(e.defaultPrevented).toBe(true);
    await user.click(screen.getByRole('button', { name: A.content.tray.discardAll }));
    expect(screen.queryByRole('region', { name: A.content.tray.heading })).not.toBeInTheDocument();
    const e2 = new Event('beforeunload', { cancelable: true });
    window.dispatchEvent(e2);
    expect(e2.defaultPrevented).toBe(false);
  });

  it('shows the harness, says what the runtime has not loaded, and what is not shown', async () => {
    renderApp({ role: 'admin', hash: '/admin/content' });
    const table = await screen.findByRole('table', { name: A.content.harness.tableLabel });
    const ben = within(table).getByRole('row', { name: /pm-ben/ });
    expect(ben).toHaveTextContent(A.content.harness.notLoaded);
    expect(within(table).getByRole('row', { name: /triage/ })).toHaveTextContent('nemotron-3-nano-30b-a3b');
    expect(screen.getByText(A.content.harness.notShown)).toBeInTheDocument();
  });

  it('reads an audit_unavailable on opening a file as its own sentence', async () => {
    const { ApiError } = await import('../../api');
    renderApp({
      role: 'admin',
      hash: `/admin/content?file=${encodeURIComponent('content/context/products/roadmap.md')}`,
      wrap: (api: Api) => {
        api.adminContentFile = async () => { throw new ApiError(502, 'audit_unavailable', 'upstream words'); };
        return api;
      },
    });
    expect(await screen.findByRole('alert')).toHaveTextContent(copy.errors.codes.audit_unavailable);
  });
});
