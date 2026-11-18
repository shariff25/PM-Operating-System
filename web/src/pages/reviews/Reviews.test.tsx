import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { ApiError } from '../../api';
import type { Api } from '../../api';
import { HOSTILE_NAME, HOSTILE_NOTE } from '../../api/mockReviews';
import { copy } from '../../copy';
import { renderApp } from '../../test/util';

const R = copy.reviews;
const P = R.panel;

const nav = () => screen.getByRole('navigation', { name: copy.nav.label });
const findNav = () => screen.findByRole('navigation', { name: copy.nav.label });
const inbox = () => screen.findByRole('list', { name: R.title });
const panel = () => screen.findByRole('region', { name: P.heading });

/** Refuse the next `times` calls of one method with this error, then behave. */
function failOnce(method: keyof Api, error: unknown, times = 1) {
  return (api: Api) => {
    let left = times;
    const real = (api[method] as (...a: unknown[]) => Promise<unknown>).bind(api);
    (api as unknown as Record<string, unknown>)[method] = async (...a: unknown[]) => {
      if (left > 0) {
        left -= 1;
        throw error;
      }
      return real(...a);
    };
    return api;
  };
}

describe('reviews: who sees them', () => {
  it('a reviewer has a Reviews entry with the count of drafts waiting for advice', async () => {
    renderApp({ role: 'reviewer' });
    const link = await within(await findNav()).findByRole('link', { name: /Reviews/ });
    expect(link).toHaveAttribute('href', '#/reviews');
    // Dana's onboarding checklist and her exports issue wait; Ana's two are advised, the restricted ones can't be.
    await waitFor(() => expect(link).toHaveTextContent(copy.nav.reviewsCount(2)));
  });

  it('a PM, an admin and an auditor have none, and the route says why calmly without asking the server', async () => {
    for (const role of ['pm', 'admin', 'auditor'] as const) {
      const listReviews = vi.fn();
      const { unmount } = renderApp({
        role,
        hash: '/reviews',
        wrap: (api) => {
          api.listReviews = listReviews;
          return api;
        },
      });
      expect(await screen.findByText(R.gone.forbidden)).toBeInTheDocument();
      expect(within(nav()).queryByRole('link', { name: /Reviews/ })).not.toBeInTheDocument();
      expect(screen.queryByRole('alert')).not.toBeInTheDocument();
      expect(listReviews).not.toHaveBeenCalled();
      unmount();
    }
  });

  it('a draft page refuses the same way for someone who is not a reviewer', async () => {
    renderApp({ role: 'pm', hash: '/reviews/apr_dana_onboarding' });
    expect(await screen.findByText(R.gone.forbidden)).toBeInTheDocument();
    expect(screen.queryByText('Onboarding checklist for new workspaces')).not.toBeInTheDocument();
  });

  it('a server that refuses the inbox (403 forbidden) gets the same calm sentence', async () => {
    renderApp({ role: 'reviewer', hash: '/reviews', wrap: failOnce('listReviews', new ApiError(403, 'forbidden', 'No.'), 99) });
    expect(await screen.findByText(R.gone.forbidden)).toBeInTheDocument();
  });
});

