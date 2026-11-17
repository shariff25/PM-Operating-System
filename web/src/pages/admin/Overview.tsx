import { useCallback } from 'react';
import { canSee, sectionHref } from '../../admin/access';
import type { AdminSection } from '../../admin/access';
import { NO_FILTERS, auditHash, dayOf } from '../../admin/filters';
import { auditTime, shortDay } from '../../admin/format';
import { useLoad } from '../../admin/useLoad';
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
      {data && (
        <>
          <section className="stack stack-tight" aria-labelledby="admin-week">
            <h2 id="admin-week" className="micro-label admin-window">
              {t.sinceLine(shortDay(data.since))}
            </h2>
            <div className="stats" role="group" aria-label={t.statsLabel}>
              <AdminStat lead label={t.stats.pending} value={data.pending_drafts} sub={t.subs.pending} />
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
