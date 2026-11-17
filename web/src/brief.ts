import type { Brief } from './api/types';

/**
 * Where the daily brief stands (docs/web-api.md, "The daily brief and follow-ups"):
 * - `today`: the last brief came within the last 24 hours, so "your brief is in" is true.
 * - `stale`: a brief came before, more than 24 hours ago.
 * - `never`: none has ever been delivered. An older server that sends neither `status` nor
 *   `delivered_at` reads the same way.
 * - `unknown`: only when the server says nothing about timing but there are items in the brief,
 *   which the contract does not allow. Nothing is claimed about when they came.
 */
export type BriefTiming = 'today' | 'stale' | 'never' | 'unknown';

const DAY = 24 * 3600_000;

export function briefTiming(brief: Brief, now = Date.now()): BriefTiming {
  // The server's own word comes first: it knows what "within 24 hours" means for this PM.
  if (brief.status === 'never' || brief.status === 'today' || brief.status === 'stale') return brief.status;
  const at = brief.delivered_at ? Date.parse(brief.delivered_at) : NaN;
  if (Number.isFinite(at)) return now - at < DAY ? 'today' : 'stale';
  const empty = brief.changed.length + brief.waiting_on_you.length + brief.you_wait_on.length === 0;
  return empty ? 'never' : 'unknown';
}
