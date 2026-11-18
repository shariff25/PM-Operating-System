import type { AdminSection } from './admin/access';

/** Only http and https addresses are ever turned into links. Anything else is shown as text. */
export function safeUrl(value: string | null | undefined): string | null {
  if (!value) return null;
  try {
    const u = new URL(value.trim());
    return u.protocol === 'https:' || u.protocol === 'http:' ? u.href : null;
  } catch {
    return null;
  }
}

/** A draft or task id is safe to put in a path or a hash only when it is a plain token. */
export function isSafeId(id: string): boolean {
  return /^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$/.test(id);
}

/**
 * Read the id out of a `/d/<id>` hash path. `/drafts/<id>` means the same: older Slack messages
 * link to it. Returns null when it is malformed or unsafe.
 */
export function draftIdFromPath(path: string): string | null {
  const raw = /^\/(?:d|drafts)\/([^/]+)$/.exec(path)?.[1];
  if (!raw) return null;
  try {
    const id = decodeURIComponent(raw);
    return isSafeId(id) ? id : null;
  } catch {
    return null;
  }
}

/** Read the id out of a `/a/<task id>` hash path. Returns null when it is malformed or unsafe. */
export function taskIdFromPath(path: string): string | null {
  const raw = /^\/a\/([^/]+)$/.exec(path)?.[1];
  if (!raw) return null;
  try {
    const id = decodeURIComponent(raw);
    return isSafeId(id) ? id : null;
  } catch {
    return null;
  }
}

/** Read the id out of a `/reviews/<id>` hash path. Returns null when it is malformed or unsafe. */
export function reviewIdFromPath(path: string): string | null {
  const raw = /^\/reviews\/([^/]+)$/.exec(path)?.[1];
  if (!raw) return null;
  try {
    const id = decodeURIComponent(raw);
    return isSafeId(id) ? id : null;
  } catch {
    return null;
  }
}

export type Route =
  | { name: 'home' }
  | { name: 'drafts' }
  | { name: 'draft'; id: string }
  | { name: 'followups' }
  | { name: 'activity' }
  | { name: 'task'; id: string }
  | { name: 'reviews' }
  | { name: 'review'; id: string }
  /** `search` is the query part of the hash (the audit filters), without the `?`. */
  | { name: 'admin'; section: AdminSection; search: string; sub?: PolicyPart };

/** The parts of the policy page: `#/admin/policy`, `#/admin/policy/propose`, `#/admin/policy/proposals`. */
export type PolicyPart = 'propose' | 'proposals';

/** `/admin`, `/admin/audit`, `/admin/team`, `/admin/connectors`, each with an optional `?query`. */
function adminRoute(path: string): Route | null {
  const part = /^\/admin\/policy\/(propose|proposals)\/?(?:\?(.*))?$/.exec(path);
  if (part) return { name: 'admin', section: 'policy', search: part[2] ?? '', sub: part[1] as PolicyPart };
  const m = /^\/admin(?:\/(audit|team|connectors|policy|content))?\/?(?:\?(.*))?$/.exec(path);
  if (!m) return null;
  return { name: 'admin', section: (m[1] as AdminSection | undefined) ?? 'overview', search: m[2] ?? '' };
}

/** Every hash the app understands. Anything unknown, malformed or unsafe is the home page. */
export function parseRoute(path: string): Route {
  if (path === '/drafts') return { name: 'drafts' };
  if (path === '/followups') return { name: 'followups' };
  if (path === '/activity') return { name: 'activity' };
  if (path === '/reviews') return { name: 'reviews' };
  const review = reviewIdFromPath(path);
  if (review) return { name: 'review', id: review };
  const admin = adminRoute(path);
  if (admin) return admin;
  const draft = draftIdFromPath(path);
  if (draft) return { name: 'draft', id: draft };
  const task = taskIdFromPath(path);
  if (task) return { name: 'task', id: task };
  return { name: 'home' };
}

export const draftHref = (id: string) => `#/d/${encodeURIComponent(id)}`;
export const taskHref = (id: string) => `#/a/${encodeURIComponent(id)}`;
export const reviewHref = (id: string) => `#/reviews/${encodeURIComponent(id)}`;

/** `/signed-out?reason=…` is where the server sends someone whose sign-in did not finish. */
export function signedOutReason(path: string): string | null {
  const m = /^\/signed-out(?:\?(.*))?$/.exec(path);
  if (!m) return null;
  return new URLSearchParams(m[1] ?? '').get('reason') ?? '';
}

/** The sign-in link for the page the PM was trying to reach, or plain `/api/auth/login`. */
export function loginHref(path: string): string {
  const base = '/api/auth/login';
  if (path === '/' || path === '' || signedOutReason(path) !== null || !path.startsWith('/')) return base;
  return `${base}?return_to=${encodeURIComponent(`/#${path}`)}`;
}
