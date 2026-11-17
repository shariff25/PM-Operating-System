import { useCallback, useEffect, useRef, useState } from 'react';
import { ApiError } from '../api';
import { messageOf } from '../app-context';

export interface Loaded<T> {
  data: T | null;
  /** A plain sentence when the load failed (and it was not a refusal). */
  error: string | null;
  /** The server answered 403: this person's role does not allow it. */
  forbidden: boolean;
  /** True while a request is out, also when there is older data on screen. */
  loading: boolean;
  reload: () => void;
}

/**
 * Load one thing when the page opens, and again on `reload`. `load` must keep the same identity
 * between renders (wrap it in useCallback), or it will load in a loop.
 */
export function useLoad<T>(load: () => Promise<T>, failed: string): Loaded<T> {
  const [state, setState] = useState<{ data: T | null; error: string | null; forbidden: boolean; loading: boolean }>({
    data: null,
    error: null,
    forbidden: false,
    loading: true,
  });
  const latest = useRef(0);
  const invalidate = useCallback(() => {
    latest.current += 1;
  }, []);

  const run = useCallback(async () => {
    const mine = ++latest.current;
    setState((s) => ({ ...s, error: null, loading: true }));
    try {
      const data = await load();
      if (mine === latest.current) setState({ data, error: null, forbidden: false, loading: false });
    } catch (e) {
      if (mine !== latest.current) return;
      const forbidden = e instanceof ApiError && e.status === 403;
      setState((s) => ({ data: s.data, error: forbidden ? null : messageOf(e, failed), forbidden, loading: false }));
    }
  }, [load, failed]);

  useEffect(() => {
    void run();
    return invalidate;
  }, [run, invalidate]);

  return { ...state, reload: () => void run() };
}
