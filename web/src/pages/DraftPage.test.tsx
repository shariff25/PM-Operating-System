import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import type { Api } from '../api';
import { copy } from '../copy';
import { renderApp } from '../test/util';

const open = (id: string, extra = {}) => renderApp({ hash: `/d/${id}`, ...extra });

describe('draft workspace', () => {
  it('edits and saves, with a visible saved or unsaved state', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    const title = await screen.findByLabelText(copy.draft.titleLabel);
    expect(screen.getByText(copy.draft.saved)).toBeInTheDocument();

    await user.type(title, ' v2');
    expect(screen.getByText(copy.draft.unsaved)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: copy.draft.save }));
    await screen.findByText(copy.draft.saved);
    expect(title).toHaveValue('PRD: TCB grace period setting v2');
  });

  it('shows five sources, highlights the selected one, and an honest empty state', async () => {
    const user = userEvent.setup();
    const { unmount } = open('apr_prd');
    const panel = await screen.findByRole('complementary', { name: copy.draft.sourcesHeading });
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
    await user.type(await screen.findByLabelText(copy.draft.askLabel), 'shorter');
    await user.click(screen.getByRole('button', { name: copy.draft.askButton }));
    const region = await screen.findByRole('region', { name: copy.draft.suggestionHeading });
    expect(within(region).getAllByText(copy.draft.diffRemoved).length).toBeGreaterThan(0);
    await user.click(within(region).getByRole('button', { name: copy.draft.accept }));
    await waitFor(() => expect(screen.queryByRole('region', { name: copy.draft.suggestionHeading })).not.toBeInTheDocument());
    expect((screen.getByLabelText(copy.draft.bodyLabel) as HTMLTextAreaElement).value).not.toContain('Who gets the banner');
  });

  it('rejects a suggestion and leaves the draft unchanged', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    const before = (await screen.findByLabelText(copy.draft.bodyLabel) as HTMLTextAreaElement).value;
    await user.type(screen.getByLabelText(copy.draft.askLabel), 'add a legal risk');
    await user.click(screen.getByRole('button', { name: copy.draft.askButton }));
    const region = await screen.findByRole('region', { name: copy.draft.suggestionHeading });
    expect(within(region).getAllByText(copy.draft.diffAdded).length).toBeGreaterThan(0);
    await user.click(within(region).getByRole('button', { name: copy.draft.rejectSuggestion }));
    await waitFor(() => expect(screen.queryByRole('region', { name: copy.draft.suggestionHeading })).not.toBeInTheDocument());
    expect((screen.getByLabelText(copy.draft.bodyLabel) as HTMLTextAreaElement).value).toBe(before);
  });

  it('explains a failed suggestion plainly', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    await user.type(await screen.findByLabelText(copy.draft.askLabel), 'make it fail');
    await user.click(screen.getByRole('button', { name: copy.draft.askButton }));
    expect(await screen.findByRole('alert')).toHaveTextContent(/I could not make that change/);
  });

  it('approves only after a confirmation that says where and under whose name', async () => {
    const user = userEvent.setup();
    open('apr_prd');
    await user.click(await screen.findByRole('button', { name: 'Approve to Confluence' }));
    const confirm = screen.getByRole('group', { name: copy.approve.confirmHeading });
    expect(confirm).toHaveTextContent('create a Confluence page in Space PM, under Specs, under your name (Ana Silva)');
    // still pending until confirmed
    expect(screen.queryByText(copy.doneFirstApproval)).not.toBeInTheDocument();
    await user.click(within(confirm).getByRole('button', { name: copy.approve.confirm }));
    await screen.findByText(copy.doneFirstApproval);
    expect(screen.getByRole('link', { name: copy.approve.resultLink })).toHaveAttribute('href', expect.stringContaining('example.com/confluence'));
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
    await screen.findByText(copy.approve.approvedWhere('#partner-pricing, a channel shared with Acme Corp'));
    expect(approve).toHaveBeenCalledWith('apr_slack', { confirmExternal: true });
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
            const { ApiError } = await import('../api');
            throw new ApiError(409, 'needs_external_confirm', 'This channel is shared outside the company.');
          }
          return orig(id, o);
        };
        return a;
      },
    });
    await user.click(await screen.findByRole('button', { name: 'Approve to Confluence' }));
    await user.click(screen.getByRole('button', { name: copy.approve.confirm }));
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(copy.approve.errors.needs_external_confirm.next);
    const tick = screen.getByRole('checkbox', { name: copy.approve.externalCheckbox });
    await user.click(tick);
    await user.click(screen.getByRole('button', { name: copy.approve.confirmExternalButton }));
    await screen.findByText(copy.approve.approvedWhere('Space PM, under Specs'));
  });

  it('says what happened when a draft has expired, and offers one next step', async () => {
    const user = userEvent.setup();
    open('apr_slack_old');
    await user.click(await screen.findByRole('button', { name: 'Approve to Slack' }));
    await user.click(screen.getByRole('button', { name: copy.approve.confirmSlack }));
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(/expired/);
    expect(within(alert).getByRole('button', { name: copy.approve.goHome })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Approve to Slack' })).not.toBeInTheDocument();
    await user.click(within(alert).getByRole('button', { name: copy.approve.goHome }));
    await screen.findByLabelText(copy.home.askLabel);
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
    // first visit with no drafts opens the practice draft by itself
    expect(await screen.findByText(copy.welcomeNoDrafts)).toBeInTheDocument();
    expect(screen.getByText(copy.draft.practiceLabel + '.')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Approve to Confluence' }));
    expect(screen.getByText(copy.approve.confirmPractice)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: copy.approve.confirm }));
    await screen.findByText(copy.approve.practiceApproved);
    expect(screen.queryByRole('link', { name: copy.approve.resultLink })).not.toBeInTheDocument();
    expect(screen.queryByText(copy.doneFirstApproval)).not.toBeInTheDocument();
    // and it can be deleted
    await user.click(screen.getAllByRole('button', { name: copy.draft.practiceDelete })[0]!);
    await screen.findByLabelText(copy.home.askLabel);
  });
});
