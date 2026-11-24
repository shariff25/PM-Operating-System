import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';
import { ApiError } from '../api';
import type { Api } from '../api';
import { createMockClient } from '../api/mock';
import { createRealClient } from '../api/real';
import { copy } from '../copy';
import { renderApp } from '../test/util';

const seenHints = (a: Api) => {
  const me = a.me.bind(a);
  a.me = async () => ({ ...(await me()), hints_seen: ['sources', 'ask', 'approve'] });
  return a;
};
const dialog = () => screen.findByRole('dialog', { name: copy.approve.confirmHeading });
const openDialog = async (user: ReturnType<typeof userEvent.setup>, kind: Parameters<typeof copy.approve.button>[0]) => {
  await user.click(await screen.findByRole('button', { name: copy.approve.button(kind) }));
  return dialog();
};

describe('the approve dialog tells what the server knows about the draft', () => {
  it('Confluence: restricted sources in attention style, and the line the page will end with', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd', wrap: seenHints });
    const d = await openDialog(user, 'confluence_page');
    expect(within(d).getByText(copy.approve.restricted).closest('.fact-warn')).not.toBeNull();
    expect(d.querySelector('.note-restricted')).toBeNull();
    expect(within(d).getByText(/^Ends with “Drafted with PM OS · run /)).toBeInTheDocument();
    // not a Slack draft: no audience row
    expect(within(d).queryByText(copy.approve.audienceHeading)).not.toBeInTheDocument();
  });

  it('Jira: no restricted note when the draft was not built from restricted sources', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_jira', wrap: seenHints });
    const d = await openDialog(user, 'jira_issue');
    expect(within(d).queryByText(copy.approve.restricted)).not.toBeInTheDocument();
    expect(within(d).getByText(/^Ends with “Drafted with PM OS/)).toBeInTheDocument();
  });

  it('shows nothing about a marker when the server sends none', async () => {
    const user = userEvent.setup();
    renderApp({
      hash: '/d/apr_jira',
      wrap: (a) => {
        seenHints(a);
        const get = a.getDraft.bind(a);
        a.getDraft = async (id) => ({ ...(await get(id)), marker: null });
        return a;
      },
    });
    const d = await openDialog(user, 'jira_issue');
    expect(within(d).queryByText(copy.approve.markerHeading)).not.toBeInTheDocument();
    expect(within(d).queryByText(/Drafted with PM OS/)).not.toBeInTheDocument();
  });

  it('Slack: posts as you with the marker, and says the audience is checked on confirm while it is unknown', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_slack_ext', audienceDrafts: true, wrap: seenHints });
    const d = await openDialog(user, 'slack_message');
    expect(within(d).getByText('Posts as you, marked “Drafted with PM OS”')).toBeInTheDocument();
    expect(within(d).getByText(copy.approve.audienceUnchecked)).toBeInTheDocument();
  });

  it('Slack: after needs_external_confirm the audience fills in, and the tick is asked for', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_slack_ext', audienceDrafts: true, wrap: seenHints });
    const d = await openDialog(user, 'slack_message');
    await user.click(within(d).getByRole('button', { name: copy.approve.confirm('slack_message') }));
    expect(await within(d).findByText(copy.approve.audienceVisible(14))).toBeInTheDocument();
    expect(within(d).getByText(copy.approve.audienceExternal)).toBeInTheDocument();
    await user.click(within(d).getByRole('checkbox'));
    await user.click(within(d).getByRole('button', { name: copy.approve.confirmExternalButton }));
    expect(await screen.findByRole('status', { name: 'Outcome' })).toHaveTextContent('Posted in a Slack channel');
  });

  it('Slack: audience_blocked fills in the audience and leaves only rejecting', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_slack_blocked', audienceDrafts: true, wrap: seenHints });
    const d = await openDialog(user, 'slack_reply');
    expect(within(d).getByText(copy.approve.restricted)).toBeInTheDocument();
    await user.click(within(d).getByRole('button', { name: copy.approve.confirm('slack_reply') }));
    expect(await within(d).findByText(copy.approve.audienceVisible(14))).toBeInTheDocument();
    expect(within(d).getByText(copy.approve.errors.audience_blocked.text)).toBeInTheDocument();
    // blocked: no way to confirm, no external box to tick, only the two real options
    expect(within(d).queryByRole('button', { name: /^Confirm and post/ })).not.toBeInTheDocument();
    expect(within(d).queryByRole('checkbox')).not.toBeInTheDocument();
    expect(within(d).queryByText(copy.approve.externalCheckbox)).not.toBeInTheDocument();
    expect(within(d).getByRole('button', { name: copy.approve.blockedReject })).toBeInTheDocument();
    // the buttons live in a footer outside the scrolling body
    const footer = d.querySelector('.modal-actions')!;
    expect(d.querySelector('.modal-scroll')!.contains(footer)).toBe(false);
    expect(within(footer as HTMLElement).getByRole('button', { name: copy.approve.blockedAsk })).toBeInTheDocument();
  });

  it('Slack: blocked, "reject" leaves the dialog and opens the reject choices', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_slack_blocked', audienceDrafts: true, wrap: seenHints });
    const d = await openDialog(user, 'slack_reply');
    await user.click(within(d).getByRole('button', { name: copy.approve.confirm('slack_reply') }));
    await user.click(await within(d).findByRole('button', { name: copy.approve.blockedReject }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(await screen.findByText(copy.reject.heading)).toBeInTheDocument();
  });

  it('Slack: blocked, "ask for another version" leaves the dialog and fills the change box', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_slack_blocked', audienceDrafts: true, wrap: seenHints });
    const d = await openDialog(user, 'slack_reply');
    await user.click(within(d).getByRole('button', { name: copy.approve.confirm('slack_reply') }));
    await user.click(await within(d).findByRole('button', { name: copy.approve.blockedAsk }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(await screen.findByDisplayValue(copy.approve.blockedAskPrefill)).toBeInTheDocument();
  });

  it('names the audience of a Slack draft the server already knows to be external, with a link to it', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_slack', wrap: seenHints });
    const d = await openDialog(user, 'slack_reply');
    expect(within(d).getByText(copy.approve.audienceExternal)).toBeInTheDocument();
    expect(within(d).getByRole('link', { name: new RegExp(copy.draft.openTarget('slack')) })).toHaveAttribute(
      'href',
      expect.stringContaining('example.com/slack'),
    );
  });
});

