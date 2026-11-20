import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { ApiError } from '../api';
import type { Api } from '../api';
import { copy } from '../copy';
import { renderApp } from '../test/util';

const open = (id: string, extra = {}) => renderApp({ hash: `/d/${id}`, ...extra });
const edit = async (user: ReturnType<typeof userEvent.setup>) =>
  user.click(await screen.findByRole('button', { name: copy.draft.edit }));
const formatted = () => screen.getByRole('article', { name: copy.draft.formattedLabel });
// The change box is docked under the document.
const changeBox = async () => screen.findByLabelText(copy.draft.askLabel);

describe('draft workspace', () => {
  it('opens as the formatted document with an Edit button, and no editor', async () => {
    open('apr_prd');
    const doc = await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(within(doc).getByRole('heading', { level: 1, name: 'PRD: TCB grace period setting' })).toBeInTheDocument();
    expect(within(doc).getByRole('heading', { level: 2, name: 'Problem' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: copy.draft.edit })).toBeInTheDocument();
    expect(screen.queryByLabelText(copy.draft.bodyLabel)).not.toBeInTheDocument();
    expect(screen.getByText(copy.draft.saved)).toBeInTheDocument();
  });

  it('edits and saves, shows saved or unsaved, and returns to the formatted view', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    await edit(user);
    const title = screen.getByLabelText(copy.draft.titleLabel);
    expect(screen.getByRole('button', { name: copy.draft.save })).toBeDisabled();

    await user.type(title, ' v2');
    expect(screen.getByText(copy.draft.unsaved)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: copy.draft.save }));
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(screen.getByText(copy.draft.saved)).toBeInTheDocument();
    expect(screen.queryByLabelText(copy.draft.titleLabel)).not.toBeInTheDocument();
    expect(within(formatted()).getByRole('heading', { level: 1 })).toHaveTextContent('PRD: TCB grace period setting v2');
  });

  it('discarding an edit returns to the formatted view with the saved text', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    await edit(user);
    await user.type(screen.getByLabelText(copy.draft.bodyLabel), 'SCRATCH');
    await user.click(screen.getByRole('button', { name: copy.draft.discard }));
    expect(formatted()).not.toHaveTextContent('SCRATCH');
    expect(screen.getByText(copy.draft.saved)).toBeInTheDocument();
  });

  it('shows five sources, highlights the selected one, and an honest empty state', async () => {
    const user = userEvent.setup();
    const { unmount } = open('apr_prd');
    const panel = await screen.findByRole('region', { name: copy.draft.sourcesHeading });
    // numbered cards, with the count beside the heading
    expect(panel).toHaveTextContent(copy.draft.sourcesCount(5));
    expect(within(panel).getByRole('button', { name: /PAY-212/ })).toHaveAccessibleName(/^Source 2: ?PAY-212: Payment failure handling, ?Jira$/);
    const buttons = within(panel).getAllByRole('button', { pressed: false });
    expect(buttons.filter((b) => b.className.includes('source'))).toHaveLength(5);
    await user.click(within(panel).getByRole('button', { name: /PAY-212/ }));
    expect(within(panel).getByRole('button', { name: /PAY-212/ })).toHaveAttribute('aria-pressed', 'true');
    expect(within(panel).getByRole('link', { name: /Open in source/ })).toHaveAttribute('href', 'https://example.com/jira/PAY-212');
    unmount();

    open('apr_slack');
    await screen.findByText(copy.noSources);
  });

  it('accepts a suggestion after showing a before and after', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    await user.type(await changeBox(), 'shorter');
    await user.click(screen.getByRole('button', { name: copy.draft.askButton }));
    const region = await screen.findByRole('region', { name: copy.draft.suggestionHeading });
    expect(within(region).getAllByText(copy.draft.diffRemoved).length).toBeGreaterThan(0);
    // the change is shown in the document itself, with the title and the unchanged text around it
    expect(screen.queryByRole('article', { name: copy.draft.formattedLabel })).not.toBeInTheDocument();
    expect(within(region).getByRole('heading', { level: 1, name: 'PRD: TCB grace period setting' })).toBeInTheDocument();
    expect(within(region).getByRole('heading', { level: 2, name: 'Proposal' })).toBeInTheDocument();
    expect(within(region).getByText(copy.draft.suggestionBar('shorter'))).toBeInTheDocument();
    await user.click(within(region).getByRole('button', { name: copy.draft.accept }));
    await waitFor(() => expect(screen.queryByRole('region', { name: copy.draft.suggestionHeading })).not.toBeInTheDocument());
    expect(formatted()).not.toHaveTextContent('Who gets the banner');
    expect(formatted()).toHaveTextContent('Revenue recognition');
  });

  it('rejects a suggestion and leaves the draft unchanged', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    const before = (await screen.findByRole('article', { name: copy.draft.formattedLabel })).textContent;
    await user.type(await changeBox(), 'add a legal risk');
    await user.click(screen.getByRole('button', { name: copy.draft.askButton }));
    const region = await screen.findByRole('region', { name: copy.draft.suggestionHeading });
    expect(within(region).getAllByText(copy.draft.diffAdded).length).toBeGreaterThan(0);
    await user.click(within(region).getByRole('button', { name: copy.draft.rejectSuggestion }));
    await waitFor(() => expect(screen.queryByRole('region', { name: copy.draft.suggestionHeading })).not.toBeInTheDocument());
    expect(formatted().textContent).toBe(before);
  });

  it('explains a failed suggestion plainly', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    await user.type(await changeBox(), 'make it fail');
    await user.click(screen.getByRole('button', { name: copy.draft.askButton }));
    expect(await screen.findByRole('alert')).toHaveTextContent(/I could not make that change/);
  });
});

