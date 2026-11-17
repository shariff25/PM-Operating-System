import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { ApiError } from '../api';
import type { Api, DraftKind } from '../api';
import { approveWords, copy } from '../copy';
import { isDefiniteRefusal } from '../errors';
import { renderApp } from '../test/util';

const KINDS: DraftKind[] = ['confluence_page', 'jira_issue', 'slack_reply', 'slack_message'];
const SYSTEM: Record<DraftKind, string> = {
  confluence_page: 'Confluence',
  jira_issue: 'Jira',
  slack_reply: 'Slack',
  slack_message: 'Slack',
};

describe('approve wording, per kind of draft', () => {
  it.each(KINDS)('%s: every line names the right system and the right thing', (kind) => {
    const w = approveWords(kind);
    const noun = { confluence_page: 'page', jira_issue: 'issue', slack_reply: 'reply', slack_message: 'message' }[kind];
    const all = [w.button, w.hint, w.hintShort, w.confirmText('Somewhere', 'Ana'), w.confirmButton, w.confirming, w.nothingYet, w.done('Somewhere'), w.toast].join('\n');
    expect(w.button).toContain(SYSTEM[kind]);
    expect(w.hint).toContain(noun);
    expect(w.hint).toContain(SYSTEM[kind]);
    expect(w.confirmText('Somewhere', 'Ana')).toContain(noun);
    expect(w.confirmText('Somewhere', 'Ana')).toContain('Somewhere');
    expect(w.toast).toContain(noun);
    // the other systems and their nouns never leak into this kind's lines
    for (const other of KINDS.filter((k) => SYSTEM[k] !== SYSTEM[kind])) expect(all).not.toContain(SYSTEM[other]);
    if (kind !== 'confluence_page') expect(all).not.toMatch(/\bpage\b/i);
    if (kind !== 'jira_issue') expect(all).not.toMatch(/\bissue\b/i);
    if (kind.startsWith('slack')) {
      expect(all).not.toMatch(/created|creates|creating/i);
      expect(w.confirmButton).toBe('Confirm and post');
      expect(w.done('#billing')).toBe('Posted in #billing.');
    } else {
      expect(w.confirmButton).toBe('Confirm and create');
      expect(w.done('Space PM')).toBe('Created in Space PM.');
      expect(all).not.toMatch(/\bpost/i);
    }
  });

  it('is what the screens use: the buttons, hints, dialogs and success lines of copy', () => {
    for (const kind of KINDS) {
      expect(copy.approve.button(kind)).toBe(approveWords(kind).button);
      expect(copy.hints.approve(kind)).toBe(approveWords(kind).hint);
      expect(copy.hintsShort.approve(kind)).toBe(approveWords(kind).hintShort);
      expect(copy.approve.confirmText(kind, 'W', 'Ana')).toBe(approveWords(kind).confirmText('W', 'Ana'));
      expect(copy.approve.approvedWhere(kind, 'W')).toBe(approveWords(kind).done('W'));
      expect(copy.toast.approved(kind)).toBe(approveWords(kind).toast);
    }
  });

  // The sample drafts are a Confluence page, a Jira issue and a Slack reply; the fourth kind is made by changing one.
  const asKind = (kind: DraftKind) => (a: Api) => {
    const get = a.getDraft.bind(a);
    a.getDraft = async (id) => ({ ...(await get(id)), kind });
    const approveIt = a.approve.bind(a);
    a.approve = async (id, o) => ({ ...(await approveIt(id, o)), kind });
    const me = a.me.bind(a);
    a.me = async () => ({ ...(await me()), hints_seen: ['sources', 'ask'] });
    return a;
  };
  const cases: [DraftKind, string][] = [
    ['confluence_page', 'apr_prd'],
    ['jira_issue', 'apr_jira'],
    ['slack_reply', 'apr_slack'],
    ['slack_message', 'apr_jira'],
  ];

  it.each(cases)('%s: the hint, the button, the dialog and the success line, end to end', async (kind, id) => {
    const user = userEvent.setup();
    renderApp({ hash: `/d/${id}`, wrap: asKind(kind) });
    const w = approveWords(kind);
    expect(await screen.findByText(w.hint)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: w.button }));
    const dialog = await screen.findByRole('dialog', { name: copy.approve.confirmHeading });
    expect(within(dialog).getByText(new RegExp(`^This will (create|post) a ${SYSTEM[kind]} ${kind === 'jira_issue' ? 'issue' : kind === 'confluence_page' ? 'page' : kind === 'slack_reply' ? 'reply' : 'message'} in `))).toBeInTheDocument();
    expect(within(dialog).getByText(w.nothingYet)).toBeInTheDocument();
    const external = kind === 'slack_reply';
    if (external) await user.click(within(dialog).getByRole('checkbox'));
    await user.click(within(dialog).getByRole('button', { name: external ? copy.approve.confirmExternalButton : w.confirmButton }));
    const draft = await screen.findByRole('status', { name: 'Outcome' });
        expect(draft).toHaveTextContent(kind.startsWith('slack') ? 'Posted in' : 'Created in');
    expect(await screen.findByText(w.toast)).toBeInTheDocument();
  });
});

