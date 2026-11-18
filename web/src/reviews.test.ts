import { describe, expect, it, vi } from 'vitest';
import { validSlackId } from './admin/team';
import { createRealClient } from './api/real';
import type { ReviewDraft, ReviewSummary } from './api/types';
import { NOTE_MAX, isRestricted, noteLength, noteToSend, reviewState, versionOf, waitingCount } from './reviews';
import { parseRoute, reviewHref } from './url';

const HASH = 'a'.repeat(64);
const base: ReviewSummary = {
  id: 'apr_1', owner: 'ana', owner_name: 'Ana Silva', kind: 'jira_issue', status: 'pending',
  created_at: '2026-11-18T10:00:00Z', expires_at: null, restricted: false, revision: 2,
  title: 'A draft', edited: false, args_hash: HASH, my_recommendation: null,
};
const mine = (verdict: 'approve' | 'changes', stale = false) => ({
  verdict, note: null, note_removed: false, revision: 2, created_at: '2026-11-18T10:00:00Z', stale,
});

describe('reviews: what a reviewer sees of a draft', () => {
  it('treats anything not marked restricted: false as restricted, as the server does', () => {
    expect(isRestricted(base)).toBe(false);
    expect(isRestricted({ restricted: true })).toBe(true);
    expect(isRestricted({} as ReviewSummary)).toBe(true);
    expect(isRestricted({ restricted: 'false' as unknown as boolean })).toBe(true);
  });

  it('says where each draft stands for the reviewer', () => {
    expect(reviewState(base)).toBe('todo');
    expect(reviewState({ ...base, my_recommendation: mine('approve') })).toBe('approve');
    expect(reviewState({ ...base, my_recommendation: mine('changes') })).toBe('changes');
    expect(reviewState({ ...base, my_recommendation: mine('approve', true) })).toBe('stale');
    expect(reviewState({ ...base, restricted: true, my_recommendation: mine('approve') })).toBe('restricted');
  });

  it('counts what waits: no advice yet, or advice on an earlier version; never a restricted one', () => {
    const list = [
      base,
      { ...base, my_recommendation: mine('approve') },
      { ...base, my_recommendation: mine('changes', true) },
      { ...base, restricted: true },
    ];
    expect(waitingCount(list)).toBe(2);
  });

  it('sends back the version as read, and refuses to guess one the server did not give', () => {
    expect(versionOf(base as ReviewDraft)).toEqual({ revision: 2, args_hash: HASH });
    expect(versionOf({ ...base, revision: null })).toBeNull();
    expect(versionOf({ ...base, revision: 0 })).toBeNull();
    expect(versionOf({ ...base, args_hash: 'ABC' })).toBeNull();
    expect(versionOf({ ...base, args_hash: undefined })).toBeNull();
    expect(versionOf({ ...base, restricted: true })).toBeNull();
  });

  it('counts note characters as the server does, and sends no empty note', () => {
    expect(NOTE_MAX).toBe(1000);
    expect(noteLength('héllo')).toBe(5);
    expect(noteLength('👍👍')).toBe(2);
    expect(noteToSend('  \n ')).toBeUndefined();
    expect(noteToSend(' ok ')).toBe(' ok ');
  });

  it('routes #/reviews and #/reviews/<id>, and never a malformed id', () => {
    expect(parseRoute('/reviews')).toEqual({ name: 'reviews' });
    expect(parseRoute('/reviews/apr_1')).toEqual({ name: 'review', id: 'apr_1' });
    expect(parseRoute('/reviews/..%2Fadmin')).toEqual({ name: 'home' });
    expect(parseRoute('/reviews/a/b')).toEqual({ name: 'home' });
    expect(reviewHref('apr 1')).toBe('#/reviews/apr%201');
  });

  it('checks a Slack member id the way the server does', () => {
    for (const ok of ['U012AB3CD', 'W0AB', 'UAB']) expect(validSlackId(ok), ok).toBe(true);
    for (const bad of ['u012AB3CD', 'U0', 'C012AB3CD', 'U012-AB', 'U012ab', `U${'A'.repeat(32)}`, '']) expect(validSlackId(bad), bad).toBe(false);
  });
});

const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });

describe('real client: reviews', () => {
  it('reads the inbox and a draft, and recommends with the version read and the CSRF token', async () => {
    const fetchImpl = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(json(200, { handle: 'ben', name: 'Ben', roles: ['reviewer'], csrf_token: 'tok-9', first_visit: false, hints_seen: [], nudges: true, reviewer: true }))
      .mockResolvedValueOnce(json(200, { drafts: [], total: 0, truncated: false, pms: ['ana'] }))
      .mockResolvedValueOnce(json(200, base))
      .mockImplementation(async () => json(200, { recommendation: mine('approve'), draft: base }));
    const api = createRealClient({ fetchImpl });
    expect((await api.me()).reviewer).toBe(true);
    expect(await api.listReviews()).toEqual({ drafts: [], total: 0, truncated: false, pms: ['ana'] });
    await api.getReview('apr_1');
    await api.recommend('apr_1', { verdict: 'approve', note: 'Looks right.', revision: 2, args_hash: HASH });
    await api.recommend('..', { verdict: 'changes', note: '   ', revision: 2, args_hash: HASH });
    const calls = fetchImpl.mock.calls.slice(1).map(([url, init]) => [init!.method, url, init!.body ?? null, (init!.headers as Record<string, string>)['X-Pmos-Csrf'] ?? null]);
    expect(calls).toEqual([
      ['GET', '/api/reviews', null, null],
      ['GET', '/api/reviews/apr_1', null, null],
      ['POST', '/api/reviews/apr_1/recommendation', JSON.stringify({ verdict: 'approve', note: 'Looks right.', revision: 2, args_hash: HASH }), 'tok-9'],
      // An empty note is no note; a path of dots alone is never sent as a path step.
      ['POST', '/api/reviews/%00/recommendation', JSON.stringify({ verdict: 'changes', revision: 2, args_hash: HASH }), 'tok-9'],
    ]);
  });

  it('reads an older server: no truncated, total or pms', async () => {
    const fetchImpl = vi.fn<typeof fetch>().mockResolvedValue(json(200, { drafts: [base] }));
    expect(await createRealClient({ fetchImpl }).listReviews()).toEqual({ drafts: [base], total: 1, truncated: false, pms: [] });
  });

  it('keeps the code of a refused recommendation', async () => {
    const fetchImpl = vi.fn<typeof fetch>().mockResolvedValue(json(409, { code: 'draft_changed', error: 'Changed.' }));
    const err = await createRealClient({ fetchImpl }).recommend('apr_1', { verdict: 'approve', revision: 1, args_hash: HASH }).catch((e: unknown) => e);
    expect(err).toMatchObject({ status: 409, code: 'draft_changed' });
  });
});
