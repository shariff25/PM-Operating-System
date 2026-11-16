import type { DraftStatus } from './api/types';

interface Timed {
  status: DraftStatus;
  expires_at: string | null;
}

/** A draft whose time ran out, whether or not the server has marked it yet. */
export function isExpired(d: Timed, now = Date.now()): boolean {
  if (d.status === 'expired') return true;
  return d.status === 'pending' && d.expires_at !== null && Date.parse(d.expires_at) <= now;
}

export function effectiveStatus(d: Timed, now = Date.now()): DraftStatus {
  return isExpired(d, now) ? 'expired' : d.status;
}

/** Drafts that need a decision first, then expired ones, then everything else in server order. */
export function sortDrafts<T extends Timed>(drafts: T[], now = Date.now()): T[] {
  const rank = (d: T) => {
    const s = effectiveStatus(d, now);
    return s === 'pending' ? 0 : s === 'expired' ? 1 : 2;
  };
  return drafts
    .map((d, i) => ({ d, i }))
    .sort((a, b) => rank(a.d) - rank(b.d) || a.i - b.i)
    .map((x) => x.d);
}
