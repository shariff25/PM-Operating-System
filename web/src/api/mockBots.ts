// Sample data for the bot marketplace (docs/web-api.md, "Bots"): the catalog, each person's own
// team, and the rules pmos-web applies to a team change and to an ask that names a bot. Fake text
// only. Used by the sample-data client (tests and the dev server).
import { ApiError } from './types';
import type { BotCard, BotRoutine, Bots, Me, SeenName, TeamChange, WithdrawnBot } from './types';

type CatalogEntry = Omit<BotCard, 'on_team' | 'added_at' | 'added_via' | 'new' | 'drafts_this_month' | 'last_used_at' | 'routines'> & {
  offered: boolean;
};

/** The catalog in its order. Scout is defined but withdrawn: an admin set `offered: false`. */
export const MOCK_CATALOG: CatalogEntry[] = [
  {
    key: 'nudge',
    display_name: 'Nudge',
    title: 'PM assistant',
    mascot: 'nudge',
    always_on: true,
    offered: true,
    summary: 'The generalist. Answers questions, writes up meetings and drafts PRDs and decisions.',
    examples: ['Draft a PRD for the grace period', 'What is blocked this week?', 'Critique this idea'],
    can_do: ['ask', 'prd', 'daily-brief', 'status-update', 'meeting-notes', 'decision-doc', 'critique', 'strategy', 'jira-draft', 'slack-reply'],
    reads: ['jira', 'confluence', 'slack', 'docs'],
    drafts: ['Jira issues', 'Confluence pages', 'Slack replies', 'Slack messages'],
    needs: [],
  },
  {
    key: 'chief',
    display_name: 'Chief',
    title: 'Chief of Staff',
    mascot: 'chief',
    always_on: false,
    offered: true,
    preview: true,
    summary: 'Runs your week: what needs you, what is slipping, and what to tell your leadership.',
    examples: ['Prep me for the staff meeting', 'What slipped since Monday?', 'Draft my weekly update'],
    can_do: ['daily-brief', 'meeting-prep', 'week-review', 'meeting-notes', 'status-update', 'exec-brief', 'decision-doc', 'okr-draft'],
    reads: ['jira', 'confluence', 'slack', 'docs'],
    drafts: ['Jira issues', 'Confluence pages', 'Slack replies'],
    needs: [],
  },
  {
    key: 'sprout',
    display_name: 'Sprout',
    title: 'Growth PM',
    mascot: 'sprout',
    always_on: false,
    offered: true,
    preview: true,
    summary: 'Finds and tests growth: funnels, experiments, sizing and pricing.',
    examples: ['Where does the signup funnel leak?', 'Design a test for the paywall', 'Size the annual-plan idea'],
    can_do: ['problem-framing', 'metrics-tree', 'funnel-diagnosis', 'experiment-design', 'experiment-readout', 'experiment-backlog', 'opportunity-sizing', 'pricing-proposal'],
    reads: ['jira', 'confluence', 'docs'],
    drafts: ['Jira issues', 'Confluence pages'],
    needs: [
      {
        source: 'analytics',
        label: 'an analytics source',
        without: 'I work from numbers you paste in or that a Confluence page holds, and I say so.',
        met: false,
      },
    ],
  },
  {
    key: 'echo',
    display_name: 'Echo',
    title: 'Outbound PM',
    mascot: 'echo',
    always_on: false,
    offered: true,
    preview: true,
    summary: 'Takes the product to market: launches, positioning, sales and customers.',
    examples: ['Write release notes for 4.2', 'Build a battlecard vs. Acme', 'Plan the SSO launch'],
    can_do: ['release-notes', 'launch-checklist', 'positioning-brief', 'battlecard', 'competitive-brief', 'call-synth', 'partner-brief', 'advisory-draft', 'poc-plan'],
    reads: ['jira', 'confluence', 'docs'],
    drafts: ['Confluence pages', 'Jira issues'],
    needs: [],
  },
  {
    key: 'scout',
    display_name: 'Scout',
    title: 'Research PM',
    mascot: 'unknown',
    always_on: false,
    offered: false,
    summary: 'Withdrawn.',
    examples: ['Withdrawn'],
    can_do: [],
    reads: [],
    drafts: [],
    needs: [],
  },
];

/** The line Echo's drafts carry in their text, from the back end (the owner's decision of 2026-11-21). */
export const ECHO_LINE = 'Draft for internal review. Not approved for external use.';

export interface MockBotsOptions {
  /**
   * What the tool server's catalog holds: `offered` (the default: Nudge, Chief, Sprout and Echo),
   * `nudge_only` (nothing offered besides Nudge, as before the marketplace), or `unavailable` (the
   * team and the catalog can't be read: `team_unavailable`, and `GET /api/bots` answers 503).
   */
  catalog?: 'offered' | 'nudge_only' | 'unavailable';
  /** The team at the start, besides Nudge. Default: Chief for a returning person, nobody on a first visit. */
  team?: string[];
  /** A bot on the team that an admin has since withdrawn (Scout). */
  withdrawnBot?: boolean;
  /** Refuse every team change with this code. `unknown_outcome` makes the change first, then loses the answer. */
  teamError?: string;
}

