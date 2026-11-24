import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from 'react';
import type { ReactNode } from 'react';
import type { Bots, Brief, DraftSummary, ReviewList, Task } from './api';
import { canAsk } from './bots';
import { messageOf, useApp } from './app-context';
import { copy } from './copy';
import { effectiveStatus, sortDrafts } from './drafts';
import { errorText, isDefiniteRefusal } from './errors';
import { useTour } from './onboarding';
import { waitingCount } from './reviews';

/** 'off' means the brief answered 501 not_built (or failed): follow-ups are hidden everywhere. */
export type BriefState = 'loading' | 'ready' | 'off';

export interface DataValue {
  drafts: DraftSummary[] | null;
  draftsError: string | null;
  /** Set when the server left pending drafts out of the list: `total` is how many wait (null if it did not say). */
  draftsCut: { total: number | null } | null;
  reloadDrafts: () => Promise<void>;
  brief: Brief | null;
  briefState: BriefState;
  setBrief: (update: (b: Brief) => Brief) => void;
  tasks: Task[] | null;
  reloadTasks: () => Promise<void>;
  /** Put a fresher copy of a task into the list (the thread view polls one task). */
  upsertTask: (t: Task) => void;
  /** Drafts that need a decision now. */
  pendingCount: number;
  runningCount: number;
  /** The reviewer's inbox (null until loaded, and always for someone who isn't a reviewer). */
  reviews: ReviewList | null;
  reviewsError: unknown;
  reloadReviews: () => Promise<void>;
  /** Drafts that wait for this reviewer's advice. */
  reviewCount: number;
  /**
   * The catalog and the person's own team (`GET /api/bots`): null until loaded, and always for
   * someone who may not ask. `botsError` is the failure of the last read.
   */
  bots: Bots | null;
  botsError: unknown;
  reloadBots: () => Promise<void>;
  /** Put the answer of a team change in place. */
  setBots: (b: Bots) => void;
  /**
   * Today sends a person to "Pick your team" once: true the first time it is asked in this
   * signed-in session, false after, so going back to Today is never blocked.
   */
  takeTeamStep: () => boolean;
}

const DataContext = createContext<DataValue | null>(null);

export function useData(): DataValue {
  const ctx = useContext(DataContext);
  if (!ctx) throw new Error('useData outside DataProvider');
  return ctx;
}

