import { AUDIT_OUTCOMES } from '../api/types';
import type { AuditQuery } from '../api/types';

/**
 * What the audit filter bar holds, as plain strings. Dates are calendar days in UTC
 * (`2026-11-16`), because the log is kept in UTC. An empty string means "no filter".
 */
export interface AuditFilters {
  actor: string;
  action: string;
  outcome: string;
  /** A marketplace bot's key (never `nudge`: Nudge's events carry none). */
  persona: string;
  from: string;
  to: string;
}

export const NO_FILTERS: AuditFilters = { actor: '', action: '', outcome: '', persona: '', from: '', to: '' };

const FILTER_KEYS = ['actor', 'action', 'outcome', 'persona', 'from', 'to'] as const;

const NAME = /^[A-Za-z0-9._-]+$/;
/** A bot's key, as the catalog's lint allows it. */
const BOT_KEY = /^[a-z0-9]+(-[a-z0-9]+)*$/;

/** A calendar day as `YYYY-MM-DD` that really exists. */
export function isDay(v: string): boolean {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(v)) return false;
  const t = Date.parse(`${v}T00:00:00Z`);
  return !Number.isNaN(t) && new Date(t).toISOString().startsWith(v);
}

/**
 * Read the filters out of a hash query string (`actor=ana&outcome=denied`). Anything malformed
 * is dropped rather than sent: the server answers 400 to it, and a bookmark should still open.
 */
export function parseFilters(search: string): AuditFilters {
  const params = new URLSearchParams(search.replace(/^\?/, ''));
  const out = { ...NO_FILTERS };
  const actor = (params.get('actor') ?? '').trim();
  if (actor.length <= 64 && NAME.test(actor)) out.actor = actor;
  const action = (params.get('action') ?? '').trim();
  if (action.length <= 100 && NAME.test(action)) out.action = action;
  const outcome = params.get('outcome') ?? '';
  if ((AUDIT_OUTCOMES as readonly string[]).includes(outcome)) out.outcome = outcome;
  const persona = params.get('persona') ?? '';
  if (persona.length <= 32 && BOT_KEY.test(persona) && persona !== 'nudge') out.persona = persona;
  const from = params.get('from') ?? '';
  if (isDay(from)) out.from = from;
  const to = params.get('to') ?? '';
  if (isDay(to)) out.to = to;
  return out;
}

/** The hash query string for a set of filters, in a fixed order, without the `?`. Empty when none. */
export function filtersToSearch(f: AuditFilters): string {
  const p = new URLSearchParams();
  for (const k of FILTER_KEYS) if (f[k]) p.set(k, f[k]);
  return p.toString();
}

export function auditHash(f: AuditFilters): string {
  const s = filtersToSearch(f);
  return s ? `/admin/audit?${s}` : '/admin/audit';
}

export const countFilters = (f: AuditFilters) => FILTER_KEYS.filter((k) => f[k] !== '').length;

/** The first day is after the last day: nothing could match, and the server would answer 400. */
export const rangeBackwards = (f: AuditFilters) => f.from !== '' && f.to !== '' && f.from > f.to;

const DAY_MS = 86_400_000;

/**
 * The query the server takes. `from` is the start of the first day; `to` is the start of the day
 * after the last one (the server's `to` is exclusive), so a single day is a whole day.
 */
export function toQuery(f: AuditFilters): AuditQuery {
  const q: AuditQuery = {};
  if (f.actor) q.actor = f.actor;
  if (f.action) q.action = f.action;
  if (f.outcome) q.outcome = f.outcome;
  if (f.persona) q.persona = f.persona;
  if (f.from) q.from = `${f.from}T00:00:00.000Z`;
  if (f.to) q.to = new Date(Date.parse(`${f.to}T00:00:00Z`) + DAY_MS).toISOString();
  return q;
}

/** The part after `?` in a hash path, or ''. */
export function searchOf(path: string): string {
  const i = path.indexOf('?');
  return i < 0 ? '' : path.slice(i + 1);
}

/** The calendar day (UTC) of a timestamp, as `YYYY-MM-DD`. */
export const dayOf = (iso: string) => iso.slice(0, 10);
