import { screen, within } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import type { Api, Brief } from '../api';
import { briefTiming } from '../brief';
import { copy } from '../copy';
import { renderApp } from '../test/util';

const day = 24 * 3600_000;
const iso = (t: number) => new Date(t).toISOString();
const empty: Brief = { date: '2026-11-17', changed: [], waiting_on_you: [], you_wait_on: [] };
const item = { system: 'jira', title: 'PAY-1 moved', summary: 's', url: 'https://example.com/x' };

/** Home with the brief the server would send, whatever the sample data holds. */
const briefOf = (brief: Brief) => (a: Api) => {
  a.brief = async () => brief;
  return a;
};
const stats = () => screen.getByRole('region', { name: copy.today.statsLabel });

describe('what the home page says about the daily brief', () => {
  it('says it is in only when it was delivered today', async () => {
    renderApp();
    expect(await screen.findByText(copy.today.briefReady)).toBeInTheDocument();
    expect(screen.queryByText(copy.today.briefNever)).not.toBeInTheDocument();
  });

  it('says the first brief has not arrived, and when to expect it, when none ever ran', async () => {
    renderApp({ briefDelivery: 'never' });
    expect(await screen.findByText(copy.today.briefNever)).toBeInTheDocument();
    // it offers the Slack command that sets one up (docs/daily-brief.md)
    expect(copy.today.briefNever).toContain('brief me at 8');
    expect(screen.queryByText(copy.today.briefReady)).not.toBeInTheDocument();
    // no calm joke, and the counts are not zeros but missing
    expect(screen.queryByText(copy.nothingWaiting)).not.toBeInTheDocument();
    expect(screen.queryByText(copy.today.statSub.quiet)).not.toBeInTheDocument();
    expect(screen.getAllByText(copy.today.briefNeverCard)).toHaveLength(2);
    const tiles = stats();
    expect(within(tiles).getAllByText(copy.today.briefNeverSub)).toHaveLength(3);
    expect(within(tiles).getAllByText('–').length).toBe(3);
  });

  it('reads a missing delivered_at with nothing in the brief as never delivered', async () => {
    renderApp({ wrap: briefOf(empty) });
    expect(await screen.findByText(copy.today.briefNever)).toBeInTheDocument();
    expect(screen.queryByText(copy.today.briefReady)).not.toBeInTheDocument();
  });

  it('reads a null delivered_at the same way', async () => {
    renderApp({ wrap: briefOf({ ...empty, delivered_at: null }) });
    expect(await screen.findByText(copy.today.briefNever)).toBeInTheDocument();
  });

  it('claims nothing when there are items but the server does not say when they came', async () => {
    renderApp({ wrap: briefOf({ ...empty, changed: [item] }) });
    await screen.findByText(item.title);
    expect(screen.queryByText(copy.today.briefReady)).not.toBeInTheDocument();
    expect(screen.queryByText(copy.today.briefNever)).not.toBeInTheDocument();
  });

  it('says when the last brief was, when it came on an earlier day', async () => {
    renderApp({ briefDelivery: 'stale' });
    expect(await screen.findByText(copy.today.briefStale('3 days ago'))).toBeInTheDocument();
    expect(screen.queryByText(copy.today.briefReady)).not.toBeInTheDocument();
  });

  it('does not tell a joke about an empty brief that came today either way: the calm line is for today only', async () => {
    renderApp({ briefDelivery: 'today', emptyBrief: true });
    expect(await screen.findByText(copy.today.briefReady)).toBeInTheDocument();
    expect(screen.getByText(copy.nothingWaiting)).toBeInTheDocument();
  });

  it('follow-ups page: no brief yet is said plainly, with how to get one', async () => {
    renderApp({ hash: '/followups', briefDelivery: 'never' });
    expect(await screen.findByText(copy.followups.emptyNever)).toBeInTheDocument();
    expect(screen.getByText(copy.followups.countLineNever)).toBeInTheDocument();
    expect(screen.queryByText(copy.followups.emptyAll)).not.toBeInTheDocument();
  });

  it('says Slack items are hidden when the server withheld them, and only then', async () => {
    const withheld: Brief = { ...empty, changed: [item], delivered_at: iso(Date.now() - 1000), status: 'today', slack_withheld: true };
    renderApp({ wrap: briefOf(withheld) });
    expect(await screen.findByText(copy.today.slackWithheld)).toBeInTheDocument();
    expect(copy.today.slackWithheld).toBe('Slack items are hidden until PM OS can check your access.');
  });

  it('follow-ups page: says Slack items are hidden when the server withheld them', async () => {
    renderApp({ hash: '/followups', wrap: briefOf({ ...empty, status: 'today', slack_withheld: true }) });
    expect(await screen.findByText(copy.today.slackWithheld)).toBeInTheDocument();
  });

  it('says nothing about Slack when nothing was withheld', async () => {
    renderApp();
    expect(await screen.findByText(copy.today.briefReady)).toBeInTheDocument();
    expect(screen.queryByText(copy.today.slackWithheld)).not.toBeInTheDocument();
  });

  it('follow-ups page: a stale brief gets no calm joke', async () => {
    renderApp({ hash: '/followups', briefDelivery: 'stale', emptyBrief: true });
    expect(await screen.findByText(copy.followups.emptyStale)).toBeInTheDocument();
    expect(screen.queryByText(copy.followups.emptyAll)).not.toBeInTheDocument();
    expect(screen.queryByText(copy.nothingWaiting)).not.toBeInTheDocument();
  });
});

describe('briefTiming', () => {
  const now = Date.parse('2026-11-17T12:00:00Z');
  it('is today, stale or never from delivered_at', () => {
    expect(briefTiming({ ...empty, delivered_at: iso(now - 60_000) }, now)).toBe('today');
    expect(briefTiming({ ...empty, delivered_at: iso(now - 3 * day) }, now)).toBe('stale');
    expect(briefTiming({ ...empty, delivered_at: null }, now)).toBe('never');
    expect(briefTiming(empty, now)).toBe('never');
  });
  it('uses status when delivered_at is missing, and reports unknown for items with no word on when', () => {
    expect(briefTiming({ ...empty, status: 'today' }, now)).toBe('today');
    expect(briefTiming({ ...empty, status: 'stale' }, now)).toBe('stale');
    expect(briefTiming({ ...empty, status: 'never' }, now)).toBe('never');
    expect(briefTiming({ ...empty, changed: [item] }, now)).toBe('unknown');
    expect(briefTiming({ ...empty, delivered_at: 'not a date' }, now)).toBe('never');
  });
  it("trusts the server's status over its own reading of delivered_at", () => {
    expect(briefTiming({ ...empty, status: 'stale', delivered_at: iso(now - 1000) }, now)).toBe('stale');
    expect(briefTiming({ ...empty, status: 'never', delivered_at: null }, now)).toBe('never');
  });
  it('reads delivered_at as within 24 hours, as the server does', () => {
    expect(briefTiming({ ...empty, delivered_at: iso(now - 23 * 3600_000) }, now)).toBe('today');
    expect(briefTiming({ ...empty, delivered_at: iso(now - 25 * 3600_000) }, now)).toBe('stale');
  });
});
