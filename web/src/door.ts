import type { Me } from './api';
import { safeUrl } from './url';

/**
 * PM OS has two front doors (slice H10, docs/openshell-any-host-spec.md §3.8): the browser-facing
 * one, for everyone, and the admin door, its own address reached over the VPN or an SSH tunnel,
 * where only the admin and audit screens are. `/api/me` says which door served the page.
 */

/** Was this page served by the admin door? Then only the admin and audit screens are reachable. */
export function onAdminDoor(me: Pick<Me, 'door'>): boolean {
  return me.door === 'admin';
}

/**
 * On the public door, for an Admin or an Auditor: the admin screens' address on the admin door
 * (an absolute http or https URL), or null when the server gave none. An older server that names
 * no door keeps the admin screens on this one.
 */
export function adminDoorHref(me: Pick<Me, 'door' | 'admin_url'>, hash = '#/admin'): string | null {
  if (me.door !== 'public' || !me.admin_url) return null;
  const base = safeUrl(me.admin_url);
  if (!base) return null;
  return `${base.replace(/\/$/, '')}/${hash}`;
}

/** Are the admin screens served on this page's own door? */
export function adminHere(me: Pick<Me, 'door'>): boolean {
  return me.door !== 'public';
}
