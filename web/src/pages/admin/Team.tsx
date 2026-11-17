import { useCallback } from 'react';
import type { TeamMember } from '../../api';
import { useApp } from '../../app-context';
import { Avatar, Empty, ErrorNote, Skeleton } from '../../components/bits';
import { Icon } from '../../components/icons';
import { copy } from '../../copy';
import { useLoad } from '../../admin/useLoad';
import { AdminHead, NoAccess, YesNo } from './parts';

const T = copy.admin.team;

function RolePill({ role }: { role: string }) {
  // A role this app has no word for is shown as written in team.yaml.
  return <span className={`pill ${role === 'admin' ? 'pill-accent' : ''}`}>{T.roleLabels[role] ?? role}</span>;
}

function MemberRow({ m }: { m: TeamMember }) {
  const sameName = m.name === m.handle;
  return (
    <tr>
      <th scope="row" className="c-member">
        <span className="member">
          <Avatar name={m.name} />
          <span className="member-text">
            <span className="member-name">{sameName ? m.handle : m.name}</span>
            {!sameName && <span className="member-handle">{m.handle}</span>}
          </span>
        </span>
      </th>
      <td className="c-roles">
        <span className="pill-row">
          {m.roles.length === 0 ? <span className="not-recorded">{T.noRoles}</span> : m.roles.map((r) => <RolePill key={r} role={r} />)}
        </span>
      </td>
      <td className="c-bot"><span className="mono-val">{m.bot}</span></td>
      <td className="c-yn"><YesNo yes={m.has_email} yesLabel={T.emailYes} noLabel={T.emailNo} /></td>
      <td className="c-yn"><YesNo yes={m.accounts ? m.accounts.atlassian : null} yesLabel={T.linked} noLabel={T.notLinked} /></td>
      <td className="c-yn"><YesNo yes={m.accounts ? m.accounts.slack : null} yesLabel={T.linked} noLabel={T.notLinked} /></td>
      <td className="c-yn"><YesNo yes={m.accounts ? m.accounts.github : null} yesLabel={T.linked} noLabel={T.notLinked} /></td>
    </tr>
  );
}

/** Everyone on the team list, with their roles and which accounts they have linked. Read only. */
export function Team() {
  const { api } = useApp();
  const load = useCallback(() => api.adminTeam(), [api]);
  const { data, error, forbidden, reload } = useLoad(load, T.loadFailed);
  if (forbidden) return <NoAccess />;

  return (
    <div className="page page-wide">
      <AdminHead section="team" title={copy.admin.sections.team} lede={T.lede} />
      {error && (
        <ErrorNote actionLabel={copy.errors.retry} onAction={reload}>
          {error}
        </ErrorNote>
      )}
      {data && !data.accounts_available && (
        <div className="note note-warn" role="status">
          <Icon name="alert" className="note-icon" />
          <div className="note-body"><p>{T.accountsDown}</p></div>
        </div>
      )}
      {data && data.accounts_available && data.linking === false && (
        <div className="note note-info" role="status">
          <Icon name="info" className="note-icon" />
          <div className="note-body"><p>{T.linkingOff}</p></div>
        </div>
      )}
      <div className="card card-table">
        {!data && !error && <div className="card-body"><Skeleton rows={5} label={T.loading} /></div>}
        {data && data.members.length === 0 && <Empty icon="followups">{T.empty}</Empty>}
        {data && data.members.length > 0 && (
          <>
            <div className="card-head card-head-sub">
              <h2>
                {copy.admin.sections.team}
                <span className="card-count">{T.count(data.members.length)}</span>
              </h2>
            </div>
            <div className="table-scroll" role="region" tabIndex={0} aria-label={T.tableLabel}>
              <table className="dtable team-table" aria-label={T.tableLabel}>
                <thead>
                  <tr>
                    <th scope="col" className="c-member">{T.columns.member}</th>
                    <th scope="col" className="c-roles">{T.columns.roles}</th>
                    <th scope="col" className="c-bot">{T.columns.bot}</th>
                    <th scope="col" className="c-yn">{T.columns.email}</th>
                    <th scope="col" className="c-yn">{T.columns.atlassian}</th>
                    <th scope="col" className="c-yn">{T.columns.slack}</th>
                    <th scope="col" className="c-yn">{T.columns.github}</th>
                  </tr>
                </thead>
                <tbody>
                  {data.members.map((m) => (
                    <MemberRow key={m.handle} m={m} />
                  ))}
                </tbody>
              </table>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
