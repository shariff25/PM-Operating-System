import { Fragment, useCallback, useEffect, useId, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';
import { canExport } from '../../admin/access';
import { saveBlob } from '../../admin/download';
import { AUDIT_OUTCOMES } from '../../api/types';
import type { AuditEvent, SandboxCols } from '../../api/types';
import { NO_FILTERS, auditHash, countFilters, parseFilters, rangeBackwards, toQuery } from '../../admin/filters';
import type { AuditFilters } from '../../admin/filters';
import { auditTime } from '../../admin/format';
import { useAuditEvents } from '../../admin/useAuditEvents';
import { useLoad } from '../../admin/useLoad';
import { messageOf, useApp } from '../../app-context';
import { Empty, ErrorNote, Skeleton, SystemTag } from '../../components/bits';
import { Icon } from '../../components/icons';
import { copy } from '../../copy';
import { AdminHead, NoAccess, OutcomePill } from './parts';

const A = copy.admin.audit;

/** OpenShell's sandbox decisions (spec §B.3), offered as the action filter's suggestions. */
const SANDBOX_ACTIONS = [
  'sandbox.deny',
  'sandbox.deny_summary',
  'sandbox.allow_summary',
  'sandbox.finding',
  'sandbox.policy_loaded',
  'sandbox.source_gap',
  'sandbox.collector_gap',
  'sandbox.collector_down',
  'sandbox.unparsed',
];

/** A value, or the words for "the log has none". */
function Value({ v, mono }: { v: string | number | null; mono?: boolean }) {
  if (v === null || v === '') return <span className="not-recorded">{copy.admin.notRecorded}</span>;
  return <span className={mono ? 'mono-val' : undefined}>{v}</span>;
}

function Field({ label, children, wide }: { label: string; children: ReactNode; wide?: boolean }) {
  return (
    <div className={`kv ${wide ? 'kv-wide' : ''}`}>
      <dt>{label}</dt>
      <dd>{children}</dd>
    </div>
  );
}

/** Every field the API returns for one event, nulls spelled out. */
function EventDetails({ e }: { e: AuditEvent }) {
  const f = A.detailFields;
  return (
    <div className="audit-detail" role="group" aria-label={A.detailsLabel}>
      <dl className="kv-grid">
        <Field label={f.id}><Value v={e.id} mono /></Field>
        <Field label={f.at}><Value v={e.at} mono /></Field>
        <Field label={f.actor}><Value v={e.actor} mono /></Field>
        <Field label={f.client}>
          <Value v={e.client} mono />
          {A.clients[e.client] && <span className="kv-note">{A.clients[e.client]}</span>}
        </Field>
        <Field label={f.action}><Value v={e.action} mono /></Field>
        <Field label={f.system}><Value v={e.system} mono /></Field>
        <Field label={f.outcome}><OutcomePill outcome={e.outcome} /></Field>
        <Field label={f.skill}><Value v={e.skill} mono /></Field>
        <Field label={f.bot}><Value v={e.bot} mono /></Field>
        <Field label={f.persona}>
          {e.persona ? <span className="mono-val">{e.persona}</span> : <span className="not-recorded">{A.personaNone}</span>}
        </Field>
        <Field label={f.task}><Value v={e.task_id} mono /></Field>
        <Field label={f.sources}><Value v={e.sources} mono /></Field>
        <Field label={f.titledSources}>
          <Value v={e.titled_sources} mono />
          <span className="kv-note">{A.titlesNote}</span>
        </Field>
        <Field label={f.target} wide><Value v={e.target} mono /></Field>
        <Field label={f.detail} wide><Value v={e.detail} /></Field>
      </dl>
      {e.sandbox && <SandboxDetails s={e.sandbox} />}
    </div>
  );
}

/**
 * A sandbox decision's own columns (spec §B.3), as plain text: the tool server has already left
 * out the host, address, path and process for an auditor, who sees the destination class.
 */
export function SandboxDetails({ s }: { s: SandboxCols }) {
  const f = A.sandboxFields;
  const dest = s.dest_host || s.dest_ip ? `${s.dest_host ?? s.dest_ip}${s.dest_port != null ? `:${s.dest_port}` : ''}` : null;
  const request = s.http_method || s.http_path ? `${s.http_method ?? ''} ${s.http_path ?? ''}`.trim() : null;
  return (
    <dl className="kv-grid" aria-label={f.label}>
      <Field label={f.sandbox}><Value v={s.sandbox ?? null} mono /></Field>
      <Field label={f.decision}><Value v={s.decision ?? null} mono /></Field>
      <Field label={f.rule}><Value v={s.rule ?? null} mono /></Field>
      <Field label={f.reason}><Value v={s.reason ?? null} mono /></Field>
      <Field label={f.destClass}><Value v={s.dest_class ?? null} /></Field>
      {dest !== null && <Field label={f.destination}><Value v={dest} mono /></Field>}
      {dest === null && s.dest_port != null && <Field label={f.destination}><Value v={`:${s.dest_port}`} mono /></Field>}
      {request !== null && <Field label={f.request} wide><Value v={request} mono /></Field>}
      {s.process && <Field label={f.process}><Value v={s.process} mono /></Field>}
      {s.allow_count != null && (
        <Field label={f.allowCount}>
          <Value v={s.allow_count} mono />
          <span className="kv-note">{f.allowNote}</span>
        </Field>
      )}
    </dl>
  );
}

function AuditRow({ e, open, onToggle }: { e: AuditEvent; open: boolean; onToggle: () => void }) {
  const time = auditTime(e.at);
  const panel = `ev-${e.id}`;
  return (
    <Fragment>
      <tr className={`audit-row ${open ? 'audit-row-open' : ''}`}>
        <td className="c-toggle">
          <button
            type="button"
            className="row-toggle"
            aria-expanded={open}
            aria-controls={panel}
            aria-label={open ? A.hideDetails(e.action, time) : A.showDetails(e.action, time)}
            onClick={onToggle}
          >
            <Icon name={open ? 'chevronDown' : 'chevronRight'} size={15} />
          </button>
        </td>
        <td className="c-time"><time dateTime={e.at}>{time}</time></td>
        <td className="c-who">{e.actor}</td>
        <td className="c-client"><span className="mono-val">{e.client}</span></td>
        <td className="c-action"><span className="mono-val action-name" title={e.action}>{e.action}</span></td>
        <td className="c-system"><SystemTag system={e.system} label={e.system} /></td>
        <td className="c-target">
          {e.target ? <span className="mono-val target-text" title={e.target}>{e.target}</span> : <span className="tcell-none" aria-hidden="true">–</span>}
          {!e.target && <span className="sr-only">{copy.admin.notRecorded}</span>}
        </td>
        <td className="c-outcome"><OutcomePill outcome={e.outcome} /></td>
      </tr>
      {open && (
        <tr className="audit-detail-row" id={panel}>
          <td colSpan={8}>
            <EventDetails e={e} />
          </td>
        </tr>
      )}
    </Fragment>
  );
}

/**
 * The bots the filter offers: the catalog's, less Nudge (its events carry no bot key), and any key
 * the address names that the catalog no longer has. Falls back to the address alone when the
 * catalog can't be read.
 */
function useBotChoices(current: string): { key: string; label: string }[] {
  const { api } = useApp();
  const load = useCallback(() => api.adminBots(), [api]);
  const { data } = useLoad(load, A.loadFailed);
  const out: { key: string; label: string }[] = [];
  for (const b of data?.bots ?? []) if (!b.always_on && b.key !== 'nudge') out.push({ key: b.key, label: b.display_name });
  for (const u of data?.unknown ?? []) if (!out.some((o) => o.key === u.key)) out.push({ key: u.key, label: A.fields.personaGone(u.key) });
  if (current && !out.some((o) => o.key === current)) out.push({ key: current, label: current });
  return out;
}

function FilterBar({ filters, onApply }: { filters: AuditFilters; onApply: (f: AuditFilters) => void }) {
  const id = useId();
  const bots = useBotChoices(filters.persona);
  const [draft, setDraft] = useState<AuditFilters>(filters);
  // When the address changes (back, a bookmark, Clear), the boxes follow it.
  const shown = JSON.stringify(filters);
  useEffect(() => setDraft(filters), [shown]); // eslint-disable-line react-hooks/exhaustive-deps
  const set = (k: keyof AuditFilters, v: string) => setDraft((d) => ({ ...d, [k]: v }));
  const active = countFilters(filters) > 0;
  const backwards = rangeBackwards(draft);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    onApply({ ...draft, actor: draft.actor.trim(), action: draft.action.trim() });
  };
  // Choosing an outcome, a bot or a day applies at once; the two text boxes apply on Enter or Apply.
  const pick = (k: 'outcome' | 'persona' | 'from' | 'to', v: string) => {
    const next = { ...draft, [k]: v };
    setDraft(next);
    if (!rangeBackwards(next)) onApply({ ...next, actor: next.actor.trim(), action: next.action.trim() });
  };
  return (
    <form className="filter-bar" role="search" aria-label={A.filtersLabel} onSubmit={submit}>
      <div className="filter-field filter-actor">
        <label className="field-label" htmlFor={`${id}-actor`}>{A.fields.actor}</label>
        <input id={`${id}-actor`} type="text" value={draft.actor} placeholder="ana" autoComplete="off" spellCheck={false} maxLength={64} onChange={(e) => set('actor', e.target.value)} />
      </div>
      <div className="filter-field filter-action">
        <label className="field-label" htmlFor={`${id}-action`}>{A.fields.action}</label>
        <input id={`${id}-action`} type="text" list={`${id}-actions`} value={draft.action} placeholder="jira.create_issue" autoComplete="off" spellCheck={false} maxLength={100} onChange={(e) => set('action', e.target.value)} />
        <datalist id={`${id}-actions`}>
          {SANDBOX_ACTIONS.map((a) => <option key={a} value={a} />)}
        </datalist>
      </div>
      <div className="filter-field filter-outcome">
        <label className="field-label" htmlFor={`${id}-outcome`}>{A.fields.outcome}</label>
        <select id={`${id}-outcome`} value={draft.outcome} onChange={(e) => pick('outcome', e.target.value)}>
          <option value="">{A.fields.anyOutcome}</option>
          {AUDIT_OUTCOMES.map((o) => (
            <option key={o} value={o}>{A.outcomes[o]}</option>
          ))}
        </select>
      </div>
      <div className="filter-field filter-persona">
        <label className="field-label" htmlFor={`${id}-persona`}>{A.fields.persona}</label>
        <select id={`${id}-persona`} value={draft.persona} onChange={(e) => pick('persona', e.target.value)}>
          <option value="">{A.fields.anyPersona}</option>
          {bots.map((b) => (
            <option key={b.key} value={b.key}>{b.label}</option>
          ))}
        </select>
      </div>
      <div className="filter-field filter-day">
        <label className="field-label" htmlFor={`${id}-from`}>{A.fields.from}</label>
        <input id={`${id}-from`} type="date" value={draft.from} aria-describedby={`${id}-days`} onChange={(e) => pick('from', e.target.value)} />
      </div>
      <div className="filter-field filter-day">
        <label className="field-label" htmlFor={`${id}-to`}>{A.fields.to}</label>
        <input id={`${id}-to`} type="date" value={draft.to} aria-describedby={`${id}-days`} onChange={(e) => pick('to', e.target.value)} />
      </div>
      <div className="filter-actions">
        <button type="submit" className="btn btn-primary">{A.apply}</button>
        {active && (
          <button type="button" className="btn btn-ghost" onClick={() => onApply(NO_FILTERS)}>{A.clear}</button>
        )}
      </div>
      <p id={`${id}-days`} className="filter-hint">
        {backwards ? <span className="filter-error" role="alert">{A.backwards}</span> : A.fields.daysHint}
      </p>
    </form>
  );
}

