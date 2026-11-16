import { useCallback, useEffect, useState } from 'react';

export type Theme = 'system' | 'light' | 'dark';
const KEY = 'pmos-theme';

/** The saved choice. Storage can be missing or blocked; then it is "system". */
export function readTheme(): Theme {
  try {
    const v = window.localStorage.getItem(KEY);
    return v === 'light' || v === 'dark' ? v : 'system';
  } catch {
    return 'system';
  }
}

/** Light and dark set `data-theme` on <html>; system removes it and follows the OS. */
export function applyTheme(theme: Theme): void {
  const root = document.documentElement;
  if (theme === 'system') delete root.dataset.theme;
  else root.dataset.theme = theme;
}

export function useTheme(): [Theme, (t: Theme) => void] {
  const [theme, setThemeState] = useState<Theme>(readTheme);
  useEffect(() => applyTheme(theme), [theme]);
  const setTheme = useCallback((t: Theme) => {
    try {
      if (t === 'system') window.localStorage.removeItem(KEY);
      else window.localStorage.setItem(KEY, t);
    } catch {
      /* the choice still applies for this visit */
    }
    setThemeState(t);
  }, []);
  return [theme, setTheme];
}

/** A CSS media query as React state. Without matchMedia (tests) it is false. */
export function useMediaQuery(query: string): boolean {
  const get = () => (typeof window !== 'undefined' && window.matchMedia ? window.matchMedia(query).matches : false);
  const [matches, setMatches] = useState(get);
  useEffect(() => {
    if (!window.matchMedia) return;
    const mq = window.matchMedia(query);
    const on = () => setMatches(mq.matches);
    on();
    mq.addEventListener('change', on);
    return () => mq.removeEventListener('change', on);
  }, [query]);
  return matches;
}

/** Phone layout: bottom tab bar, panel as tabs under the header. */
export const PHONE_QUERY = '(max-width: 767px)';