describe('what the app says when an approval did not come back clearly', () => {
  /** An approve that fails in the way the test names, optionally after the server did the work. */
  const failing = (make: () => unknown, opts: { doneAnyway?: boolean } = {}) => (a: Api) => {
    const orig = a.approve.bind(a);
    a.approve = async (id, o) => {
      if (opts.doneAnyway) await orig(id, o);
      throw make();
    };
    return a;
  };
  const approve = async (user: ReturnType<typeof userEvent.setup>) => {
    await user.click(await screen.findByRole('button', { name: copy.approve.button('confluence_page') }));
    const dialog = await screen.findByRole('dialog', { name: copy.approve.confirmHeading });
    await user.click(within(dialog).getByRole('button', { name: copy.approve.confirm('confluence_page') }));
  };
  const unknownText = copy.approve.errors.unknown_outcome.text;

  const unsure: [string, () => unknown][] = [
    ['a network failure', () => new ApiError(0, 'network', copy.errors.network)],
    ['a timeout (408)', () => new ApiError(408, 'unknown', copy.errors.unknown)],
    ['a server error (500)', () => new ApiError(500, 'unknown', copy.errors.unknown)],
    ['the server saying unknown_outcome', () => new ApiError(502, 'unknown_outcome', 'The write may or may not have happened.')],
    ['unknown_outcome on a 409', () => new ApiError(409, 'unknown_outcome', 'Unclear.')],
    ['something that is not an api error at all', () => new TypeError('boom')],
  ];

  it.each(unsure)('%s: says it is not known, offers Check again, and no blind retry', async (_name, make) => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd', wrap: failing(make) });
    await approve(user);
    const note = await screen.findByRole('alert');
    expect(note).toHaveTextContent(unknownText);
    expect(note).not.toHaveTextContent(/nothing was (changed|created|sent)/i);
    expect(within(note).getByRole('button', { name: copy.approve.checkAgain })).toBeInTheDocument();
    // the dialog is gone, and approving again is blocked until the PM has checked
    expect(screen.queryByRole('dialog', { name: copy.approve.confirmHeading })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: copy.approve.button('confluence_page') })).toBeDisabled();
    expect(screen.getByText(copy.draft.checkFirst)).toBeInTheDocument();
  });

  it('Check again finds the draft still waiting: it says so, and approving is possible again', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd', wrap: failing(() => new ApiError(0, 'network', copy.errors.network)) });
    await approve(user);
    await user.click(await screen.findByRole('button', { name: copy.approve.checkAgain }));
    expect(await screen.findByText(copy.approve.checkedPending)).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole('button', { name: copy.approve.button('confluence_page') })).toBeEnabled());
    expect(screen.queryByText(unknownText)).not.toBeInTheDocument();
  });

  it('Check again finds it was approved after all: it shows the outcome, with no second send', async () => {
    const user = userEvent.setup();
    let sends = 0;
    renderApp({
      hash: '/d/apr_prd',
      wrap: (a) => {
        const orig = a.approve.bind(a);
        a.approve = async (id, o) => {
          sends++;
          await orig(id, o);
          throw new ApiError(0, 'network', copy.errors.network);
        };
        return a;
      },
    });
    await approve(user);
    await user.click(await screen.findByRole('button', { name: copy.approve.checkAgain }));
    const outcome = await screen.findByRole('status', { name: 'Outcome' });
    expect(outcome).toHaveTextContent('Created in Space PM, under “Specs”.');
    expect(sends).toBe(1);
  });

  it('Check again that itself fails keeps saying it is not known', async () => {
    const user = userEvent.setup();
    let fail = false;
    renderApp({
      hash: '/d/apr_prd',
      wrap: (a) => {
        failing(() => new ApiError(500, 'unknown', copy.errors.unknown))(a);
        const get = a.getDraft.bind(a);
        a.getDraft = async (id) => {
          if (fail) throw new ApiError(0, 'network', copy.errors.network);
          return get(id);
        };
        return a;
      },
    });
    await approve(user);
    fail = true;
    await user.click(await screen.findByRole('button', { name: copy.approve.checkAgain }));
    expect(await screen.findByText(copy.approve.checkFailed)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: copy.approve.button('confluence_page') })).toBeDisabled();
  });

  it('a definite refusal (a 4xx the server meant) is the only case that says nothing was sent', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd', wrap: failing(() => new ApiError(403, 'forbidden', 'You cannot approve this one.')) });
    await approve(user);
    const note = await screen.findByRole('alert');
    expect(note).toHaveTextContent(copy.errors.codes.forbidden);
    expect(screen.queryByRole('button', { name: copy.approve.checkAgain })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: copy.approve.button('confluence_page') })).toBeEnabled();
  });

  it.each([
    [502, 'upstream_unavailable'],
    [503, 'audience_unavailable'],
    [503, 'unavailable'],
  ] as const)('%s %s on a decision is a refusal the table says changed nothing: it says so and offers a retry', async (status, code) => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd', wrap: failing(() => new ApiError(status, code, 'x')) });
    await approve(user);
    const note = await screen.findByRole('alert');
    expect(note).toHaveTextContent(copy.errors.codes[code]);
    expect(screen.queryByRole('button', { name: copy.approve.checkAgain })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: copy.approve.button('confluence_page') })).toBeEnabled();
  });

  it('a 4xx without a sentence of its own says the server refused, so nothing was sent', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd', wrap: failing(() => new ApiError(400, 'bad_request', '')) });
    await approve(user);
    expect(await screen.findByRole('alert')).toHaveTextContent(copy.errors.codes.bad_request);
  });
});

