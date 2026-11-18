import type { ReviewDraft, ReviewSummary } from './api/types';

// What a reviewer sees of a draft, worked out from what the server sent (docs/web-api.md,
// "Reviews"). Kept free of React so each rule has a unit test.

/** The longest note the server takes, in characters. */
export const NOTE_MAX = 1000;

/** The server treats any draft it does not mark `restricted: false` as restricted, and so does the app. */
export function isRestricted(r: Pick<ReviewSummary, 'restricted'>): boolean {
  return r.restricted !== false;
}

export type ReviewState = 'restricted' | 'todo' | 'approve' | 'changes' | 'stale';

/** Where a draft stands for this reviewer: their own advice on it, if any, and whether it still holds. */
export function reviewState(r: ReviewSummary): ReviewState {
  if (isRestricted(r)) return 'restricted';
  const mine = r.my_recommendation;
  if (!mine) return 'todo';
  if (mine.stale) return 'stale';
  return mine.verdict;
}

/** Drafts that wait for this reviewer: no advice yet, or advice on an earlier version. Restricted ones can't be advised on. */
export function waitingCount(list: readonly ReviewSummary[]): number {
  return list.filter((r) => {
    const s = reviewState(r);
    return s === 'todo' || s === 'stale';
  }).length;
}

/** The version a recommendation must name: both the revision and the hash, as read. Null when the server sent neither. */
export function versionOf(r: ReviewDraft): { revision: number; args_hash: string } | null {
  if (isRestricted(r)) return null;
  if (typeof r.revision !== 'number' || !Number.isInteger(r.revision) || r.revision < 1) return null;
  if (typeof r.args_hash !== 'string' || !/^[0-9a-f]{64}$/.test(r.args_hash)) return null;
  return { revision: r.revision, args_hash: r.args_hash };
}

/** Characters as the server counts them: one per code point, not per UTF-16 unit. */
export function noteLength(note: string): number {
  return [...note].length;
}

/** A note is sent only when it has something in it; the server trims it and an empty one is none. */
export function noteToSend(note: string): string | undefined {
  return note.trim() === '' ? undefined : note;
}