describe('reviews: the inbox', () => {
  it('lists the assigned PMs’ drafts newest first, with owner, kind, age and the reviewer’s advice', async () => {
    renderApp({ role: 'reviewer', hash: '/reviews' });
    const rows = within(await inbox()).getAllByRole('link');
    expect(rows.map((r) => r.getAttribute('href'))).toEqual([
      '#/reviews/apr_dana_launch',
      '#/reviews/apr_dana_onboarding',
      '#/reviews/apr_slack',
      '#/reviews/apr_prd',
      '#/reviews/apr_jira',
      '#/reviews/apr_dana_exports',
    ]);
    expect(rows[1]).toHaveTextContent('Onboarding checklist for new workspaces');
    expect(rows[1]).toHaveTextContent('dana');
    expect(rows[1]).toHaveTextContent(R.state.todo);
    expect(rows[2]).toHaveTextContent('Ana Silva');
    expect(rows[2]).toHaveTextContent(R.state.approve);
    expect(rows[4]).toHaveTextContent(R.state.changes);
    expect(screen.getByText(R.who('ana and dana'))).toBeInTheDocument();
    expect(screen.getByText(R.countLine(2))).toBeInTheDocument();
  });

  it('shows a restricted draft as metadata only: no title, labelled, with why', async () => {
    renderApp({ role: 'reviewer', hash: '/reviews' });
    const row = within(await inbox()).getAllByRole('link').find((r) => r.getAttribute('href') === '#/reviews/apr_prd')!;
    expect(row).toHaveTextContent(R.restrictedTitle);
    expect(row).toHaveTextContent(R.restrictedRow);
    expect(row).toHaveTextContent(R.state.restricted);
    expect(row).not.toHaveTextContent('PRD: TCB grace period setting');
  });

  it('marks advice given on an earlier version', async () => {
    renderApp({
      role: 'reviewer',
      hash: '/reviews',
      wrap: (api) => {
        const list = api.listReviews.bind(api);
        api.listReviews = async () => {
          const l = await list();
          return { ...l, drafts: l.drafts.map((d) => (d.id === 'apr_jira' ? { ...d, my_recommendation: { ...d.my_recommendation!, stale: true } } : d)) };
        };
        return api;
      },
    });
    const row = within(await inbox()).getAllByRole('link').find((r) => r.getAttribute('href') === '#/reviews/apr_jira')!;
    expect(row).toHaveTextContent(R.state.stale);
  });

  it('says when the server left drafts out', async () => {
    renderApp({
      role: 'reviewer',
      hash: '/reviews',
      wrap: (api) => {
        const list = api.listReviews.bind(api);
        api.listReviews = async () => ({ ...(await list()), truncated: true, total: 1200 });
        return api;
      },
    });
    await inbox();
    expect(screen.getByText(R.truncated(1200, 6))).toBeInTheDocument();
  });

  it('has a calm empty state, different when nobody is assigned', async () => {
    const empty = (pms: string[]) => (api: Api) => {
      api.listReviews = async () => ({ drafts: [], total: 0, truncated: false, pms });
      return api;
    };
    const first = renderApp({ role: 'reviewer', hash: '/reviews', wrap: empty(['ana']) });
    expect(await screen.findByText(R.empty)).toBeInTheDocument();
    first.unmount();
    renderApp({ role: 'reviewer', hash: '/reviews', wrap: empty([]) });
    expect(await screen.findByText(R.emptyNobody)).toBeInTheDocument();
  });

  it('offers a retry when the inbox cannot load', async () => {
    renderApp({ role: 'reviewer', hash: '/reviews', wrap: failOnce('listReviews', new ApiError(503, 'unavailable', 'Down.'), 2) });
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(copy.errors.codes.unavailable);
    await userEvent.click(within(alert).getByRole('button', { name: copy.errors.retry }));
    expect(await inbox()).toBeInTheDocument();
  });
});

describe('reviews: reading a draft', () => {
  it('shows it read-only with its sources and numbered markers, and no way to approve, edit or send', async () => {
    renderApp({ role: 'reviewer', hash: '/reviews/apr_dana_onboarding' });
    const doc = await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(within(doc).getByRole('heading', { level: 1 })).toHaveTextContent('Onboarding checklist for new workspaces');
    expect(within(doc).getByText('Space PM, under “Onboarding”')).toBeInTheDocument();
    expect(doc.querySelectorAll('button.cite')).toHaveLength(2);
    expect(screen.getByText('SUP-88: Admins stuck after signup')).toBeInTheDocument();
    expect(screen.getByText(R.readOnly)).toBeInTheDocument();
    for (const name of [/^Approve/, /^Edit$/, /^Reject/, /Suggest a change/, /^Send/]) {
      expect(screen.queryByRole('button', { name })).not.toBeInTheDocument();
    }
    expect(screen.queryByRole('textbox', { name: copy.draft.bodyLabel })).not.toBeInTheDocument();
  });

  it('a marker jumps to its source card, as in the draft workspace', async () => {
    renderApp({ role: 'reviewer', hash: '/reviews/apr_dana_onboarding' });
    const doc = await screen.findByRole('article', { name: copy.draft.formattedLabel });
    await userEvent.click(doc.querySelector<HTMLButtonElement>('button.cite')!);
    await waitFor(() => expect(document.getElementById('src-s1')!.querySelector('button.source')).toHaveFocus());
    expect(document.getElementById('src-s1')!.querySelector('button.source')).toHaveAttribute('aria-pressed', 'true');
  });

  it('says when its owner edited it', async () => {
    renderApp({
      role: 'reviewer',
      hash: '/reviews/apr_dana_onboarding',
      wrap: (api) => {
        const get = api.getReview.bind(api);
        api.getReview = async (id) => ({ ...(await get(id)), edited: true });
        return api;
      },
    });
    expect(await screen.findByText(R.editedByOwner)).toBeInTheDocument();
  });

  it('a restricted draft: metadata only, the reason in one line, no recommend panel', async () => {
    renderApp({ role: 'reviewer', hash: '/reviews/apr_prd' });
    expect(await screen.findByText(R.restrictedLine)).toBeInTheDocument();
    expect(screen.getByText('Ana Silva')).toBeInTheDocument();
    expect(screen.queryByText('PRD: TCB grace period setting')).not.toBeInTheDocument();
    expect(screen.queryByRole('region', { name: P.heading })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: P.approve })).not.toBeInTheDocument();
  });

  it.each([
    ['apr_ben_own', 'own_draft'],
    ['apr_nobody', 'not_found'],
  ] as const)('%s: one calm sentence (%s)', async (id, code) => {
    renderApp({ role: 'reviewer', hash: `/reviews/${id}` });
    expect(await screen.findByText(R.gone[code])).toBeInTheDocument();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    if (code === 'own_draft') expect(screen.getByRole('link', { name: R.openOwn })).toHaveAttribute('href', `#/d/${id}`);
  });

  it.each(['not_pending', 'expired'] as const)('a draft that is %s: one calm sentence', async (code) => {
    renderApp({ role: 'reviewer', hash: '/reviews/apr_jira', wrap: failOnce('getReview', new ApiError(409, code, 'No.')) });
    expect(await screen.findByText(R.gone[code])).toBeInTheDocument();
    expect(screen.getByRole('link', { name: R.back })).toHaveAttribute('href', '#/reviews');
  });
});