describe('draft side panel', () => {
  it('shows what the bot read, a checklist from the real state, and the details beside the document', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(screen.queryByRole('tablist')).not.toBeInTheDocument();
    const check = screen.getByRole('region', { name: copy.draft.checklistHeading });
    expect(check).toHaveTextContent(copy.draft.checklist.sources(5));
    expect(check).toHaveTextContent(copy.draft.checklist.internal);
    expect(check).toHaveTextContent(copy.draft.checklist.noExpiry);
    expect(check).not.toHaveTextContent(copy.draft.checklist.suggestion);
    const details = screen.getByRole('region', { name: copy.panel.details });
    expect(details).toHaveTextContent('pm-ana');
    expect(details).toHaveTextContent(copy.panel.never);
    expect(within(details).getByRole('link', { name: copy.panel.openTask })).toHaveAttribute('href', '#/a/tsk_seed_prd');
    expect(within(details).getByText('apr_prd')).toBeInTheDocument();
    // editing shows up in the checklist
    await edit(user);
    expect(check).toHaveTextContent(copy.draft.checklist.unsaved);
  });

  it('says so in the checklist when a draft has no sources and goes outside the company', async () => {
    open('apr_slack');
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    const check = screen.getByRole('region', { name: copy.draft.checklistHeading });
    expect(check).toHaveTextContent(copy.draft.checklist.noSources);
    expect(check).toHaveTextContent(copy.draft.checklist.external);
    expect(check).toHaveTextContent(/Expires soon: \d+ h left/);
  });

  it('shows the breadcrumb, and a toast when the draft is saved', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    const crumbs = screen.getByRole('navigation', { name: copy.panel.breadcrumb });
    expect(within(crumbs).getByRole('link', { name: copy.nav.drafts })).toHaveAttribute('href', '#/drafts');
    expect(within(crumbs).getByText('PRD: TCB grace period setting')).toHaveAttribute('aria-current', 'page');
    await edit(user);
    await user.type(screen.getByLabelText(copy.draft.bodyLabel), ' more');
    await user.click(screen.getByRole('button', { name: copy.draft.save }));
    await screen.findByText(copy.toast.saved);
  });

  it('a Slack draft shows the time left, and the expired one offers no change box and no checklist', async () => {
    const { unmount } = open('apr_slack');
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(screen.getAllByText(/\d+ h left/).length).toBeGreaterThan(0);
    unmount();
    open('apr_slack_old');
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(screen.queryByLabelText(copy.draft.askLabel)).not.toBeInTheDocument();
    expect(screen.queryByRole('region', { name: copy.draft.checklistHeading })).not.toBeInTheDocument();
  });
});