const TEAM_STATUS: Record<string, number> = {
  bad_request: 400,
  not_allowed: 403,
  not_a_member: 403,
  bad_origin: 403,
  bad_csrf: 403,
  always_on: 409,
  bot_unavailable: 409,
  team_full: 409,
  rate_limited: 429,
  upstream_unavailable: 502,
  unknown_outcome: 502,
  assertion_required: 503,
  unavailable: 503,
  users_unavailable: 503,
};

const KEY = /^[a-z0-9]+(-[a-z0-9]+)*$/;
const HOUR = 3600_000;

interface TeamRow {
  added_at: string;
  added_via: 'onboarding' | 'bots_page' | 'picker' | 'seed';
}

/** A routine of the person's, as the runtime keeps it: `persona` null is Nudge's. */
export interface MockRoutine extends BotRoutine {
  persona: string | null;
}

export interface MockTeam {
  /** The team fields of `Me`, worked out as pmos-web does (docs/web-api.md, "Me"). */
  meFields(me: Me): Pick<Me, 'team' | 'ask_persona' | 'team_unavailable' | 'show_team_step' | 'show_team_note'>;
  bots(me: Me): Bots;
  change(me: Me, change: TeamChange): Bots;
  /** The checks `POST /api/tasks` makes for `persona`; answers the key to store (null for Nudge). */
  checkAsk(me: Me, persona: string | undefined): string | null;
  /** Whether attribution may name this key now (`persona_offered`). */
  offered(persona: string | null | undefined): boolean;
  /** What a reload of the dev server keeps. */
  snapshot(): { team: [string, TeamRow][]; lastAsked: string };
  restore(s: { team?: [string, TeamRow][]; lastAsked?: string }): void;
}

const catalogEntry = (key: string) => MOCK_CATALOG.find((c) => c.key === key);