describe('reviews: recommending', () => {
  it('recommends approving, then shows what was recorded and when', async () => {
    const spy = vi.fn();
    renderApp({
      role: 'reviewer',
      hash: '/reviews/apr_dana_onboarding',
      wrap: (api) => {
        const rec = api.recommend.bind(api);
        api.recommend = async (id, input) => {
          spy(id, input);
          return rec(id, input);
        };
        return api;
      },
    });
    const box = await panel();
    expect(within(box).getByText(P.intro('dana'))).toBeInTheDocument();
    await userEvent.click(within(box).getByRole('button', { name: P.approve }));
    expect(await within(box).findByText(P.mine.approve)).toBeInTheDocument();
    expect(within(box).getByText(P.when(copy.time.justNow))).toBeInTheDocument();
    expect(spy).toHaveBeenCalledWith('apr_dana_onboarding', { verdict: 'approve', note: undefined, revision: 1, args_hash: expect.stringMatching(/^[0-9a-f]{64}$/) });
    expect(await screen.findByText(P.recorded.approve)).toBeInTheDocument();
  });

  it('suggests changes with a note, counted as typed, and clears the box once recorded', async () => {
    renderApp({ role: 'reviewer', hash: '/reviews/apr_dana_onboarding' });
    const box = await panel();
    const note = within(box).getByLabelText(P.noteLabel('dana'));
    await userEvent.type(note, 'Link the IT guide.');
    expect(within(box).getByText(P.counterLabel(18, 1000))).toBeInTheDocument();
    await userEvent.click(within(box).getByRole('button', { name: P.changes }));
    expect(await within(box).findByText(P.mine.changes)).toBeInTheDocument();
    expect(within(box).getByText('Link the IT guide.')).toBeInTheDocument();
    expect(note).toHaveValue('');
  });

  it('a note over 1000 characters blocks sending and says so', async () => {
    renderApp({ role: 'reviewer', hash: '/reviews/apr_dana_onboarding' });
    const box = await panel();
    const note = within(box).getByLabelText(P.noteLabel('dana'));
    await userEvent.click(note);
    await userEvent.paste('x'.repeat(1001));
    expect(note).toHaveAttribute('aria-invalid', 'true');
    expect(within(box).getByText(P.tooLong(1000))).toBeInTheDocument();
    expect(within(box).getByRole('button', { name: P.approve })).toBeDisabled();
    expect(within(box).getByRole('button', { name: P.changes })).toBeDisabled();
  });

  it('409 draft_changed: reloads the new version, keeps the note, and says so plainly', async () => {
    renderApp({ role: 'reviewer', hash: '/reviews/apr_dana_exports' });
    const box = await panel();
    expect(screen.getByText(/at most three times/)).toBeInTheDocument();
    await userEvent.type(within(box).getByLabelText(P.noteLabel('dana')), 'Say who gets the email.');
    await userEvent.click(within(box).getByRole('button', { name: P.changes }));
    expect(await within(box).findByRole('alert')).toHaveTextContent(P.changed('dana'));
    expect(screen.getByText(/at most twice, then stop/)).toBeInTheDocument();
    expect(screen.getByText(R.editedByOwner)).toBeInTheDocument();
    expect(within(box).getByLabelText(P.noteLabel('dana'))).toHaveValue('Say who gets the email.');
    expect(within(box).queryByText(P.mine.changes)).not.toBeInTheDocument();
    // On the new version it goes through.
    await userEvent.click(within(box).getByRole('button', { name: P.changes }));
    expect(await within(box).findByText(P.mine.changes)).toBeInTheDocument();
    expect(within(box).getByText('Say who gets the email.')).toBeInTheDocument();
  });

  it.each([
    ['unknown_outcome', new ApiError(502, 'unknown_outcome', 'Lost.')],
    ['a dropped connection', new ApiError(0, 'network', 'No answer.')],
    ['a 504', new ApiError(504, 'unknown_outcome', 'Late.')],
  ])('%s: never claims it was or was not recorded, blocks a blind resend, and offers to check', async (_name, error) => {
    renderApp({ role: 'reviewer', hash: '/reviews/apr_dana_onboarding', wrap: failOnce('recommend', error) });
    const box = await panel();
    await userEvent.type(within(box).getByLabelText(P.noteLabel('dana')), 'Fine by me.');
    await userEvent.click(within(box).getByRole('button', { name: P.approve }));
    const alert = await within(box).findByRole('alert');
    expect(alert).toHaveTextContent(P.unsure);
    expect(alert).not.toHaveTextContent(/not saved|was saved|recorded\.|nothing changed/i);
    expect(screen.queryByText(P.recorded.approve)).not.toBeInTheDocument();
    expect(within(box).getByRole('button', { name: P.approve })).toBeDisabled();
    expect(within(box).getByLabelText(P.noteLabel('dana'))).toHaveValue('Fine by me.');
    await userEvent.click(within(alert).getByRole('button', { name: P.checkAgain }));
    expect(await within(box).findByText(P.checkedNone)).toBeInTheDocument();
    expect(within(box).getByRole('button', { name: P.approve })).toBeEnabled();
  });

  it('after a lost answer that was in fact recorded, Check again shows it', async () => {
    renderApp({ role: 'reviewer', hash: '/reviews/apr_dana_onboarding', reviewError: 'unknown_outcome' });
    const box = await panel();
    await userEvent.click(within(box).getByRole('button', { name: P.approve }));
    await userEvent.click(await within(box).findByRole('button', { name: P.checkAgain }));
    expect(await within(box).findByText(P.checkedSome)).toBeInTheDocument();
    expect(within(box).getByText(P.mine.approve)).toBeInTheDocument();
  });

  it('a draft decided meanwhile turns into its calm sentence', async () => {
    renderApp({ role: 'reviewer', hash: '/reviews/apr_dana_onboarding', reviewError: 'not_pending', wrap: (api) => {
      let reads = 0;
      const get = api.getReview.bind(api);
      api.getReview = async (id) => {
        reads += 1;
        if (reads > 1) throw new ApiError(409, 'not_pending', 'Decided.');
        return get(id);
      };
      return api;
    } });
    const box = await panel();
    await userEvent.click(within(box).getByRole('button', { name: P.approve }));
    expect(await screen.findByText(R.gone.not_pending)).toBeInTheDocument();
  });

  it('a plain refusal says what happened in human words, never the code', async () => {
    renderApp({ role: 'reviewer', hash: '/reviews/apr_dana_onboarding', reviewError: 'assertion_required' });
    const box = await panel();
    await userEvent.click(within(box).getByRole('button', { name: P.approve }));
    const alert = await within(box).findByRole('alert');
    expect(alert).toHaveTextContent(copy.errors.codes.assertion_required);
    expect(alert).not.toHaveTextContent('assertion_required');
  });

  it('an older server that sends no version: no panel buttons, one line why', async () => {
    renderApp({
      role: 'reviewer',
      hash: '/reviews/apr_dana_onboarding',
      wrap: (api) => {
        const get = api.getReview.bind(api);
        api.getReview = async (id) => ({ ...(await get(id)), revision: null });
        return api;
      },
    });
    const box = await panel();
    expect(within(box).getByText(P.cannot)).toBeInTheDocument();
    expect(within(box).queryByRole('button', { name: P.approve })).not.toBeInTheDocument();
  });

  it('a hostile owner name is plain text everywhere', async () => {
    renderApp({ role: 'reviewer', hash: '/reviews/apr_zed', hostile: true });
    const box = await panel();
    expect(within(box).getByText(P.intro(HOSTILE_NAME))).toBeInTheDocument();
    expect(document.querySelector('main img')).toBeNull();
    expect((window as unknown as { __pwned?: number }).__pwned).toBeUndefined();
  });
});