/** The audit log: filters in the address, a table of events, "Load more", and an export. */
export function AuditLog({ search }: { search: string }) {
  const { api, me, navigate } = useApp();
  const filters = parseFilters(search);
  const log = useAuditEvents(api, filters);
  const [exporting, setExporting] = useState(false);
  const [exported, setExported] = useState<{ rowCount: number; filename: string; truncated: boolean } | null>(null);
  const [exportError, setExportError] = useState<string | null>(null);
  const [openIds, setOpenIds] = useState<ReadonlySet<string>>(new Set());

  if (log.forbidden) return <NoAccess />;

  const filtered = countFilters(filters) > 0;
  const apply = (f: AuditFilters) => {
    setExported(null);
    setExportError(null);
    setOpenIds(new Set());
    navigate(auditHash(f));
  };
  const toggle = (id: string) =>
    setOpenIds((s) => {
      const next = new Set(s);
      if (!next.delete(id)) next.add(id);
      return next;
    });

  const runExport = async () => {
    setExporting(true);
    setExported(null);
    setExportError(null);
    try {
      const out = await api.adminAuditExport(toQuery(filters));
      saveBlob(out.blob, out.filename);
      setExported({ rowCount: out.rowCount, filename: out.filename, truncated: out.truncated });
    } catch (e) {
      setExportError(messageOf(e, A.exportFailed));
    } finally {
      setExporting(false);
    }
  };

  const events = log.events;
  return (
    <div className="page page-wide">
      <AdminHead section="audit" title={copy.admin.sections.audit} lede={A.lede} />
      <p className="meta-line">
        <Icon name="shield" size={15} />
        <span>{A.metadataOnly}</span>
      </p>
      <section className="card card-table" aria-label={A.eventsHeading}>
        <FilterBar filters={filters} onApply={apply} />
        <div className="card-head card-head-sub">
          <h2>
            {A.eventsHeading}
            {events && <span className="card-count" aria-live="polite">{A.count(events.length, log.hasMore)}</span>}
          </h2>
          {canExport(me) && (
            <button type="button" className="btn btn-sm" disabled={exporting || log.backwards} onClick={() => void runExport()}>
              <Icon name="download" size={15} />
              {exporting ? A.exporting : A.export}
            </button>
          )}
        </div>
        {exported && (
          <div className="export-note" role="status">
            <div className={`note ${exported.truncated ? 'note-warn' : 'note-good'}`}>
              <Icon name={exported.truncated ? 'alert' : 'check'} className="note-icon" />
              <div className="note-body">
                <p>{A.exportedLine(exported.rowCount, exported.filename)}</p>
                {exported.truncated && <p>{A.exportTruncated(exported.rowCount)}</p>}
              </div>
            </div>
          </div>
        )}
        {exportError && (
          <div className="export-note">
            <ErrorNote>{exportError}</ErrorNote>
          </div>
        )}
        {log.error && (
          <div className="export-note">
            <ErrorNote actionLabel={copy.errors.retry} onAction={log.retry}>{log.error}</ErrorNote>
          </div>
        )}
        {!events && !log.error && !log.backwards && <div className="card-body"><Skeleton rows={6} label={A.loading} /></div>}
        {log.backwards && (
          <Empty icon="clock" small>{A.backwards}</Empty>
        )}
        {events && events.length === 0 && (
          <Empty
            icon="activity"
            action={filtered ? <button type="button" className="btn btn-sm" onClick={() => apply(NO_FILTERS)}>{A.clear}</button> : undefined}
          >
            {filtered ? A.emptyFiltered : A.empty}
          </Empty>
        )}
        {events && events.length > 0 && (
          <div className="table-scroll">
            <table className="dtable audit-table" aria-label={A.tableLabel}>
              <colgroup>
                <col className="col-toggle" />
                <col className="col-time" />
                <col className="col-who" />
                <col className="col-client" />
                <col className="col-action" />
                <col className="col-system" />
                <col className="col-target" />
                <col className="col-outcome" />
              </colgroup>
              <thead>
                <tr>
                  <th scope="col" className="c-toggle"><span className="sr-only">{A.columns.details}</span></th>
                  <th scope="col" className="c-time">{A.columns.time}</th>
                  <th scope="col" className="c-who">{A.columns.who}</th>
                  <th scope="col" className="c-client">{A.columns.client}</th>
                  <th scope="col" className="c-action">{A.columns.action}</th>
                  <th scope="col" className="c-system">{A.columns.system}</th>
                  <th scope="col" className="c-target">{A.columns.target}</th>
                  <th scope="col" className="c-outcome">{A.columns.outcome}</th>
                </tr>
              </thead>
              <tbody>
                {events.map((e) => (
                  <AuditRow key={e.id} e={e} open={openIds.has(e.id)} onToggle={() => toggle(e.id)} />
                ))}
              </tbody>
            </table>
          </div>
        )}
        {events && events.length > 0 && (
          <div className="load-more">
            {log.moreError && <ErrorNote>{log.moreError}</ErrorNote>}
            {log.hasMore ? (
              <button type="button" className="btn" disabled={log.loadingMore} onClick={log.loadMore}>
                {log.loadingMore ? A.loadingMore : A.loadMore}
              </button>
            ) : (
              <p className="muted small">{A.endLine}</p>
            )}
          </div>
        )}
      </section>
    </div>
  );
}
