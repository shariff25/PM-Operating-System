import { describe, expect, it, vi } from 'vitest';
import type { BotCard, Me } from './api';
import { ApiError } from './api/types';
import { createRealClient } from './api/real';
import { MOCK_CATALOG } from './api/mockBots';
import { canAsk, defaultPersona, identify, isUnknownOutcome, skillLabel, teamErrorText, teamOf } from './bots';
import { copy } from './copy';
import { tourState } from './onboarding';

const card = (key: string): BotCard => {
  const { offered: _o, ...c } = MOCK_CATALOG.find((x) => x.key === key)!;
  void _o;
  return { ...c, on_team: false, added_at: null, added_via: null, new: false, drafts_this_month: 0, last_used_at: null, routines: [] };
};
const catalog = [card('nudge'), card('chief')];

const ME: Me = {
  handle: 'ana', name: 'Ana', roles: ['pm'], csrf_token: 't', first_visit: false, hints_seen: [], nudges: true,
  permissions: ['ask-bot'],
};

describe('who drafted it', () => {
  it('null (and the key "nudge") is Nudge', () => {
    expect(identify(null, true, catalog)).toEqual({ kind: 'nudge', name: 'Nudge', mascot: 'nudge' });
    expect(identify(undefined, undefined, null).kind).toBe('nudge');
    expect(identify('nudge', true, catalog).kind).toBe('nudge');
  });
  it('an offered key is looked up in the catalog', () => {
    expect(identify('chief', true, catalog)).toEqual({ kind: 'bot', name: 'Chief', mascot: 'chief' });
  });
  it('a withdrawn key, or one the catalog does not list, is "a bot no longer offered", never a name from the key', () => {
    expect(identify('chief', false, catalog)).toEqual({ kind: 'gone', name: copy.bots.gone, mascot: 'unknown' });
    expect(identify('sprout', true, catalog)).toEqual({ kind: 'gone', name: copy.bots.gone, mascot: 'unknown' });
  });
  it('while the catalog can\'t be read, an offered key is named plainly, with the neutral face', () => {
    expect(identify('chief', true, null)).toEqual({ kind: 'unchecked', name: copy.bots.unchecked, mascot: 'unknown' });
  });
});

describe('the team from Me', () => {
  it('reads an older server\'s missing fields as Nudge only', () => {
    expect(teamOf(ME)).toEqual(['nudge']);
    expect(defaultPersona(ME)).toBe('nudge');
  });
  it('has no team without ask-bot', () => {
    expect(canAsk({ permissions: ['view-audit-log'] })).toBe(false);
    expect(teamOf({ ...ME, permissions: [] })).toEqual([]);
  });
  it('defaults to the bot asked last only while it is on the team', () => {
    expect(defaultPersona({ ...ME, team: ['nudge', 'chief'], ask_persona: 'chief' })).toBe('chief');
    expect(defaultPersona({ ...ME, team: ['nudge'], ask_persona: 'chief' })).toBe('nudge');
  });
});

describe('labels', () => {
  it('labels skills from the app\'s own map, and shows an unknown key as the key', () => {
    expect(skillLabel('meeting-prep')).toBe('Meeting prep');
    expect(skillLabel('time-travel')).toBe('time-travel');
  });
});

describe('team change errors', () => {
  it('uses team wording where the general sentence is about drafts', () => {
    expect(teamErrorText(new ApiError(429, 'rate_limited', 'x'))).toBe(copy.bots.errors.codes.rate_limited);
    expect(teamErrorText(new ApiError(409, 'team_full', 'x'))).toBe(copy.errors.codes.team_full);
    // never the server's own sentence for a code the app does not know
    expect(teamErrorText(new ApiError(409, 'weird', 'Server text'))).toBe(copy.bots.errors.fallback);
  });
  it('an unknown outcome never says nothing changed', () => {
    const e = new ApiError(502, 'unknown_outcome', 'x');
    expect(isUnknownOutcome(e)).toBe(true);
    expect(teamErrorText(e)).not.toMatch(/Nothing changed/);
    expect(isUnknownOutcome(new ApiError(0, 'network', 'x'))).toBe(true);
    expect(isUnknownOutcome(new ApiError(409, 'team_full', 'x'))).toBe(false);
  });
});

describe('the guided start with "Pick your team"', () => {
  const first: Me = { ...ME, first_visit: true };
  it('is the first of four steps while the server says to show it', () => {
    const s = tourState({ ...first, show_team_step: true });
    expect(s.steps.map((x) => x.key)).toEqual(['team', 'ask', 'open', 'decide']);
    expect(s.current).toBe('team');
  });
  it('stays a step, done, once picked or skipped', () => {
    for (const mark of ['team_picked', 'team_skipped'] as const) {
      const s = tourState({ ...first, hints_seen: [mark] });
      expect(s.steps[0]).toEqual({ key: 'team', done: true });
      expect(s.done).toBe(1);
      expect(s.current).toBe('ask');
    }
  });
  it('keeps three steps where nothing besides Nudge is offered, and none for an auditor', () => {
    expect(tourState(first).steps).toHaveLength(3);
    expect(tourState({ ...first, roles: ['auditor'], show_team_step: true }).steps).toEqual([]);
  });
  it('a returning person never gets it', () => {
    expect(tourState({ ...ME, hints_seen: [] }).active).toBe(false);
  });
});

describe('the real client', () => {
  const json = (status: number, body: unknown) => new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
  it('reads the bots, changes the team with PUT, and asks a bot by key', async () => {
    const fetchImpl = vi
      .fn<typeof fetch>()
      .mockResolvedValueOnce(json(200, { ...ME, csrf_token: 'tok' }))
      .mockResolvedValueOnce(json(200, { bots: [], withdrawn: [] }))
      .mockResolvedValueOnce(json(200, { bots: [], withdrawn: [] }))
      .mockResolvedValueOnce(json(201, { id: 'tsk_1' }))
      .mockResolvedValueOnce(json(201, { id: 'tsk_2' }));
    const api = createRealClient({ fetchImpl });
    await api.me();
    await api.bots();
    await api.changeTeam({ remove: ['chief'], via: 'bots_page', routines: 'pause' });
    await api.createTask('Prep me', 'chief');
    await api.createTask('Plain');
    const calls = fetchImpl.mock.calls;
    expect(calls[1]![0]).toBe('/api/bots');
    expect(calls[1]![1]!.method).toBe('GET');
    expect(calls[2]![0]).toBe('/api/me/team');
    expect(calls[2]![1]!.method).toBe('PUT');
    expect(calls[2]![1]!.body).toBe(JSON.stringify({ remove: ['chief'], via: 'bots_page', routines: 'pause' }));
    expect((calls[2]![1]!.headers as Record<string, string>)['X-Pmos-Csrf']).toBe('tok');
    expect(calls[3]![1]!.body).toBe(JSON.stringify({ text: 'Prep me', persona: 'chief' }));
    // an ask without a bot sends no persona, as an older server expects
    expect(calls[4]![1]!.body).toBe(JSON.stringify({ text: 'Plain' }));
  });
  it('turns a refused ask into its code', async () => {
    const fetchImpl = vi.fn<typeof fetch>().mockResolvedValue(json(409, { code: 'bot_not_on_team', error: 'Not on your team.' }));
    const api = createRealClient({ fetchImpl });
    await expect(api.createTask('x', 'sprout')).rejects.toMatchObject({ status: 409, code: 'bot_not_on_team' });
  });
});