describe('reviews: what the owner sees', () => {
  const block = () => screen.findByRole('region', { name: copy.recommendations.heading });

  it('lists each recommendation in the workspace: who, verdict, when, the note', async () => {
    renderApp({ hash: '/d/apr_jira' });
    const recs = await block();
    expect(within(recs).getByText(copy.recommendations.line('Ben Carter', 'changes', false))).toBeInTheDocument();
    expect(within(recs).getByText(/Say which time zone the end date uses/)).toBeInTheDocument();
    expect(within(recs).queryByText(copy.recommendations.earlier)).not.toBeInTheDocument();
  });

  it('marks it as given on an earlier version once the owner saves an edit', async () => {
    renderApp({ hash: '/d/apr_jira' });
    await block();
    await userEvent.click(screen.getByRole('button', { name: copy.draft.edit }));
    await userEvent.type(screen.getByLabelText(copy.draft.bodyLabel), ' In the admin’s time zone.');
    await userEvent.click(screen.getByRole('button', { name: copy.draft.save }));
    const recs = await block();
    expect(await within(recs).findByText(copy.recommendations.line('Ben Carter', 'changes', true))).toBeInTheDocument();
    expect(within(recs).getByText(copy.recommendations.earlier)).toBeInTheDocument();
  });

  it('says a removed note was removed', async () => {
    renderApp({ hash: '/d/apr_slack_old' });
    const recs = await block();
    expect(within(recs).getByText(copy.recommendations.noteRemoved)).toBeInTheDocument();
  });

  it('shows no block when there is no advice', async () => {
    renderApp({ hash: '/d/apr_prd' });
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(screen.queryByRole('region', { name: copy.recommendations.heading })).not.toBeInTheDocument();
  });

  it('the approve dialog carries the advice as one fact line and never blocks the confirm', async () => {
    renderApp({ hash: '/d/apr_jira' });
    await block();
    await userEvent.click(screen.getByRole('button', { name: 'Approve to Jira' }));
    const dialog = screen.getByRole('dialog');
    expect(within(dialog).getByText(copy.recommendations.factHeading)).toBeInTheDocument();
    expect(within(dialog).getByText(copy.recommendations.line('Ben Carter', 'changes', false))).toBeInTheDocument();
    const confirm = within(dialog).getByRole('button', { name: copy.approve.confirm('jira_issue') });
    expect(confirm).toBeEnabled();
    await userEvent.click(confirm);
    expect(await screen.findByText(copy.approve.approvedWhere('jira_issue', 'Project PAY, a new Story under PAY-212: TCB grace period'))).toBeInTheDocument();
  });

  it('a hostile reviewer name and note render as plain text', async () => {
    renderApp({ hash: '/d/apr_hostile', hostile: true });
    const recs = await block();
    expect(within(recs).getByText(copy.recommendations.line(HOSTILE_NAME, 'changes', false))).toBeInTheDocument();
    expect(within(recs).getByText(HOSTILE_NOTE)).toBeInTheDocument();
    expect(recs.querySelector('img, a, strong')).toBeNull();
    expect((window as unknown as { __pwned?: number }).__pwned).toBeUndefined();
  });
});

