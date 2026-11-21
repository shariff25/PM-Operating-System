import { ApiError } from '../api';
import type { DirectoryUser } from '../api';
import { copy } from '../copy';
import { errorText, isDefiniteRefusal } from '../errors';

const T = copy.admin.team;
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
const two = (n: number) => String(n).padStart(2, '0');

/** What to call someone: their display name, or their handle when they have none. Plain text, always. */
export function nameOf(u: Pick<DirectoryUser, 'handle' | 'display_name'>): string {
  const n = u.display_name.trim();
  return n === '' ? u.handle : n;
}

/** "19 Nov, 14:05 UTC". An unreadable value is returned as it came. */
export function whenUtc(at: string): string {
  const t = new Date(at);
  if (Number.isNaN(t.getTime())) return at;
  return `${t.getUTCDate()} ${MONTHS[t.getUTCMonth()]}, ${two(t.getUTCHours())}:${two(t.getUTCMinutes())} UTC`;
}

/** "31 Dec 2026" for a `YYYY-MM-DD` day. */
export function longDay(day: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(day);
  if (!m) return day;
  return `${Number(m[3])} ${MONTHS[Number(m[2]) - 1] ?? m[2]} ${m[1]}`;
}

/** "Product Manager and Reviewer", "Admin, Product Manager and Auditor". */
export function roleWords(roles: readonly string[]): string {
  const words = roles.map((r) => T.roleLabels[r] ?? r);
  if (words.length <= 1) return words[0] ?? '';
  return `${words.slice(0, -1).join(', ')} and ${words[words.length - 1]}`;
}

/** "ana, ben and dana". */
export function listWords(items: readonly string[]): string {
  if (items.length <= 1) return items[0] ?? '';
  return `${items.slice(0, -1).join(', ')} and ${items[items.length - 1]}`;
}

/** Someone an admin should look at: a sign-in to confirm, a link waiting for the host, or Admin and Auditor together. */
export function needsAttention(u: DirectoryUser): boolean {
  return u.status === 'active' && (u.sign_in_unconfirmed || u.entra_needs_host || u.dual_role || u.role_conflict);
}

export type TeamFilter = 'all' | 'attention' | 'deactivated';
export const TEAM_FILTERS: readonly TeamFilter[] = ['all', 'attention', 'deactivated'];

export function filterUsers(users: readonly DirectoryUser[], f: TeamFilter): DirectoryUser[] {
  if (f === 'attention') return users.filter(needsAttention);
  if (f === 'deactivated') return users.filter((u) => u.status === 'deactivated');
  return [...users];
}

/** `?show=attention&person=sam` on `#/admin/team`. Anything unknown reads as the default. */
export function teamQuery(search: string): { show: TeamFilter; person: string | null } {
  const q = new URLSearchParams(search);
  const show = q.get('show');
  const person = q.get('person');
  return {
    show: show === 'attention' || show === 'deactivated' ? show : 'all',
    person: person && /^[a-z0-9][a-z0-9._-]{0,63}$/.test(person) ? person : null,
  };
}

export const teamHref = (o: { show?: TeamFilter; person?: string } = {}) => {
  const q = new URLSearchParams();
  if (o.show && o.show !== 'all') q.set('show', o.show);
  if (o.person) q.set('person', o.person);
  const s = q.toString();
  return `#/admin/team${s ? `?${s}` : ''}`;
};

/** Is this deactivated person still inside the grace period? */
export function inGrace(u: DirectoryUser, now = Date.now()): boolean {
  return u.grace_ends_at !== null && Date.parse(u.grace_ends_at) > now;
}

export type WriteOutcome =
  /** Refused, and it is known nothing changed: show the sentence. */
  | { kind: 'refused'; text: string; code: string }
  /** The team moved on (`stale`), so the list must be read again before another try. */
  | { kind: 'stale' }
  /** It may or may not have happened: read the list again, and never say either way. */
  | { kind: 'unknown' };

/** What a failed write means for the Team screen. Every code in the contract has its own sentence. */
export function writeOutcome(e: unknown): WriteOutcome {
  if (e instanceof ApiError && e.code === 'stale') return { kind: 'stale' };
  // On these routes `upstream_unavailable` means the tool server was never reached or refused: nothing changed (the
  // contract's own Errors table). A lost answer is `unknown_outcome`.
  const reachedNothing = e instanceof ApiError && e.code === 'upstream_unavailable';
  if (!reachedNothing && !isDefiniteRefusal(e)) return { kind: 'unknown' };
  if (!(e instanceof ApiError)) return { kind: 'unknown' };
  const code = e.code;
  const text = Object.hasOwn(T.errors, code) ? T.errors[code]! : errorText(e, T.failed);
  return { kind: 'refused', text, code };
}

/**
 * A Slack member id as the server takes it (docs/web-api.md, `NewUser`: "U… or W…"): U or W,
 * then capital letters and digits, 3 to 32 characters in all, as the contract crate checks.
 */
export function validSlackId(id: string): boolean {
  return /^[UW][A-Z0-9]{2,31}$/.test(id);
}
