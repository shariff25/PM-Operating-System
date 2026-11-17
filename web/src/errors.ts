import { SERVER_ERROR_CODES, ApiError, isServerErrorCode } from './api/types';
import { copy } from './copy';

/**
 * Was the request definitely refused, so it is known nothing was done? That is what the table in
 * docs/web-api.md ("Error codes", column "Nothing changed?") says for the code the server sent.
 * `upstream_unavailable` counts only on a decision on a draft (`'decision'`): for a task the task
 * may have been made. A dropped connection, a timeout (also a 408), a reply that cannot be read, a
 * 5xx with a code this app does not know, or `unknown_outcome` leaves it open whether the write
 * went through, and the app must not say either way.
 */
export function isDefiniteRefusal(e: unknown, what: 'decision' | 'other' = 'other'): e is ApiError {
  if (!(e instanceof ApiError) || e.status === 408) return false;
  if (isServerErrorCode(e.code)) {
    const nothing = SERVER_ERROR_CODES[e.code];
    return nothing === 'yes' || (nothing === 'decision' && what === 'decision');
  }
  // A code the app does not know: only a 4xx the server meant is a refusal.
  return e.status >= 400 && e.status < 500;
}

/**
 * The plain sentence for an error. For a code in the contract it is the app's own wording (which
 * claims "nothing changed" only where the table does); for anything else, the server's sentence.
 */
export function errorText(e: unknown, fallback: string): string {
  if (e instanceof ApiError) {
    if (isServerErrorCode(e.code)) return copy.errors.codes[e.code];
    if (e.message) return e.message;
  } else if (e instanceof Error && e.message) {
    return e.message;
  }
  return fallback;
}
