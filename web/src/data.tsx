import { createContext, useCallback, useContext, useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';
import type { Brief, DraftSummary, Task } from './api';
import { messageOf, useApp } from './app-context';
import { copy } from './copy';
import { effectiveStatus, sortDrafts } from './drafts';
import { errorText, isDefiniteRefusal } from './errors';

/** 'off' means the brief answered 501 not_built (or failed): follow-ups are hidden everywhere. */
export type BriefState = 'loading' | 'ready' | 'off';

export interface DataValue {
  drafts: DraftSummary[] | null;
  draftsError: string | null;
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
}

const DataContext = createContext<DataValue | null>(null);

export function useData(): DataValue {
  const ctx = useContext(DataContext);
  if (!ctx) throw new Error('useData outside DataProvider');
  return ctx;
}

/** The lists the navigation counts and several pages share, loaded once and refreshed on demand. */
export function DataProvider({ children }: { children: ReactNode }) {
  const { api } = useApp();
  const [drafts, setDrafts] = useState<DraftSummary[] | null>(null);
  const [draftsError, setDraftsError] = useState<string | null>(null);
  const [brief, setBriefState] = useState<Brief | null>(null);
  const [briefState, setBriefStatus] = useState<BriefState>('loading');
  const [tasks, setTasks] = useState<Task[] | null>(null);

  const reloadDrafts = useCallback(async () => {
    try {
      setDrafts(sortDrafts(await api.listDrafts()));
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

  useEffect(() => {
    void reloadDrafts();
    void reloadBrief();
    void reloadTasks();
  }, [reloadDrafts, reloadBrief, reloadTasks]);

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
      reloadDrafts,
      brief,
      briefState,
      setBrief,
      tasks,
      reloadTasks,
      upsertTask,
      pendingCount: (drafts ?? []).filter((d) => effectiveStatus(d, now) === 'pending').length,
      runningCount: (tasks ?? []).filter((t) => t.status === 'queued' || t.status === 'working').length,
    };
  }, [drafts, draftsError, reloadDrafts, brief, briefState, setBrief, tasks, reloadTasks, upsertTask]);

  return <DataContext.Provider value={value}>{children}</DataContext.Provider>;
}

/**
 * Start a request and open it as a thread. Used by the home composer, the palette, follow-up
 * actions and Try again. Returns false (with `error` set) when nothing was started.
 */
export function useAsk() {
  const { navigate } = useApp();
  const { upsertTask } = useData();
  const [asking, setAsking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const start = useCallback(
    async (run: () => Promise<Task>, fallback: string): Promise<boolean> => {
      setAsking(true);
      setError(null);
      try {
        const t = await run();
        upsertTask(t);
        navigate(`/a/${encodeURIComponent(t.id)}`);
        return true;
      } catch (e) {
        // Only a definite refusal says nothing was started; anything else may have gone through.
        setError(isDefiniteRefusal(e) ? errorText(e, fallback) : copy.errors.unsureStarted);
        return false;
      } finally {
        setAsking(false);
      }
    },
    [navigate, upsertTask],
  );
  return { start, asking, error, setError };
}