describe('approving', () => {
  it('approves only after a confirmation with the title, place, name and link destinations', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    await user.click(await screen.findByRole('button', { name: 'Approve to Confluence' }));
    const confirm = screen.getByRole('dialog', { name: copy.approve.confirmHeading });
    expect(confirm).toHaveTextContent('create a Confluence page in Space PM, under “Specs”, under your name (Ana Silva)');
    expect(within(confirm).getByText('PRD: TCB grace period setting')).toBeInTheDocument();
    expect(within(confirm).getByText('Ana Silva', { selector: 'dd' })).toBeInTheDocument();
    const links = within(confirm).getByRole('list', { name: copy.approve.confirmLinks });
    expect(links).toHaveTextContent('example.com');
    expect(links).toHaveTextContent('https://example.com/wiki/billing-roadmap');
    // still pending until confirmed
    expect(screen.queryByText(copy.doneFirstApproval)).not.toBeInTheDocument();
    await user.click(within(confirm).getByRole('button', { name: copy.approve.confirm('confluence_page') }));
    await screen.findByText(copy.doneFirstApproval);
    expect(screen.getByRole('link', { name: copy.approve.resultLink })).toHaveAttribute('href', expect.stringContaining('example.com/confluence'));
  });

  it('renders the confirmation outside the draft, and says so when there are no links', async () => {
    const user = userEvent.setup();
    open('apr_jira');
    await user.click(await screen.findByRole('button', { name: 'Approve to Jira' }));
    const dialog = screen.getByRole('dialog');
    expect(dialog.closest('main')).toBeNull();
    expect(document.body.contains(dialog)).toBe(true);
    expect(within(dialog).getByText(copy.approve.confirmNoLinks)).toBeInTheDocument();
  });

  it('closes the confirmation with Escape and with Not yet', async () => {
    const user = userEvent.setup();
    open('apr_jira');
    await user.click(await screen.findByRole('button', { name: 'Approve to Jira' }));
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Approve to Jira' }));
    await user.click(screen.getByRole('button', { name: copy.approve.cancel }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('cannot approve while editing or with unsaved changes', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    await edit(user);
    const approve = screen.getByRole('button', { name: 'Approve to Confluence' });
    expect(approve).toBeDisabled();
    expect(screen.getByText(copy.draft.saveFirst)).toBeInTheDocument();
    await user.type(screen.getByLabelText(copy.draft.bodyLabel), ' more');
    expect(approve).toBeDisabled();
    await user.click(screen.getByRole('button', { name: copy.draft.save }));
    await waitFor(() => expect(screen.getByRole('button', { name: 'Approve to Confluence' })).toBeEnabled());
  });

  it('cannot approve while a suggestion is waiting for a decision', async () => {
    const user = userEvent.setup();
    const approve = vi.fn();
    open('apr_prd', {
      wrap: (a: Api) => {
        const orig = a.approve.bind(a);
        a.approve = (id, o) => { approve(); return orig(id, o); };
        return a;
      },
    });
    await user.type(await changeBox(), 'shorter');
    await user.click(screen.getByRole('button', { name: copy.draft.askButton }));
    const region = await screen.findByRole('region', { name: copy.draft.suggestionHeading });
    expect(screen.getByRole('button', { name: 'Approve to Confluence' })).toBeDisabled();
    expect(screen.getByText(copy.draft.resolveFirst)).toBeInTheDocument();
    await user.click(within(region).getByRole('button', { name: copy.draft.rejectSuggestion }));
    await waitFor(() => expect(screen.getByRole('button', { name: 'Approve to Confluence' })).toBeEnabled());
    expect(approve).not.toHaveBeenCalled();
  });

  it('sends the version it showed, and reloads with a plain message when the draft changed (409 stale)', async () => {
    const user = userEvent.setup();
    const seen: (string | undefined)[] = [];
    const { api } = open('apr_prd', {
      wrap: (a: Api) => {
        const orig = a.approve.bind(a);
        a.approve = (id, o) => { seen.push(o?.version); return orig(id, o); };
        return a;
      },
    });
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    // someone else changes the draft after the PM opened it
    await api.saveDraft('apr_prd', { body: '# Changed elsewhere\n\nA new risk.' });
    await user.click(screen.getByRole('button', { name: 'Approve to Confluence' }));
    await user.click(screen.getByRole('button', { name: copy.approve.confirm('confluence_page') }));

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(copy.approve.errors.stale.text);
    expect(seen).toEqual(['1']);
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(formatted()).toHaveTextContent('A new risk.');
    // still pending: nothing was published
    expect((await api.getDraft('apr_prd')).status).toBe('pending');

    // the second try carries the new version and goes through
    await user.click(screen.getByRole('button', { name: 'Approve to Confluence' }));
    await user.click(screen.getByRole('button', { name: copy.approve.confirm('confluence_page') }));
    await screen.findByText(copy.doneFirstApproval);
    expect(seen).toEqual(['1', '2']);
  });

  it('asks for an extra confirmation on external drafts before sending confirm_external', async () => {
    const user = userEvent.setup();
    const approve = vi.fn();
    open('apr_slack', {
      wrap: (a: Api) => {
        const orig = a.approve.bind(a);
        a.approve = (id, o) => { approve(id, o); return orig(id, o); };
        return a;
      },
    });
    await user.click(await screen.findByRole('button', { name: 'Approve to Slack' }));
    const send = screen.getByRole('button', { name: copy.approve.confirmExternalButton });
    expect(send).toBeDisabled();
    expect(screen.getByText(copy.approve.externalWarning)).toBeInTheDocument();
    await user.click(screen.getByRole('checkbox', { name: copy.approve.externalCheckbox }));
    await user.click(send);
    await screen.findByText(copy.approve.approvedWhere('slack_reply', 'A Slack thread'));
    expect(approve).toHaveBeenCalledWith('apr_slack', { confirmExternal: true, version: '1' });
  });

  it('handles 409 needs_external_confirm from the server by asking for the tick', async () => {
    const user = userEvent.setup();
    open('apr_prd', {
      wrap: (a: Api) => {
        const orig = a.approve.bind(a);
        let first = true;
        a.approve = async (id, o) => {
          if (first) {
            first = false;
            throw new ApiError(409, 'needs_external_confirm', 'This channel is shared outside the company.');
          }
          return orig(id, o);
        };
        return a;
      },
    });
    await user.click(await screen.findByRole('button', { name: 'Approve to Confluence' }));
    await user.click(screen.getByRole('button', { name: copy.approve.confirm('confluence_page') }));
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(copy.approve.errors.needs_external_confirm.next);
    const tick = screen.getByRole('checkbox', { name: copy.approve.externalCheckbox });
    await user.click(tick);
    await user.click(screen.getByRole('button', { name: copy.approve.confirmExternalButton }));
    await screen.findByText(copy.approve.approvedWhere('confluence_page', 'Space PM, under “Specs”'));
  });

  it('says what happened when the server reports expiry, and offers one next step', async () => {
    const user = userEvent.setup();
    open('apr_slack', {
      wrap: (a: Api) => {
        a.approve = async () => {
          throw new ApiError(409, 'expired', 'This Slack draft expired after 24 hours, so it was not sent.');
        };
        return a;
      },
    });
    await user.click(await screen.findByRole('button', { name: 'Approve to Slack' }));
    await user.click(screen.getByRole('checkbox', { name: copy.approve.externalCheckbox }));
    await user.click(screen.getByRole('button', { name: copy.approve.confirmExternalButton }));
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(/expired/);
    expect(within(alert).getByRole('button', { name: copy.approve.goHome })).toBeInTheDocument();
  });

  it('rejects with a fixed reason', async () => {
    const user = userEvent.setup();
    open('apr_jira');
    await user.click(await screen.findByRole('button', { name: copy.reject.button }));
    expect(screen.getAllByRole('radio')).toHaveLength(6);
    await user.click(screen.getByRole('radio', { name: copy.reject.reasons.tone }));
    await user.click(screen.getByRole('button', { name: copy.reject.confirm }));
    await screen.findByText(copy.approve.rejectedNote);
    expect(screen.queryByRole('button', { name: /^Approve/ })).not.toBeInTheDocument();
  });

  it('practice draft: approving creates nothing and no link appears', async () => {
    const user = userEvent.setup();
    renderApp({ noDrafts: true });
    // a first visit with no drafts makes the practice draft; it is listed on Today, and opens from there
    await user.click(await screen.findByRole('link', { name: /Practice: why PMs should take the stairs/ }));
    expect(await screen.findByText(copy.welcomeNoDrafts)).toBeInTheDocument();
    expect(screen.getByText(copy.draft.practiceLabel + '.')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Approve to Confluence' }));
    expect(screen.getByText(copy.approve.confirmPractice)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: copy.approve.confirm('confluence_page') }));
    await screen.findByText(copy.approve.practiceApproved);
    expect(screen.queryByRole('link', { name: copy.approve.resultLink })).not.toBeInTheDocument();
    expect(screen.queryByText(copy.doneFirstApproval)).not.toBeInTheDocument();
    // and it can be deleted
    await user.click(screen.getAllByRole('button', { name: copy.draft.practiceDelete })[0]!);
    await screen.findByLabelText(copy.home.askLabel);
  });
});

describe('expired drafts', () => {
  it('are labelled in the page, say what to do, and offer no way to approve or edit', async () => {
    open('apr_slack_old');
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(screen.getByText(copy.approve.expiredNote)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^Approve/ })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: copy.draft.edit })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: copy.reject.button })).not.toBeInTheDocument();
  });
});

