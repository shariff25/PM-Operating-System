import { screen, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import type { Api } from '../api';
import { copy, midSentence } from '../copy';
import { renderApp } from '../test/util';

describe('target labels in the middle of a sentence', () => {
  // every label docs/web-api.md documents, and what it should read as inside a sentence
  const cases: [string, string][] = [
    ['#launch-acme', '#launch-acme'],
    ['A thread in #launch-acme', 'a thread in #launch-acme'],
    ['A private channel', 'a private channel'],
    ['A thread in a private channel', 'a thread in a private channel'],
    ['Direct message with ben', 'direct message with ben'],
    ['A direct message thread with ben', 'a direct message thread with ben'],
    ['Direct message', 'direct message'],
    ['A direct message thread', 'a direct message thread'],
    ['A group direct message', 'a group direct message'],
    ['A group direct message thread', 'a group direct message thread'],
    ['A Slack channel', 'a Slack channel'],
    ['A Slack thread', 'a Slack thread'],
    ['A direct message', 'a direct message'],
    ['An unrecognised channel', 'an unrecognised channel'],
    ['Space PM, under “Specs”', 'space PM, under “Specs”'],
    ['Page “PRD: Billing retries”', 'page “PRD: Billing retries”'],
    ['Project PAY, a new Story under PAY-212: TCB grace period', 'project PAY, a new Story under PAY-212: TCB grace period'],
  ];
  it.each(cases)('%s reads as %s', (label, expected) => {
    expect(midSentence(label)).toBe(expected);
  });

  it('leaves an acronym or a handle that starts the label alone', () => {
    expect(midSentence('PRD team space')).toBe('PRD team space');
    expect(midSentence('')).toBe('');
  });

  it('fixes the stray capital in the Slack approve sentences', () => {
    expect(copy.approve.confirmText('slack_reply', 'A Slack thread', 'Ana')).toBe(
      'This will post a Slack reply in a Slack thread, under your name (Ana).',
    );
    expect(copy.approve.confirmText('slack_message', '#launch-acme', 'Ana')).toContain('in #launch-acme,');
    expect(copy.approve.approvedWhere('slack_reply', 'A thread in #launch-acme')).toBe('Posted in a thread in #launch-acme.');
    // Confluence and Jira labels name a space or project, so they keep their capital
    expect(copy.approve.approvedWhere('confluence_page', 'Space PM')).toBe('Created in Space PM.');
  });
});

describe('the drafts list, when the server left some out', () => {
  it('says how many are waiting in total and that only the newest are listed', async () => {
    renderApp({ hash: '/drafts', listCap: 2 });
    const note = await screen.findByText(/drafts are waiting in total/);
    expect(note).toHaveTextContent(/^\d+ drafts are waiting in total\. Only the newest 2 are listed here\.$/);
  });

  it('says nothing when the list is whole', async () => {
    renderApp({ hash: '/drafts' });
    await screen.findByRole('tablist', { name: copy.draftsPage.tabsLabel });
    await screen.findAllByRole('link', { name: /Check|PRD|Reply|Message|Direct|Jira|Churn/ });
    expect(screen.queryByText(/waiting in total/)).not.toBeInTheDocument();
    expect(screen.queryByText(/Only the newest/)).not.toBeInTheDocument();
  });

  it('copes with a server that says truncated but not how many', () => {
    expect(copy.draftsPage.truncated(null, 5)).toMatch(/Only the newest/);
    expect(copy.draftsPage.truncated(1500, 1000)).toBe('1500 drafts are waiting in total. Only the newest 1000 are listed here.');
  });
});

describe('the admin overview shows the audit retention', () => {
  const R = copy.admin.overview.retention;
  const card = async () => within(await screen.findByRole('region', { name: R.title }));

  it('shows how long events are kept, the oldest one, and the last clean-up', async () => {
    renderApp({ role: 'admin', hash: '/admin' });
    const c = await card();
    expect(c.getByText('Events are kept for 90 days')).toBeInTheDocument();
    expect(c.getByText(/^Oldest event: \d+ [A-Z][a-z]{2}$/)).toBeInTheDocument();
    expect(c.getByText(/^Last clean-up: \d+ [A-Z][a-z]{2}, removed 1200 events$/)).toBeInTheDocument();
  });

  it('says plainly that no clean-up has run, and copes with an older server that sends none of it', async () => {
    renderApp({
      role: 'admin',
      hash: '/admin',
      wrap: (a: Api) => {
        const f = a.adminOverview.bind(a);
        a.adminOverview = async () => {
          const o = await f();
          return { ...o, retention_days: null, oldest_event_at: null, last_purge: null };
        };
        return a;
      },
    });
    const c = await card();
    expect(c.getByText(R.keptUnknown)).toBeInTheDocument();
    expect(c.getByText(R.oldestNone)).toBeInTheDocument();
    expect(c.getByText('No clean-up has run yet')).toBeInTheDocument();
  });

  it('reads one event as "event", and a missing field the same as null', async () => {
    renderApp({
      role: 'auditor',
      hash: '/admin',
      wrap: (a: Api) => {
        const f = a.adminOverview.bind(a);
        a.adminOverview = async () => {
          const o = await f();
          const rest: Partial<typeof o> = { ...o };
          delete rest.retention_days;
          delete rest.oldest_event_at;
          return { ...o, ...rest, retention_days: undefined, oldest_event_at: undefined, last_purge: { at: '2026-11-17T03:07:12.004Z', deleted: 1 } };
        };
        return a;
      },
    });
    const c = await card();
    expect(c.getByText('Last clean-up: 17 Nov, removed 1 event')).toBeInTheDocument();
    expect(c.getByText(R.keptUnknown)).toBeInTheDocument();
  });
});

describe('the newer admin error codes have plain words', () => {
  it.each(['github_not_linked', 'policy_mismatch', 'audit_unavailable', 'stale_base', 'rate_limited', 'not_configured'] as const)(
    '%s',
    (code) => {
      expect(copy.errors.codes[code].length).toBeGreaterThan(20);
      expect(copy.errors.codes[code]).not.toContain('—');
    },
  );

  it('tells an admin who has not linked GitHub to link it in PM OS', () => {
    expect(copy.errors.codes.github_not_linked).toMatch(/^Link your GitHub account in PM OS/);
  });
});
