import { createContext, useContext } from 'react';
import type { Api, Me } from './api';
import type { Theme } from './theme';
import { ApiError, isServerErrorCode } from './api/types';

export interface AppContextValue {
  api: Api;
  me: Me;
  setMe: (me: Me) => void;
  refreshMe: () => Promise<void>;
  /** A fixed poll interval in ms (tests). Left out, polling follows the schedule in polling.ts. */
  pollMs: number | undefined;
  navigate: (path: string) => void;
  theme: Theme;
  setTheme: (t: Theme) => void;
  /** Open the command palette, optionally with text already typed. */
  openPalette: (text?: string) => void;
}

export const AppContext = createContext<AppContextValue | null>(null);

export function useApp(): AppContextValue {
  const ctx = useContext(AppContext);
  if (!ctx) throw new Error('useApp outside AppContext');
  return ctx;
}

/**
 * A plain sentence from any thrown value. ApiError messages for a code in the contract are
 * pmos-web's own plain sentences; for any other code the server's message is never shown, only
 * `fallback` (as `errorText`, the re-check of eb24d28, LH).
 */
export function messageOf(e: unknown, fallback: string): string {
  if (e instanceof ApiError && !isServerErrorCode(e.code) && e.code !== 'network') return fallback;
  return e instanceof Error && e.message ? e.message : fallback;
}