/** The lists the navigation counts and several pages share, loaded once and refreshed on demand. */
export function DataProvider({ children }: { children: ReactNode }) {
  const { api, me } = useApp();
  // The admin door (slice H10) serves no PM route: nothing of a PM's own is loaded there.
  const pmDoor = me.door !== 'admin';
  const reviewer = pmDoor && me.reviewer === true;
  const [reviews, setReviews] = useState<ReviewList | null>(null);
  const [reviewsError, setReviewsError] = useState<unknown>(null);
  const [drafts, setDrafts] = useState<DraftSummary[] | null>(null);
  const [draftsCut, setDraftsCut] = useState<{ total: number | null } | null>(null);
  const [draftsError, setDraftsError] = useState<string | null>(null);
  const [brief, setBriefState] = useState<Brief | null>(null);
  const [briefState, setBriefStatus] = useState<BriefState>('loading');
  const [tasks, setTasks] = useState<Task[] | null>(null);
  const asks = pmDoor && canAsk(me);
  const [bots, setBotsState] = useState<Bots | null>(null);
  const [botsError, setBotsError] = useState<unknown>(null);
  const teamStepTaken = useRef(false);

  const reloadDrafts = useCallback(async () => {
    try {
      const l = await api.listDrafts();
      setDrafts(sortDrafts(l.drafts));
      setDraftsCut(l.truncated ? { total: l.total_pending } : null);
      setDraftsError(null);
    } catch (e) {
      setDraftsError(messageOf(e, copy.errors.loadHomeFailed));
    }
  }, [api]);

  const reloadBrief = useCallback(async () => {
    try {
      setBriefState(await api.brief());
      setBriefStatus('ready');
    } catch {
      // 501 not_built hides the brief entirely. Any other failure hides it too, quietly,
      // because the app works without it.
      setBriefState(null);
      setBriefStatus('off');
    }
  }, [api]);

  const reloadTasks = useCallback(async () => {
    try {
      const list = await api.listTasks();
      setTasks([...list].sort((a, b) => b.created_at.localeCompare(a.created_at)));
    } catch {
      setTasks((t) => t ?? []);
    }
  }, [api]);

  const reloadReviews = useCallback(async () => {
    if (!reviewer) return;
    try {
      setReviews(await api.listReviews());
      setReviewsError(null);
    } catch (e) {
      setReviewsError(e);
    }
  }, [api, reviewer]);

  const reloadBots = useCallback(async () => {
    if (!asks) return;
    try {
      setBotsState(await api.bots());
      setBotsError(null);
    } catch (e) {
      setBotsError(e);
    }
  }, [api, asks]);

  useEffect(() => {
    if (asks) void reloadBots();
    else {
      setBotsState(null);
      setBotsError(null);
    }
  }, [asks, reloadBots]);

  const setBots = useCallback((b: Bots) => {
    setBotsState(b);
    setBotsError(null);
  }, []);

  const takeTeamStep = useCallback(() => {
    if (teamStepTaken.current) return false;
    teamStepTaken.current = true;
    return true;
  }, []);

  useEffect(() => {
    if (!pmDoor) {
      setDrafts([]);
      setTasks([]);
      setBriefState(null);
      setBriefStatus('off');
      return;
    }
    void reloadDrafts();
    void reloadBrief();
    void reloadTasks();
  }, [pmDoor, reloadDrafts, reloadBrief, reloadTasks]);

  useEffect(() => {
    if (reviewer) void reloadReviews();
    else {
      setReviews(null);
      setReviewsError(null);
    }
  }, [reviewer, reloadReviews]);

  const upsertTask = useCallback((t: Task) => {
    setTasks((list) => {
      const rest = (list ?? []).filter((x) => x.id !== t.id);
      return [t, ...rest].sort((a, b) => b.created_at.localeCompare(a.created_at));
    });
  }, []);

  const setBrief = useCallback((update: (b: Brief) => Brief) => {
    setBriefState((b) => (b ? update(b) : b));
  }, []);

  const value = useMemo<DataValue>(() => {
    const now = Date.now();
    return {
      drafts,
      draftsError,
      draftsCut,
      reloadDrafts,
      brief,
      briefState,
      setBrief,
      tasks,
      reloadTasks,
      upsertTask,
      pendingCount: (drafts ?? []).filter((d) => effectiveStatus(d, now) === 'pending').length,
      runningCount: (tasks ?? []).filter((t) => t.status === 'queued' || t.status === 'working').length,
      reviews,
      reviewsError,
      reloadReviews,
      reviewCount: reviews ? waitingCount(reviews.drafts) : 0,
      bots,
      botsError,
      reloadBots,
      setBots,
      takeTeamStep,
    };
  }, [drafts, draftsError, draftsCut, reloadDrafts, brief, briefState, setBrief, tasks, reloadTasks, upsertTask, reviews, reviewsError, reloadReviews, bots, botsError, reloadBots, setBots, takeTeamStep]);

  return <DataContext.Provider value={value}>{children}</DataContext.Provider>;
}

/**
 * Start a request and open it as a thread. Used by the home composer, the palette, follow-up
 * actions and Try again. Returns false (with `error` set) when nothing was started.
 */
export function useAsk() {
  const { navigate } = useApp();
  const { upsertTask } = useData();
  const { step } = useTour();
  const [asking, setAsking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const start = useCallback(
    async (run: () => Promise<Task>, fallback: string, onError?: (e: unknown) => void): Promise<boolean> => {
      setAsking(true);
      setError(null);
      try {
        const t = await run();
        upsertTask(t);
        // The guided start's first step: asking for something.
        void step('ask');
        navigate(`/a/${encodeURIComponent(t.id)}`);
        return true;
      } catch (e) {
        // Only a definite refusal says nothing was started; anything else may have gone through.
        setError(isDefiniteRefusal(e) ? errorText(e, fallback) : copy.errors.unsureStarted);
        onError?.(e);
        return false;
      } finally {
        setAsking(false);
      }
    },
    [navigate, upsertTask, step],
  );
  return { start, asking, error, setError };
}
