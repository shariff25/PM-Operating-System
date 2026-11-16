import { copy } from './copy';

const MIN = 60_000;
const HOUR = 60 * MIN;
const DAY = 24 * HOUR;

/** "5 min ago", "yesterday", "3 days ago", or a short date for anything older than a week. */
export function relTime(at: string, now = Date.now()): string {
  const t = Date.parse(at);
  if (Number.isNaN(t)) return '';
  const ms = Math.max(0, now - t);
  if (ms < MIN) return copy.time.justNow;
  if (ms < HOUR) return copy.time.minutes(Math.floor(ms / MIN));
  if (ms < DAY) return copy.time.hours(Math.floor(ms / HOUR));
  if (ms < 2 * DAY) return copy.time.yesterday;
  if (ms < 7 * DAY) return copy.time.days(Math.floor(ms / DAY));
  return new Date(t).toLocaleDateString(undefined, { day: 'numeric', month: 'short' });
}

/** A full, readable date and time for the Details tab. */
export function longTime(at: string): string {
  const t = Date.parse(at);
  if (Number.isNaN(t)) return at;
  return new Date(t).toLocaleString(undefined, {
    weekday: 'short',
    day: 'numeric',
    month: 'short',
    hour: '2-digit',
    minute: '2-digit',
  });
}

/** Time left before a Slack draft expires, or null when it has none or it already passed. */
export function timeLeft(expiresAt: string | null, now = Date.now()): string | null {
  if (!expiresAt) return null;
  const ms = Date.parse(expiresAt) - now;
  if (Number.isNaN(ms) || ms <= 0) return null;
  return copy.time.left(Math.floor(ms / HOUR), Math.max(1, Math.floor(ms / MIN)));
}
