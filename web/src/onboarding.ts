import { useCallback } from 'react';
import type { Me, SeenName, TourMark } from './api';
import { useApp } from './app-context';

/**
 * The guided start, led by Nudge. It is built on the state the server already keeps for the
 * hints (docs/web-api.md, "Me"): `first_visit`, `hints_seen` (which also holds the guided
 * start's marks) and `nudges`. There is no second store.
 *
 * - It is on for a first visit, and after "Show hints again" (which clears `hints_seen` and then
 *   marks `tour_started`). A returning person, with no marks, never sees it.
 * - Each step is done by doing it in the app: asking (`tour_asked`), opening a draft
 *   (`tour_opened`), approving, rejecting or changing one (`tour_decided`).
 * - It ends when every step is done, or with "Skip the tour" (`tour_skipped`).
 * - "Hints and nudges" off hides it, as it hides the hints.
 * - With the bot marketplace it has a fourth step, first: "Pick your team" (`/start/team`). It is
 *   one of the steps while the server says to show it (`show_team_step`) and, once picked or
 *   skipped (`team_picked`, `team_skipped`), it stays one, done. Where nothing is offered besides
 *   Nudge (or an older server), the guided start keeps its three steps.
 */
export type TourStep = 'team' | 'ask' | 'open' | 'decide';
/** The steps done by doing them in the app, each with its own mark. */
export type DoStep = Exclude<TourStep, 'team'>;
export type TourVariant = 'pm' | 'reviewer' | 'admin' | 'auditor';

export const STEP_MARK: Record<DoStep, TourMark> = {
  ask: 'tour_asked',
  open: 'tour_opened',
  decide: 'tour_decided',
};

export interface TourState {
  /** Show the welcome card. */
  active: boolean;
  variant: TourVariant;
  /** The steps this person can do, in order; none for an auditor. */
  steps: { key: TourStep; done: boolean }[];
  done: number;
  /** The first step not yet done. */
  current: TourStep | null;
  /** The guided start ran for this person (now or before), and was not skipped. */
  started: boolean;
  skipped: boolean;
}

const has = (me: Pick<Me, 'hints_seen'>, mark: SeenName) => me.hints_seen.includes(mark);

/** "Pick your team" was picked or skipped: the step is done. */
export const teamStepDone = (me: Pick<Me, 'hints_seen'>) => has(me, 'team_picked') || has(me, 'team_skipped');

/** Who the welcome is for. An admin who also reviews is shown the admin's pointer. */
export function tourVariant(me: Pick<Me, 'roles' | 'reviewer' | 'permissions'>): TourVariant {
  const roles = me.roles;
  if (roles.includes('admin')) return 'admin';
  if (me.reviewer === true || roles.includes('reviewer')) return 'reviewer';
  // An auditor reads metadata only: no drafts, no asking.
  if (roles.includes('auditor') && !roles.includes('pm')) return 'auditor';
  return 'pm';
}

export function tourState(me: Me): TourState {
  const variant = tourVariant(me);
  const skipped = has(me, 'tour_skipped');
  const team = variant !== 'auditor' && (me.show_team_step === true || teamStepDone(me));
  const keys: TourStep[] = variant === 'auditor' ? [] : [...(team ? (['team'] as const) : []), 'ask', 'open', 'decide'];
  const steps = keys.map((key) => ({ key, done: key === 'team' ? teamStepDone(me) : has(me, STEP_MARK[key]) }));
  const done = steps.filter((s) => s.done).length;
  const started = !skipped && (me.first_visit || has(me, 'tour_started') || steps.some((s) => s.done));
  const finished = steps.length > 0 && done === steps.length;
  return {
    active: me.nudges && started && !finished,
    variant,
    steps,
    done,
    current: steps.find((s) => !s.done)?.key ?? null,
    started,
    skipped,
  };
}

/**
 * Should approving now show the one "Approved!" moment? Only for someone who was given the
 * guided start and did not skip it, only once, and not with "Hints and nudges" off.
 */
export function mayCheer(me: Me): boolean {
  return me.nudges && tourState(me).started && !has(me, 'tour_cheered');
}

/**
 * Record what was just done. Writes only while the guided start is on (or, for `tour_cheered`,
 * while a cheer is due), and only marks not already there. A failed write is not shown: the
 * guided start is a help, and the step can be done again.
 */
export function useTour() {
  const { api, me, setMe } = useApp();
  const mark = useCallback(
    async (...marks: TourMark[]) => {
      const state = tourState(me);
      const wanted = marks.filter(
        (m) => !me.hints_seen.includes(m) && (m === 'tour_cheered' ? mayCheer(me) : m === 'tour_started' || m === 'tour_skipped' ? true : state.active),
      );
      let latest: Me | null = null;
      for (const m of wanted) {
        try {
          latest = await api.markHintSeen(m);
        } catch {
          /* not shown: see above */
        }
      }
      if (latest) setMe(latest);
    },
    [api, me, setMe],
  );
  const step = useCallback((s: DoStep) => mark(STEP_MARK[s]), [mark]);
  return { state: tourState(me), mark, step };
}
