import { describe, expect, it } from 'vitest';
import type { Me, SeenName } from './api';
import { mayCheer, tourState, tourVariant } from './onboarding';

const me = (patch: Partial<Me> = {}): Me => ({
  handle: 'ana',
  name: 'Ana Silva',
  roles: ['pm'],
  csrf_token: 't',
  first_visit: true,
  hints_seen: [],
  nudges: true,
  mascot: true,
  permissions: ['ask-bot', 'decide-own-approval'],
  reviewer: false,
  ...patch,
});
const seen = (...marks: SeenName[]) => ({ hints_seen: marks });

describe('the guided start', () => {
  it('is on for a first visit, at step 1 of 3', () => {
    const s = tourState(me());
    expect(s.active).toBe(true);
    expect(s.variant).toBe('pm');
    expect(s.steps.map((x) => x.key)).toEqual(['ask', 'open', 'decide']);
    expect(s.done).toBe(0);
    expect(s.current).toBe('ask');
  });

  it('counts each step done, in any order, and moves to the first one left', () => {
    let s = tourState(me(seen('tour_started', 'tour_asked')));
    expect([s.done, s.current]).toEqual([1, 'open']);
    s = tourState(me({ first_visit: false, ...seen('tour_started', 'tour_asked', 'tour_opened') }));
    expect([s.active, s.done, s.current]).toEqual([true, 2, 'decide']);
    s = tourState(me({ first_visit: false, ...seen('tour_started', 'tour_opened') }));
    expect([s.done, s.current]).toEqual([1, 'ask']);
  });

  it('ends when all three are done', () => {
    const s = tourState(me({ first_visit: false, ...seen('tour_started', 'tour_asked', 'tour_opened', 'tour_decided') }));
    expect(s.active).toBe(false);
    expect(s.started).toBe(true);
    expect(s.done).toBe(3);
  });

  it('ends at once when skipped, and stays ended', () => {
    const s = tourState(me(seen('tour_skipped')));
    expect(s.active).toBe(false);
    expect(s.skipped).toBe(true);
    expect(tourState(me(seen('tour_started', 'tour_asked', 'tour_skipped'))).active).toBe(false);
  });

  it('starts again after "Show hints again", which leaves only tour_started', () => {
    const s = tourState(me({ first_visit: false, ...seen('tour_started') }));
    expect(s.active).toBe(true);
    expect(s.done).toBe(0);
  });

  it('is off for a returning person with no marks', () => {
    expect(tourState(me({ first_visit: false })).active).toBe(false);
    expect(tourState(me({ first_visit: false, ...seen('sources', 'ask', 'approve') })).active).toBe(false);
  });

  it('is hidden with "Hints and nudges" off, and does not depend on Show Nudge', () => {
    expect(tourState(me({ nudges: false })).active).toBe(false);
    expect(tourState(me({ mascot: false })).active).toBe(true);
  });

  it('knows each role', () => {
    expect(tourVariant(me({ roles: ['pm', 'reviewer'], reviewer: true }))).toBe('reviewer');
    expect(tourVariant(me({ roles: ['admin'] }))).toBe('admin');
    expect(tourVariant(me({ roles: ['admin', 'reviewer'], reviewer: true }))).toBe('admin');
    expect(tourVariant(me({ roles: ['auditor'] }))).toBe('auditor');
    expect(tourVariant(me({ roles: ['pm', 'auditor'] }))).toBe('pm');
  });

  it('gives an auditor no steps: a one-line welcome until it is dismissed', () => {
    const s = tourState(me({ roles: ['auditor'], permissions: ['view-audit-log'] }));
    expect(s.variant).toBe('auditor');
    expect(s.steps).toEqual([]);
    expect(s.active).toBe(true);
    expect(tourState(me({ roles: ['auditor'], ...seen('tour_skipped') })).active).toBe(false);
  });

  it('gives a reviewer and an admin the same three steps', () => {
    expect(tourState(me({ roles: ['pm', 'reviewer'], reviewer: true })).steps).toHaveLength(3);
    expect(tourState(me({ roles: ['admin'] })).steps).toHaveLength(3);
  });
});

describe('the "Approved!" moment', () => {
  it('is due once, for someone given the guided start', () => {
    expect(mayCheer(me())).toBe(true);
    expect(mayCheer(me({ first_visit: false, ...seen('tour_started', 'tour_asked', 'tour_opened', 'tour_decided') }))).toBe(true);
    expect(mayCheer(me(seen('tour_cheered')))).toBe(false);
  });

  it('is not due for a returning person, after a skip, or with nudges off', () => {
    expect(mayCheer(me({ first_visit: false }))).toBe(false);
    expect(mayCheer(me(seen('tour_skipped')))).toBe(false);
    expect(mayCheer(me({ nudges: false }))).toBe(false);
  });
});