describe('team: the invite takes a Slack member id', () => {
  const open = async () => {
    renderApp({ role: 'admin', hash: '/admin/team' });
    await userEvent.click(await screen.findByRole('button', { name: copy.admin.team.invite.button }));
    const d = screen.getByRole('dialog');
    await userEvent.type(within(d).getByLabelText(copy.admin.team.invite.handle), 'cy');
    await userEvent.type(within(d).getByLabelText(copy.admin.team.invite.signInName), 'cy@contoso.example');
    await userEvent.type(within(d).getByLabelText(copy.admin.team.invite.atlassianEmail), 'cy@contoso.example');
    return d;
  };

  it('says where to find it, refuses a malformed one before sending, and sends a good one', async () => {
    const d = await open();
    const field = within(d).getByLabelText(copy.admin.team.invite.slackId);
    expect(field).toHaveAccessibleDescription(copy.admin.team.invite.slackHint);
    await userEvent.type(field, 'u12');
    expect(field).toHaveAttribute('aria-invalid', 'true');
    expect(field).toHaveAccessibleDescription(copy.admin.team.invite.slackInvalid);
    const submit = within(d).getByRole('button', { name: copy.admin.team.invite.submit });
    expect(submit).toBeDisabled();
    expect(within(d).getByText(copy.admin.team.invite.fixSlack)).toBeInTheDocument();
    await userEvent.clear(field);
    await userEvent.type(field, 'U0CY0001');
    expect(field).not.toHaveAttribute('aria-invalid');
    await userEvent.click(submit);
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });
});