describe('the other actions never claim "nothing happened" when they cannot know', () => {
  const unsureOn = (method: 'reject' | 'acceptSuggestion' | 'requestChange' | 'createTask' | 'draftFollowup', err: unknown) => (a: Api) => {
    (a as unknown as Record<string, unknown>)[method] = async () => {
      throw err;
    };
    return a;
  };
  const network = new ApiError(0, 'network', copy.errors.network);

  it('reject: a dropped connection says it is not known, and Check again looks at the draft', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd', wrap: unsureOn('reject', network) });
    await user.click(await screen.findByRole('button', { name: copy.reject.button }));
    await user.click(screen.getByRole('button', { name: copy.reject.confirm }));
    const note = await screen.findByText(new RegExp(copy.errors.unsure(copy.errors.unsureWhat.rejected).replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
    expect(note).not.toHaveTextContent(/nothing was/i);
    await user.click(within(note.closest('[role="alert"]') as HTMLElement).getByRole('button', { name: copy.errors.checkAgain }));
    await waitFor(() => expect(screen.queryByText(/I can't tell whether the rejection/)).not.toBeInTheDocument());
  });

  it('reject: a 4xx shows the app\'s sentence for its code, with no Check again', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd', wrap: unsureOn('reject', new ApiError(409, 'not_pending', 'This draft has already been decided.')) });
    await user.click(await screen.findByRole('button', { name: copy.reject.button }));
    await user.click(screen.getByRole('button', { name: copy.reject.confirm }));
    expect(await screen.findByText(copy.errors.codes.not_pending)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: copy.errors.checkAgain })).not.toBeInTheDocument();
  });

  it('ask for a change: a dropped connection says it is not known, and Check again looks for the suggestion', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd', wrap: unsureOn('requestChange', new ApiError(504, 'unknown', copy.errors.unknown)) });
    await user.type(await screen.findByLabelText(copy.draft.askLabel), 'shorter');
    await user.click(screen.getByRole('button', { name: copy.draft.askButton }));
    const note = await screen.findByRole('alert');
    expect(note).toHaveTextContent(copy.errors.unsure(copy.errors.unsureWhat.asked));
    expect(note).not.toHaveTextContent(/nothing was/i);
    await user.click(within(note).getByRole('button', { name: copy.errors.checkAgain }));
    expect(await screen.findByText(copy.draft.checkedNoChange)).toBeInTheDocument();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('accept suggestion: a dropped connection says it is not known, and Check again reloads the draft', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd', wrap: unsureOn('acceptSuggestion', network) });
    await user.type(await screen.findByLabelText(copy.draft.askLabel), 'shorter');
    await user.click(screen.getByRole('button', { name: copy.draft.askButton }));
    const region = await screen.findByRole('region', { name: copy.draft.suggestionHeading });
    await user.click(within(region).getByRole('button', { name: copy.draft.accept }));
    const note = await screen.findByRole('alert');
    expect(note).toHaveTextContent(copy.errors.unsure(copy.errors.unsureWhat.accepted));
    expect(within(note).getByRole('button', { name: copy.errors.checkAgain })).toBeInTheDocument();
  });

  it('a request from the home page that may or may not have started points to Activity, not to "nothing was started"', async () => {
    const user = userEvent.setup();
    renderApp({ wrap: unsureOn('createTask', network) });
    await user.type(await screen.findByLabelText(copy.home.askLabel), 'Draft a PRD');
    await user.click(screen.getByRole('button', { name: new RegExp(`^${copy.home.askButton}`) }));
    const note = await screen.findByRole('alert');
    expect(note).toHaveTextContent(copy.errors.unsureStarted);
    expect(note).not.toHaveTextContent(/nothing was/i);
  });

  it('a request refused with a 4xx shows the app\'s sentence for its code', async () => {
    const user = userEvent.setup();
    renderApp({ wrap: unsureOn('createTask', new ApiError(403, 'forbidden', 'Your role does not allow this.')) });
    await user.type(await screen.findByLabelText(copy.home.askLabel), 'Draft a PRD');
    await user.click(screen.getByRole('button', { name: new RegExp(`^${copy.home.askButton}`) }));
    expect(await screen.findByRole('alert')).toHaveTextContent(copy.errors.codes.forbidden);
  });

  it('a follow-up draft that may or may not have started points to Activity', async () => {
    const user = userEvent.setup();
    renderApp({ wrap: unsureOn('draftFollowup', new ApiError(500, 'unknown', copy.errors.unknown)) });
    await user.click((await screen.findAllByRole('button', { name: /Draft a reply/ }))[0]!);
    expect(await screen.findByText(copy.errors.unsureStarted)).toBeInTheDocument();
  });
});

describe('error wording', () => {
  it('no general error line claims that nothing was changed, sent, created or started', () => {
    const lines: string[] = [];
    const walk = (v: unknown) => {
      if (typeof v === 'string') lines.push(v);
      else if (v && typeof v === 'object') Object.values(v).forEach(walk);
    };
    const { codes: _codes, ...general } = copy.errors; // the per-code sentences follow the table instead (errorCodes.test.ts)
    void _codes;
    walk(general);
    lines.push(copy.errors.unsure('x'));
    for (const l of lines) expect(l, l).not.toMatch(/nothing was (changed|sent|created|started)/i);
  });

  it('knows what a definite refusal is', () => {
    expect(isDefiniteRefusal(new ApiError(400, 'bad_request', ''))).toBe(true);
    expect(isDefiniteRefusal(new ApiError(409, 'stale', ''))).toBe(true);
    expect(isDefiniteRefusal(new ApiError(403, 'forbidden', ''))).toBe(true);
    expect(isDefiniteRefusal(new ApiError(408, 'unknown', ''))).toBe(false);
    expect(isDefiniteRefusal(new ApiError(409, 'unknown_outcome', ''))).toBe(false);
    expect(isDefiniteRefusal(new ApiError(500, 'unknown', ''))).toBe(false);
    expect(isDefiniteRefusal(new ApiError(502, 'upstream_unavailable', ''))).toBe(false);
    expect(isDefiniteRefusal(new ApiError(502, 'upstream_unavailable', ''), 'decision')).toBe(true);
    expect(isDefiniteRefusal(new ApiError(503, 'unavailable', ''))).toBe(true);
    expect(isDefiniteRefusal(new ApiError(502, 'unknown_outcome', ''), 'decision')).toBe(false);
    expect(isDefiniteRefusal(new ApiError(0, 'network', ''))).toBe(false);
    expect(isDefiniteRefusal(new Error('x'))).toBe(false);
    expect(isDefiniteRefusal(undefined)).toBe(false);
  });
});
