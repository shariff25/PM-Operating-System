import { useCallback } from 'react';
import type { Connector } from '../../api';
import { useApp } from '../../app-context';
import { Empty, ErrorNote, Skeleton } from '../../components/bits';
import { Icon, systemIcon } from '../../components/icons';
import { copy } from '../../copy';
import { auditTime, titleCase } from '../../admin/format';
import { useLoad } from '../../admin/useLoad';
import { relTime } from '../../time';
import { AdminHead, NoAccess } from './parts';

const C = copy.admin.connectors;

function HealthPill({ c }: { c: Connector }) {
  const ok = c.health === 'ok';
  const why = !ok && c.error_class ? (C.errorClasses[c.error_class] ?? c.error_class) : null;
  return (
    <span className="health">
      <span className={`pill ${ok ? 'pill-good' : 'pill-bad'}`}>
        <span className="pill-dot" aria-hidden="true" />
        {C.health[c.health] ?? c.health}
      </span>
      {why && <span className="health-why">{why}</span>}
    </span>
  );
}

/** Admins only: each connector, whether it runs on the real system or fixtures, and whether it answers. */
export function Connectors() {
  const { api } = useApp();
  const load = useCallback(() => api.adminConnectors(), [api]);
  const { data, error, forbidden, loading, reload } = useLoad(load, C.loadFailed);
  if (forbidden) return <NoAccess />;
  const now = Date.now();

  return (
    <div className="page page-wide">
      <AdminHead section="connectors" title={copy.admin.sections.connectors} lede={C.lede}>
        <button type="button" className="btn btn-keep" disabled={loading} onClick={reload}>
          <Icon name="refresh" size={15} />
          {loading && data ? C.rechecking : C.recheck}
        </button>
      </AdminHead>
      {error && (
        <ErrorNote actionLabel={copy.errors.retry} onAction={reload}>
          {error}
        </ErrorNote>
      )}
      <div className="card card-table">
        {!data && !error && <div className="card-body"><Skeleton rows={4} label={C.loading} /></div>}
        {data && data.length === 0 && <Empty icon="globe">{C.empty}</Empty>}
        {data && data.length > 0 && (
          <div className="table-scroll" role="region" tabIndex={0} aria-label={C.tableLabel}>
            <table className="dtable connectors-table" aria-label={C.tableLabel}>
              <thead>
                <tr>
                  <th scope="col" className="c-name">{C.columns.name}</th>
                  <th scope="col" className="c-mode">{C.columns.mode}</th>
                  <th scope="col" className="c-health">{C.columns.health}</th>
                  <th scope="col" className="c-checked">{C.columns.checked}</th>
                </tr>
              </thead>
              <tbody>
                {data.map((c) => (
                  <tr key={c.name}>
                    <th scope="row" className="c-name">
                      <span className="conn-name">
                        <span className="conn-icon"><Icon name={systemIcon(c.name)} size={16} /></span>
                        {titleCase(c.name)}
                      </span>
                    </th>
                    <td className="c-mode">
                      <span className={`pill ${c.mode === 'live' ? 'pill-accent' : 'pill-warn'}`}>
                        <span className="pill-dot" aria-hidden="true" />
                        {C.modes[c.mode] ?? c.mode}
                      </span>
                    </td>
                    <td className="c-health"><HealthPill c={c} /></td>
                    <td className="c-checked">
                      <time dateTime={c.checked_at} title={`${auditTime(c.checked_at)} UTC`}>{relTime(c.checked_at, now)}</time>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>
      <p className="meta-line">
        <Icon name="info" size={15} />
        <span>{C.note}</span>
      </p>
    </div>
  );
}
