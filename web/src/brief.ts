import type { Brief } from './api/types';

/**
 * Where the daily brief stands, as far as the server says:
 * - `today`: delivered today, so "your brief is in" is true.
 * - `stale`: delivered on an earlier day.
 * - `never`: no brief has ever run (`delivered_at` is null or absent, and there is nothing in it).
 * - `unknown`: there are items but the server does not say when they came, so nothing is claimed.
 */
export type BriefTiming = 'today' | 'stale' | 'never' | 'unknown';

const sameLocalDay = (a: number, b: number) => new Date(a).toDateString() === new Date(b).toDateString();

export function briefTiming(brief: Brief, now = Date.now()): BriefTiming {
  const at = brief.delivered_at ? Date.parse(brief.delivered_at) : NaN;
  if (Number.isFinite(at)) return sameLocalDay(at, now) ? 'today' : 'stale';
  if (brief.status === 'today' || brief.status === 'stale') return brief.status;
  const empty = brief.changed.length + brief.waiting_on_you.length + brief.you_wait_on.length === 0;
  return empty ? 'never' : 'unknown';
}