describe('saved edits are on the draft itself', () => {
  it('says so where the PM edits, and shows when it was edited after saving', async () => {
    const user = userEvent.setup();
    renderApp({ hash: '/d/apr_prd', wrap: seenHints });
    await user.click(await screen.findByRole('button', { name: copy.draft.edit }));
    expect(screen.getByText(new RegExp(copy.draft.editSavedNote))).toBeInTheDocument();
    await user.type(screen.getByLabelText(copy.draft.bodyLabel), ' More.');
    await user.click(screen.getByRole('button', { name: copy.draft.save }));
    expect(await screen.findAllByText(copy.draft.editedWhen('just now'))).not.toHaveLength(0);
  });

  it('no copy says an edit lives only in the web app', () => {
    const all = JSON.stringify(copy).toLowerCase();
    expect(all).not.toMatch(/only (in|on) (the|this) (web )?app/);
    expect(all).not.toMatch(/edit[^"]{0,40}(stays|lives) (only )?(here|in this)/);
  });

  it('after an approve answers not_pending, shows the draft as decided, not an error', async () => {
    const user = userEvent.setup();
    renderApp({
      hash: '/d/apr_prd',
      wrap: (a) => {
        seenHints(a);
        const reject = a.reject.bind(a);
        a.approve = async () => {
          await reject('apr_prd'); // someone decided it in Slack meanwhile
          throw new ApiError(409, 'not_pending', 'x');
        };
        return a;
      },
    });
    const d = await openDialog(user, 'confluence_page');
    await user.click(within(d).getByRole('button', { name: copy.approve.confirm('confluence_page') }));
    expect(await screen.findByText(copy.approve.rejectedNote)).toBeInTheDocument();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('an approve that answers 200 with the draft already approved shows it as approved', async () => {
    const user = userEvent.setup();
    renderApp({
      hash: '/d/apr_prd',
      wrap: (a) => {
        seenHints(a);
        const get = a.getDraft.bind(a);
        a.approve = async (id) => ({ ...(await get(id)), status: 'approved', result_url: 'https://example.com/wiki/p' });
        return a;
      },
    });
    const d = await openDialog(user, 'confluence_page');
    await user.click(within(d).getByRole('button', { name: copy.approve.confirm('confluence_page') }));
    await waitFor(() => expect(screen.getByRole('status', { name: 'Outcome' })).toHaveTextContent('Created in'));
    expect(screen.getByRole('link', { name: copy.approve.resultLink })).toBeInTheDocument();
  });
});

describe('where a draft goes', () => {
  it('Slack labels carry no channel id, with an Open in Slack link beside the target', async () => {
    renderApp({ hash: '/d/apr_slack', wrap: seenHints });
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(screen.getAllByText('A Slack thread').length).toBeGreaterThan(0);
    expect(document.body.textContent).not.toMatch(/\bC0[0-9A-Z]{3,}\b/);
    expect(screen.getAllByRole('link', { name: new RegExp(copy.draft.openTarget('slack')) }).length).toBeGreaterThan(0);
  });

  it('Slack sources read "Slack thread"', async () => {
    renderApp({ hash: '/d/apr_prd', wrap: seenHints });
    await screen.findByRole('article', { name: copy.draft.formattedLabel });
    expect(screen.getAllByText('Slack thread').length).toBeGreaterThan(0);
  });

  it('Open in Jira and Open in Confluence read as they should', () => {
    expect(copy.draft.openTarget('jira')).toBe('Open in Jira');
    expect(copy.draft.openTarget('confluence')).toBe('Open in Confluence');
  });
});

describe('writes carry an Origin, as a browser sends one', () => {
  it('the mock refuses a write without one with 403 bad_origin, and a read still works', async () => {
    const api = createMockClient({ latency: 0, signedIn: true, origin: null });
    await expect(api.setNudges(false)).rejects.toMatchObject({ status: 403, code: 'bad_origin' });
    await expect(api.approve('apr_prd')).rejects.toMatchObject({ status: 403, code: 'bad_origin' });
    await expect(api.devLogin('ana')).rejects.toMatchObject({ code: 'bad_origin' });
    await expect(api.me()).resolves.toMatchObject({ handle: 'ana' });
  });

  it('the mock takes a write that has one (the page origin, as in a browser)', async () => {
    const api = createMockClient({ latency: 0, signedIn: true });
    await expect(api.setNudges(false)).resolves.toMatchObject({ nudges: false });
  });

  it('the real client works against a server that refuses writes without an Origin, when the browser adds it', async () => {
    const server =
      (origin: string | null): typeof fetch =>
      async (_url, init) => {
        const method = init?.method ?? 'GET';
        if (method !== 'GET' && !origin) {
          return new Response(JSON.stringify({ code: 'bad_origin', error: 'x' }), { status: 403, headers: { 'Content-Type': 'application/json' } });
        }
        return new Response(
          JSON.stringify({ handle: 'ana', name: 'A', roles: [], csrf_token: 't', first_visit: false, hints_seen: [], nudges: false }),
          { status: 200, headers: { 'Content-Type': 'application/json' } },
        );
      };
    await expect(createRealClient({ fetchImpl: server('http://localhost:3000') }).setNudges(false)).resolves.toMatchObject({ nudges: false });
    await expect(createRealClient({ fetchImpl: server(null) }).setNudges(false)).rejects.toMatchObject({ code: 'bad_origin' });
  });
});