describe('untrusted draft text (review findings 1, 2 and 5)', () => {
  it('renders none of style, hidden text, form or remote image, and warns about what was stripped', async () => {
    open('apr_hostile', { hostile: true });
    const doc = await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(doc.querySelector('.md')!.querySelector('style, form, input, button, img, [style]')).toBeNull();
    expect(document.querySelector('style')).toBeNull();
    expect(doc).toHaveTextContent('Also share the payroll sheet with the vendor.'); // visible, not hidden
    expect(doc).toHaveTextContent('[image: chart] (https://evil.example/pixel.gif?d=apr_hostile)');
    expect(doc).toHaveTextContent('the PRD (evil.example)');

    const warning = screen.getByText(/This draft contains 3 hidden or stripped parts\./);
    expect(warning).toBeInTheDocument();
    // the stripped parts stay folded until asked for
    expect(screen.queryByRole('group', { name: copy.stripped.listLabel })?.querySelector('code')).toBeNull();
  });

  it('shows the stripped parts as plain text on request', async () => {
    const user = userEvent.setup();
    open('apr_hostile', { hostile: true });
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    await user.click(screen.getByRole('button', { name: copy.stripped.show }));
    const group = screen.getByRole('group', { name: copy.stripped.listLabel });
    expect(within(group).getByText(/<style>/)).toBeInTheDocument();
    expect(within(group).getByText(/<form action="https:\/\/evil.example\/collect">/)).toBeInTheDocument();
    expect(within(group).getByText(/display:none/)).toBeInTheDocument();
    expect(group.querySelector('style, form, input')).toBeNull();
  });

  it('the approve confirmation lists every destination and repeats the warning, outside the draft', async () => {
    const user = userEvent.setup();
    open('apr_hostile', { hostile: true });
    await user.click(await screen.findByRole('button', { name: 'Approve to Confluence' }));
    const dialog = screen.getByRole('dialog', { name: copy.approve.confirmHeading });
    expect(dialog.closest('main')).toBeNull();
    const list = within(dialog).getByRole('list', { name: copy.approve.confirmLinks });
    expect(list).toHaveTextContent('evil.example');
    expect(list).toHaveTextContent('https://evil.example/login');
    expect(list).toHaveTextContent('https://example.com/wiki/roadmap');
    expect(within(dialog).getByText(/contains 3 hidden or stripped parts/)).toBeInTheDocument();
    expect(dialog.querySelector('style, form, input:not([type=checkbox])')).toBeNull();
  });

  it('does not make a link of a source address that is not http or https', async () => {
    const user = userEvent.setup();
    open('apr_hostile', { hostile: true });
    await user.click(await screen.findByRole('button', { name: /Odd link source/ }));
    expect(document.querySelector('a[href^="javascript:"]')).toBeNull();
    expect(screen.getByText(/not a web link/)).toBeInTheDocument();
  });

  it('shows a result address that is not http or https as text', async () => {
    renderApp({
      hash: '/d/apr_done',
      wrap: (a: Api) => {
        const orig = a.getDraft.bind(a);
        a.getDraft = async (id) => ({ ...(await orig(id)), result_url: 'javascript:alert(1)' });
        return a;
      },
    });
    await screen.findByText(copy.approve.approvedWhere('confluence_page', 'Space PM, under “Updates”'));
    expect(document.querySelector('a[href^="javascript:"]')).toBeNull();
    expect(screen.getByText(/not a web link/)).toBeInTheDocument();
  });
});

describe('the outcome banner after approving', () => {
  it('has no Open it button when the draft has no result link', async () => {
    renderApp({
      hash: '/d/apr_done',
      wrap: (a: Api) => {
        const orig = a.getDraft.bind(a);
        a.getDraft = async (id) => ({ ...(await orig(id)), result_url: null });
        return a;
      },
    });
    await screen.findByText(copy.approve.approvedNoLink);
    expect(screen.queryByRole('link', { name: copy.approve.resultLink })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: copy.approve.resultLink })).not.toBeInTheDocument();
  });

  it('has the Open it link when the draft has one', async () => {
    open('apr_done');
    expect(await screen.findByRole('link', { name: copy.approve.resultLink })).toHaveAttribute('href', 'https://example.com/wiki/weekly-9-nov');
  });
});