export function createMockTeam(
  now0: number,
  options: MockBotsOptions & { firstVisit: boolean },
  usage: { draftsThisMonth: (persona: string | null) => number; lastUsed: (persona: string | null) => string | null },
): MockTeam {
  const catalogMode = options.catalog ?? 'offered';
  const iso = (t: number) => new Date(t).toISOString();
  const isOffered = (key: string) => {
    const c = catalogEntry(key);
    if (!c || !c.offered) return false;
    return catalogMode !== 'nudge_only' || c.always_on;
  };
  const offeredList = () => MOCK_CATALOG.filter((c) => isOffered(c.key));

  const team = new Map<string, TeamRow>();
  const seed = options.team ?? (options.firstVisit ? [] : ['chief']);
  for (const k of seed) team.set(k, { added_at: iso(now0 - 26 * HOUR), added_via: 'onboarding' });
  if (options.withdrawnBot) team.set('scout', { added_at: iso(now0 - 9 * 24 * HOUR), added_via: 'bots_page' });
  let lastAsked = seed.includes('chief') ? 'chief' : 'nudge';
  let changes = 0;

  const routines: MockRoutine[] = [
    { id: 'rtn_brief', persona: null, enabled: true, cron: '30 8 * * 1-5', tz: 'Europe/London', prompt: 'brief me', next_run_at: iso(now0 + 18 * HOUR) },
    { id: 'rtn_week', persona: 'chief', enabled: true, cron: '0 9 * * 1', tz: 'Europe/London', prompt: 'What slipped last week?', next_run_at: iso(now0 + 3 * 24 * HOUR) },
    { id: 'rtn_scout', persona: 'scout', enabled: true, cron: '0 10 * * 3', tz: 'Europe/London', prompt: 'What are customers asking about?', next_run_at: iso(now0 + 4 * 24 * HOUR) },
  ];
  const routinesOf = (persona: string | null): BotRoutine[] =>
    routines.filter((r) => r.persona === persona).map(({ persona: _p, ...r }) => (void _p, { ...r }));

  const canAsk = (me: Me) => (me.permissions ?? []).includes('ask-bot');
  const has = (me: Me, m: SeenName) => me.hints_seen.includes(m);
  /** `tourState(me).active` as the server works it out: the three steps only. */
  const tourOn = (me: Me) => {
    const steps = ['tour_asked', 'tour_opened', 'tour_decided'] as const;
    const started = me.first_visit || has(me, 'tour_started') || steps.some((s) => has(me, s));
    return me.nudges && !has(me, 'tour_skipped') && started && !steps.every((s) => has(me, s));
  };
  const teamKeys = () => ['nudge', ...offeredList().filter((c) => !c.always_on && team.has(c.key)).map((c) => c.key)];

  const botsOf = (me: Me): Bots => {
    if (!canAsk(me)) throw new ApiError(403, 'not_allowed', 'Your role does not allow asking.');
    if (catalogMode === 'unavailable') throw new ApiError(503, 'unavailable', 'The tool server is not answering.');
    const bots: BotCard[] = offeredList().map(({ offered: _o, ...c }) => {
      void _o;
      const row = team.get(c.key);
      const persona = c.always_on ? null : c.key;
      return {
        ...structuredClone(c),
        on_team: c.always_on || !!row,
        added_at: c.always_on ? null : (row?.added_at ?? null),
        added_via: c.always_on ? null : (row?.added_via ?? null),
        new: false,
        drafts_this_month: usage.draftsThisMonth(persona),
        last_used_at: usage.lastUsed(persona),
        routines: routinesOf(persona),
      };
    });
    const withdrawn: WithdrawnBot[] = [...team.entries()]
      .filter(([k]) => !isOffered(k))
      .map(([k, row]) => ({ key: k, added_at: row.added_at, added_via: row.added_via, drafts_this_month: usage.draftsThisMonth(k), last_used_at: usage.lastUsed(k), routines: routinesOf(k) }));
    return { bots, withdrawn };
  };

  return {
    meFields(me) {
      if (!canAsk(me)) return { team: [], ask_persona: null, team_unavailable: false, show_team_step: false, show_team_note: false };
      if (catalogMode === 'unavailable') return { team: ['nudge'], ask_persona: 'nudge', team_unavailable: true, show_team_step: false, show_team_note: false };
      const keys = teamKeys();
      const others = offeredList().some((c) => !c.always_on);
      const step = others && !has(me, 'team_picked') && !has(me, 'team_skipped') && tourOn(me);
      const note = others && !step && !has(me, 'team_picked') && !has(me, 'team_skipped') && !has(me, 'team_announced') && keys.length === 1;
      return { team: keys, ask_persona: keys.includes(lastAsked) ? lastAsked : 'nudge', team_unavailable: false, show_team_step: step, show_team_note: note };
    },

    bots: botsOf,

    change(me, body) {
      if (!canAsk(me)) throw new ApiError(403, 'not_allowed', 'Your role does not allow changing a team.');
      const add = body.add ?? [];
      const remove = body.remove ?? [];
      if (!['onboarding', 'bots_page', 'picker'].includes(body.via)) throw new ApiError(400, 'bad_request', 'Unknown via.');
      if (body.routines !== undefined && body.routines !== 'nudge' && body.routines !== 'pause') throw new ApiError(400, 'bad_request', 'Unknown routines.');
      if ([...add, ...remove].some((k) => !KEY.test(k) || k.length > 32) || add.length + remove.length === 0 || add.length > 16 || remove.length > 16) {
        throw new ApiError(400, 'bad_request', 'That change is not in a shape I can use.');
      }
      if (add.some((k) => remove.includes(k))) throw new ApiError(400, 'bad_request', 'A bot is both added and removed.');
      if ([...add, ...remove].some((k) => catalogEntry(k)?.always_on)) throw new ApiError(409, 'always_on', 'Nudge is always on the team.');
      const code = options.teamError;
      if (code && code !== 'unknown_outcome') throw new ApiError(TEAM_STATUS[code] ?? 400, code, 'Refused by the sample data.');
      if (catalogMode === 'unavailable') throw new ApiError(503, 'unavailable', 'The tool server is not answering.');
      if (add.some((k) => !isOffered(k))) throw new ApiError(409, 'bot_unavailable', 'That bot is not offered.');
      if (++changes > 30) throw new ApiError(429, 'rate_limited', 'Too many team changes this hour.');
      const after = new Set([...team.keys(), ...add].filter((k) => !remove.includes(k)));
      if (after.size + 1 > 16) throw new ApiError(409, 'team_full', 'A team holds at most 16 bots.');
      // A removed bot's routines move (or pause) before the bot leaves.
      for (const r of routines) {
        if (r.persona !== null && remove.includes(r.persona)) {
          if (body.routines === 'pause') r.enabled = false;
          else r.persona = null;
        }
      }
      for (const k of remove) team.delete(k);
      for (const k of add) if (!team.has(k)) team.set(k, { added_at: iso(Date.now()), added_via: body.via });
      if (code === 'unknown_outcome') throw new ApiError(502, 'unknown_outcome', 'The answer was lost.');
      return botsOf(me);
    },

    checkAsk(me, persona) {
      if (persona === undefined || persona === 'nudge') {
        lastAsked = 'nudge';
        return null;
      }
      if (!KEY.test(persona)) throw new ApiError(400, 'bad_request', 'That is not a bot key.');
      if (catalogMode === 'unavailable') throw new ApiError(503, 'unavailable', 'The tool server is not answering.');
      if (!team.has(persona)) throw new ApiError(409, 'bot_not_on_team', 'That bot is not on your team.');
      if (!isOffered(persona)) throw new ApiError(409, 'bot_unavailable', 'That bot is not offered any more.');
      void me;
      lastAsked = persona;
      return persona;
    },

    offered(persona) {
      if (persona === null || persona === undefined) return true;
      if (catalogMode === 'unavailable') return false;
      return isOffered(persona);
    },

    snapshot() {
      return { team: [...team.entries()], lastAsked };
    },
    restore(s) {
      if (s.team) {
        team.clear();
        for (const [k, row] of s.team) team.set(k, row);
      }
      if (s.lastAsked) lastAsked = s.lastAsked;
    },
  };
}
