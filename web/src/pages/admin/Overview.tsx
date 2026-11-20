import { useCallback } from 'react';
import { canSee, sectionHref } from '../../admin/access';
import type { AdminSection } from '../../admin/access';
import { NO_FILTERS, auditHash, dayOf } from '../../admin/filters';
import { auditTime, shortDay } from '../../admin/format';
import { listWords, longDay, teamHref } from '../../admin/team';
import { useLoad } from '../../admin/useLoad';
import type { AdminOverview as AdminOverviewData, OverviewUsers } from '../../api';
import { useApp } from '../../app-context';
import { Empty, ErrorNote, Skeleton, SystemTag } from '../../components/bits';
import { Icon } from '../../components/icons';
import type { IconName } from '../../components/icons';
import { copy } from '../../copy';
import { AdminHead, NoAccess } from './parts';

const R = copy.admin.overview.recent;

function AdminStat({ label, value, sub, lead, warn, href }: { label: string; value: number | null; sub: string; lead?: boolean; warn?: boolean; href?: string }) {
  const body = (
    <>
      <span className="stat-label">{label}</span>
      <span className="stat-value">{value ?? '–'}</span>
      <span className={`stat-sub ${warn ? 'stat-sub-warn' : ''}`}>{sub}</span>
    </>
  );
  const cls = `stat ${lead ? 'stat-lead' : ''}`;
  return href ? (
    <a className={`${cls} stat-link`} href={href}>
      {body}
    </a>
  ) : (
    <div className={cls}>{body}</div>
  );
}

const CARDS: { section: Exclude<AdminSection, 'overview'>; icon: IconName }[] = [
  { section: 'audit', icon: 'activity' },
  { section: 'team', icon: 'followups' },
  { section: 'connectors', icon: 'globe' },
  { section: 'policy', icon: 'shield' },
  { section: 'content', icon: 'layers' },
];

/**
 * The five newest events with one outcome, straight from the audit log, and a way into the log
 * on that outcome. Metadata only, as everywhere in the log. A refusal (403) draws nothing.
 */
function RecentEvents({ outcome, title, empty }: { outcome: 'denied' | 'approved'; title: string; empty: string }) {
  const { api } = useApp();
  const load = useCallback(() => api.adminAudit({ outcome, limit: 5 }), [api, outcome]);
  const { data, error, forbidden, reload } = useLoad(load, R.failed);
  if (forbidden) return null;
  const id = `recent-${outcome}`;
  return (
    <section className="card" aria-labelledby={id}>
      <div className="card-head">
        <h2 id={id}>{title}</h2>
        <a className="card-link" href={`#${auditHash({ ...NO_FILTERS, outcome })}`}>
          {R.seeAll}
          <span className="sr-only"> {title.toLowerCase()}</span>
          <Icon name="chevronRight" size={14} />
        </a>
      </div>
      {error && (
        <div className="card-body">
          <ErrorNote actionLabel={copy.errors.retry} onAction={reload}>
            {error}
          </ErrorNote>
        </div>
      )}
      {!data && !error && <div className="card-body"><Skeleton rows={3} label={copy.admin.loading} /></div>}
      {data && data.events.length === 0 && <Empty icon={outcome === 'denied' ? 'shield' : 'check'} small>{empty}</Empty>}
      {data && data.events.length > 0 && (
        <ul className="rows" aria-label={title}>
          {data.events.map((e) => (
            <li key={e.id} className="row recent-row">
              <SystemTag system={e.system} label={e.system} />
              <span className="row-main">
                <span className="row-title mono-val" title={e.action}>{e.action}</span>
                <span className="row-meta">
                  {e.actor}
                  <span aria-hidden="true"> · </span>
                  <span className="sr-only">, </span>
                  {copy.admin.audit.clients[e.client] ?? e.client}
                </span>
              </span>
              <time className="row-when" dateTime={e.at}>{auditTime(e.at)}</time>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** What the audit log keeps, and when it was last cleaned. Any field may be missing from an older server. */
function Retention({ data }: { data: AdminOverviewData }) {
  const r = copy.admin.overview.retention;
  const days = data.retention_days ?? null;
  const oldest = data.oldest_event_at ?? null;
  const purge = data.last_purge ?? null;
  return (
    <section className="card" aria-labelledby="admin-retention">
      <div className="card-head">
        <h2 id="admin-retention">{r.title}</h2>
      </div>
      <ul className="rows retention-rows">
        <li className="row">
          <Icon name="clock" size={16} />
          <span className="row-main"><span className="row-title">{days !== null ? r.kept(days) : r.keptUnknown}</span></span>
        </li>
        <li className="row">
          <Icon name="activity" size={16} />
          <span className="row-main">
            <span className="row-meta">{oldest ? r.oldest(shortDay(oldest)) : r.oldestNone}</span>
          </span>
        </li>
        <li className="row">
          <Icon name="check" size={16} />
          <span className="row-main">
            <span className="row-meta">{purge ? r.purge(shortDay(purge.at), purge.deleted) : r.purgeNone}</span>
          </span>
        </li>
      </ul>
    </section>
  );
}

const P = copy.admin.overview.people;
const N = copy.admin.overview.notices;

/** Handles as links to that person on the Team page (plain text for someone who can't open it). */
function Handles({ handles, label, linkable }: { handles: readonly string[]; label: string; linkable: boolean }) {
  return (
    <ul className="handle-links" aria-label={label}>
      {handles.map((h) => (
        <li key={h}>
          {linkable ? <a className="handle-link" href={teamHref({ person: h })}>{h}</a> : <span className="handle-link">{h}</span>}
        </li>
      ))}
    </ul>
  );
}

/**
 * The notices that can't wait: the pilot exception ending or ended, a service whose lookups look
 * like someone guessing names, and the deactivation cap. Each shows only when it applies.
 */
function UserNotices({ users, linkable }: { users: OverviewUsers; linkable: boolean }) {
  const pilot = users.pilot_exception;
  const lapsed = pilot?.lapsed && users.role_conflicts.length > 0;
  const ending = pilot && pilot.ending_soon && !pilot.lapsed;
  const miss = users.resolve_miss_warning;
  const capped = users.deactivation_limit_reached;
  if (!lapsed && !ending && miss.length === 0 && !capped) return null;
  return (
    <div className="overview-notices">
      {lapsed && pilot && (
        <div className="note note-error" role="alert">
          <Icon name="alert" className="note-icon" />
          <div className="note-body">
            <p>{N.lapsed(longDay(pilot.ends))}</p>
            <Handles handles={users.role_conflicts} label={copy.admin.team.roleConflict} linkable={linkable} />
            <p>{N.lapsedNext}</p>
          </div>
        </div>
      )}
      {ending && pilot && (
        <div className="note note-warn" role="status">
          <Icon name="clock" className="note-icon" />
          <div className="note-body"><p>{N.ending(longDay(pilot.ends))}</p></div>
        </div>
      )}
      {miss.length > 0 && (
        <div className="note note-warn" role="status">
          <Icon name="shield" className="note-icon" />
          <div className="note-body">
            <p>{N.resolveMiss(listWords(miss.map((s) => N.services[s] ?? s)), users.resolve_miss_warning_threshold)}</p>
            <p className="note-next">{N.resolveMissNext}</p>
          </div>
        </div>
      )}
      {capped && (
        <div className="note note-warn" role="status">
          <Icon name="alert" className="note-icon" />
          <div className="note-body"><p>{N.capReached(users.deactivation_limit)}</p></div>
        </div>
      )}
    </div>
  );
}

/** The user database's flags that need an admin, each a row with whose it is. Nothing to show draws nothing. */
function PeopleFlags({ users, linkable }: { users: OverviewUsers; linkable: boolean }) {
  const ends = users.pilot_exception ? longDay(users.pilot_exception.ends) : null;
  const conflictUsers = users.identity_conflict_users ?? [];
  const rows: { key: string; icon: IconName; title: string; next: string; handles?: readonly string[]; plain?: boolean }[] = [];
  if (users.sign_in_unconfirmed.length) rows.push({ key: 'unconfirmed', icon: 'alert', title: P.unconfirmed, next: P.unconfirmedNext, handles: users.sign_in_unconfirmed });
  if (users.identity_conflicts === null) rows.push({ key: 'conflicts', icon: 'info', title: P.conflictsUnknown, next: '' });
  else if (users.identity_conflicts > 0) rows.push({ key: 'conflicts', icon: 'shield', title: P.conflicts(users.identity_conflicts), next: P.conflictsNext, handles: conflictUsers });
  if (users.dual_role.length) {
    const ended = users.pilot_exception?.lapsed && ends;
    rows.push({ key: 'dual', icon: 'layers', title: ended ? P.dualRoleEnded(ends) : P.dualRole(ends), next: ended ? P.dualRoleEndedNext : P.dualRoleNext, handles: users.dual_role });
  }
  if (users.entra_needs_host.length) rows.push({ key: 'host', icon: 'lock', title: P.needsHost, next: P.needsHostNext, handles: users.entra_needs_host });
  if (users.no_slack.length) rows.push({ key: 'slack', icon: 'message', title: P.noSlack, next: P.noSlackNext, handles: users.no_slack });
  if (users.bots_without_active_owner.length) rows.push({ key: 'bots', icon: 'cpu', title: P.orphanBots, next: P.orphanBotsNext, handles: users.bots_without_active_owner, plain: true });
  const resets = users.password_resets ?? [];
  if (resets.length) rows.push({ key: 'resets', icon: 'shield', title: P.resets(resets.length), next: P.resetsNext, handles: [...new Set(resets.map((r) => r.handle))] });
  if (rows.length === 0) return null;
  return (
    <section className="card" aria-labelledby="admin-people">
      <div className="card-head">
        <h2 id="admin-people">
          {P.title}
          <span className="card-count">{P.counts(users.active, users.deactivated)}</span>
        </h2>
        {linkable && (
          <a className="card-link" href={teamHref({ show: 'attention' })}>
            {P.openTeam}
            <Icon name="chevronRight" size={14} />
          </a>
        )}
      </div>
      <ul className="rows flag-rows">
        {rows.map((r) => (
          <li key={r.key} className="row">
            <Icon name={r.icon} size={16} />
            <span className="row-main">
              <span className="row-title">{r.title}</span>
              {r.next && <span className="row-meta">{r.next}</span>}
              {r.handles && r.handles.length > 0 && <Handles handles={r.handles} label={P.listLabel(r.title)} linkable={linkable && !r.plain} />}
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}

/** The admin home: the week's counts, the latest denials and approvals, and a way into each part. */
export function AdminOverview() {
  const { api, me } = useApp();
  const load = useCallback(() => api.adminOverview(), [api]);
  const { data, error, forbidden, reload } = useLoad(load, copy.admin.overview.error);
  if (forbidden) return <NoAccess />;

  const t = copy.admin.overview;
  const missing = data !== null && [data.approved, data.rejected, data.denied].some((n) => n === null);
  const since = data ? dayOf(data.since) : '';
  // Each tile opens the audit log on that outcome, from the day the window began.
  const to = (outcome: string) => `#${auditHash({ ...NO_FILTERS, outcome, from: since })}`;
  const sub = (n: number | null, text: string) => (n === null ? t.subs.unavailable : text);

  return (
    <div className="page">
      <AdminHead section="overview" title={copy.admin.sections.overview} lede={t.lede} />
      {error && (
        <ErrorNote actionLabel={copy.errors.retry} onAction={reload}>
          {error}
        </ErrorNote>
      )}
      {!data && !error && <Skeleton rows={3} label={copy.admin.loading} variant="cards" />}
      {data?.users && <UserNotices users={data.users} linkable={canSee(me, 'team')} />}
      {data && (
        <>
          <section className="stack stack-tight" aria-labelledby="admin-week">
            <h2 id="admin-week" className="micro-label admin-window">
              {t.sinceLine(shortDay(data.since))}
            </h2>
            <div className="stats" role="group" aria-label={t.statsLabel}>
              <AdminStat lead label={t.stats.pending} value={data.pending_drafts} sub={data.held_drafts ? t.held(data.held_drafts) : t.subs.pending} />
              <AdminStat label={t.stats.approved} value={data.approved} sub={sub(data.approved, t.subs.approved)} href={canSee(me, 'audit') ? to('approved') : undefined} />
              <AdminStat label={t.stats.rejected} value={data.rejected} sub={sub(data.rejected, t.subs.rejected)} href={canSee(me, 'audit') ? to('rejected') : undefined} />
              <AdminStat
                label={t.stats.denied}
                value={data.denied}
                sub={data.denied === null ? t.subs.unavailable : t.subs.denied(data.denied)}
                warn={(data.denied ?? 0) > 0}
                href={canSee(me, 'audit') ? to('denied') : undefined}
              />
            </div>
            {missing && <p className="admin-hint">{t.partial}</p>}
          </section>
          {data.users && <PeopleFlags users={data.users} linkable={canSee(me, 'team')} />}
          <Retention data={data} />
          {canSee(me, 'audit') && (
            <div className="recent-grid">
              <RecentEvents outcome="denied" title={R.denied} empty={R.noDenied} />
              <RecentEvents outcome="approved" title={R.approved} empty={R.noApproved} />
            </div>
          )}
          <nav aria-label={t.linksLabel} className="link-cards">
            {CARDS.filter((c) => canSee(me, c.section)).map((c) => (
              <a key={c.section} className="card link-card" href={sectionHref(c.section)}>
                <span className="link-card-icon"><Icon name={c.icon} size={18} /></span>
                <span className="link-card-text">
                  <span className="link-card-title">{t.links[c.section].title}</span>
                  <span className="link-card-body">{t.links[c.section].body}</span>
                </span>
                <Icon name="chevronRight" size={16} className="link-card-go" />
              </a>
            ))}
          </nav>
        </>
      )}
    </div>
  );
}
