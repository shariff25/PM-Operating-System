import { useCallback, useEffect, useRef, useState } from 'react';
import type { DirectoryUser, IdentityKind, UserList, Written } from '../../api';
import { ApiError } from '../../api';
import { useApp } from '../../app-context';
import { Avatar, Empty, ErrorNote, Skeleton } from '../../components/bits';
import { Icon } from '../../components/icons';
import { useToast } from '../../components/Toasts';
import { copy } from '../../copy';
import { messageOf } from '../../app-context';
import {
  TEAM_FILTERS,
  filterUsers,
  inGrace,
  listWords,
  longDay,
  nameOf,
  needsAttention,
  teamQuery,
  whenUtc,
  writeOutcome,
} from '../../admin/team';
import type { TeamFilter } from '../../admin/team';
import { tabKeys } from '../Drafts';
import { AdminHead, NoAccess } from './parts';
import { RowMenu } from './teamParts';
import type { MenuItem, Problem } from './teamParts';
import {
  ConfirmSignInDialog,
  DeactivateDialog,
  FieldsDialog,
  InviteDialog,
  ReactivateDialog,
  ReviewersDialog,
  RolesDialog,
  UnlinkDialog,
} from './teamDialogs';
import type { Flow } from './teamDialogs';

const T = copy.admin.team;

type Action =
  | { kind: 'invite' }
  | { kind: 'roles' | 'fields' | 'confirm' | 'deactivate' | 'reactivate' | 'reviewers'; handle: string }
  | { kind: 'unlink'; handle: string; identity: IdentityKind };

// ---------- Cells ----------

function RolesCell({ u, list }: { u: DirectoryUser; list: UserList }) {
  // Admin and Auditor together is one badge, so the pairing reads at a glance.
  const both = u.dual_role || u.role_conflict;
  const rest = both ? u.roles.filter((r) => r !== 'admin' && r !== 'auditor') : u.roles;
  const ends = list.pilot_exception ? longDay(list.pilot_exception.ends) : null;
  return (
    <span className="pill-row">
      {u.roles.length === 0 && <span className="not-recorded">{T.noRoles}</span>}
      {both && (
        <span
          className={`pill ${u.role_conflict ? 'pill-bad' : 'pill-warn'} pill-dual`}
          title={u.role_conflict ? T.roleConflictHint : ends ? T.dualRoleHint(ends) : undefined}
        >
          {u.role_conflict ? <Icon name="alert" size={12} /> : <span className="pill-dot" aria-hidden="true" />}
          {T.dualRole}
          <span className="sr-only">. {u.role_conflict ? T.roleConflictHint : ends ? T.dualRoleHint(ends) : ''}</span>
        </span>
      )}
      {rest.map((r) => (
        <span key={r} className={`pill ${r === 'admin' ? 'pill-accent' : ''}`}>{T.roleLabels[r] ?? r}</span>
      ))}
    </span>
  );
}

function StatusCell({ u }: { u: DirectoryUser }) {
  if (u.status === 'active') {
    return (
      <span className="pill pill-plain pill-good">
        <span className="pill-dot" aria-hidden="true" />
        {T.status.active}
      </span>
    );
  }
  const grace = inGrace(u);
  return (
    <span className="cell-stack">
      <span className="pill pill-plain">
        <span className="pill-dot" aria-hidden="true" />
        {T.status.deactivated}
      </span>
      {u.grace_ends_at && (
        <span className={`cell-sub cell-wrap ${grace ? 'cell-sub-warn' : ''}`}>
          {grace ? T.undoUntil(whenUtc(u.grace_ends_at)) : T.graceOver(whenUtc(u.grace_ends_at))}
        </span>
      )}
    </span>
  );
}

function SignInCell({ u, admin, onConfirm }: { u: DirectoryUser; admin: boolean; onConfirm: (() => void) | null }) {
  const S = T.signIn;
  const address = admin && u.sign_in_name ? <span className="cell-sub mono-val" title={u.sign_in_name}>{u.sign_in_name}</span> : null;
  let state;
  if (u.entra_needs_host) {
    state = <span className="sign-state sign-warn" title={S.needsHostHint}><Icon name="lock" size={13} />{S.needsHost}<span className="sr-only">. {S.needsHostHint}</span></span>;
  } else if (u.sign_in_unconfirmed) {
    state = (
      <span className="sign-line">
        <span className="sign-state sign-warn" title={S.unconfirmedHint}><Icon name="alert" size={13} />{S.unconfirmed}<span className="sr-only">. {S.unconfirmedHint}</span></span>
        {onConfirm && (
          <button type="button" className="btn btn-sm btn-quiet sign-confirm" onClick={onConfirm} aria-label={S.confirmFor(nameOf(u))}>
            {S.confirm}
          </button>
        )}
      </span>
    );
  } else if (u.entra_linked) {
    state = <span className="sign-state sign-good"><Icon name="check" size={13} weight={2.25} />{S.linked}</span>;
  } else if (u.has_sign_in_name) {
    state = <span className="sign-state">{S.notYet}</span>;
  } else {
    state = <span className="sign-state">{S.none}</span>;
  }
  return (
    <span className="cell-stack">
      {state}
      {address}
    </span>
  );
}

function AccountsCell({ u }: { u: DirectoryUser }) {
  const A = T.accounts;
  const items = [
    { key: 'slack', label: A.slack, on: u.accounts ? u.accounts.slack : null },
    { key: 'atlassian', label: A.atlassian, on: u.accounts ? u.accounts.atlassian : null },
    { key: 'github', label: A.github, on: u.accounts ? u.accounts.github : null },
  ];
  return (
    <ul className="acct-list">
      {items.map((it) => (
        <li key={it.key} className={`acct ${it.on ? 'acct-on' : it.on === null ? 'acct-unknown' : ''}`}>
          {it.on ? <Icon name="check" size={11} weight={2.5} /> : <span className="acct-dot" aria-hidden="true" />}
          <span aria-hidden="true">{it.label}</span>
          <span className="sr-only">{it.on ? A.linked(it.label) : it.on === null ? A.unknown(it.label) : A.notLinked(it.label)}</span>
        </li>
      ))}
    </ul>
  );
}

function ReviewsCell({ u }: { u: DirectoryUser }) {
  const lines = [
    u.reviews.length > 0 ? T.reviews(listWords(u.reviews)) : null,
    u.reviewed_by.length > 0 ? T.reviewedBy(listWords(u.reviewed_by)) : null,
  ].filter((x): x is string => x !== null);
  if (lines.length === 0) return <span className="cell-none">{T.noReviews}</span>;
  return (
    <span className="cell-stack">
      {lines.map((l) => (
        <span key={l} className="cell-sub cell-wrap" title={l}>{l}</span>
      ))}
    </span>
  );
}

// ---------- The page ----------

/** The list, read again on demand. `apply` puts a write's answer on screen at once, before the list is read again. */
function useUsers() {
  const { api } = useApp();
  const [state, setState] = useState<{ data: UserList | null; error: string | null; forbidden: boolean }>({ data: null, error: null, forbidden: false });
  const latest = useRef(0);
  const dataRef = useRef<UserList | null>(null);

  const reload = useCallback(async () => {
    const mine = ++latest.current;
    setState((s) => ({ ...s, error: null }));
    try {
      const data = await api.adminUsers();
      if (mine !== latest.current) return;
      dataRef.current = data;
      setState({ data, error: null, forbidden: false });
    } catch (e) {
      if (mine !== latest.current) return;
      const forbidden = e instanceof ApiError && e.status === 403;
      setState((s) => ({ data: s.data, error: forbidden ? null : messageOf(e, T.loadFailed), forbidden }));
    }
  }, [api]);

  useEffect(() => {
    void reload();
    return () => {
      latest.current += 1;
    };
  }, [reload]);

  const apply = useCallback((w: Written) => {
    const cur = dataRef.current;
    if (!cur) return;
    const has = cur.users.some((u) => u.handle === w.user.handle);
    const users = has ? cur.users.map((u) => (u.handle === w.user.handle ? w.user : u)) : [...cur.users, w.user].sort((a, b) => a.handle.localeCompare(b.handle));
    const next = { ...cur, version: w.version, users };
    dataRef.current = next;
    setState((s) => ({ ...s, data: next }));
  }, []);

  return { ...state, reload, apply, current: () => dataRef.current };
}

/** Everyone in the user database: roles, status, sign-in and linked accounts. Admins change people here; auditors read. */
export function Team({ search = '' }: { search?: string }) {
  const { me } = useApp();
  const toast = useToast();
  const { data, error, forbidden, reload, apply, current } = useUsers();
  const query = teamQuery(search);
  const [filter, setFilter] = useState<TeamFilter>(query.show);
  const [action, setAction] = useState<Action | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<Problem | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const tabs = useRef<Partial<Record<TeamFilter, HTMLButtonElement | null>>>({});
  const scrolled = useRef(false);

  // Follow a link from the overview: the filter it names, and the person it points at.
  useEffect(() => {
    setFilter(query.show);
    scrolled.current = false;
  }, [query.show, query.person]);
  useEffect(() => {
    if (!data || !query.person || scrolled.current) return;
    scrolled.current = true;
    document.getElementById(`person-${query.person}`)?.scrollIntoView?.({ block: 'center' });
  }, [data, query.person]);

  if (forbidden) return <NoAccess />;

  const admin = data?.view === 'admin';
  const target = action && action.kind !== 'invite' ? (data?.users.find((u) => u.handle === action.handle) ?? null) : null;

  const open = (a: Action) => {
    setProblem(null);
    setAction(a);
  };
  const close = () => {
    const back = action && action.kind !== 'invite' ? `[data-actions="${action.handle}"]` : '[data-invite]';
    setAction(null);
    setProblem(null);
    // Focus goes back where the dialog was opened from.
    requestAnimationFrame(() => document.querySelector<HTMLElement>(back)?.focus());
  };

  const flow: Flow = {
    busy,
    problem,
    close,
    submit: (call, done) => {
      const list = current();
      if (!list || busy) return;
      setBusy(true);
      setProblem(null);
      void (async () => {
        try {
          const w = await call(list.version);
          apply(w);
          setNote(w.note ?? null);
          toast(done(w), 'good');
          setBusy(false);
          close();
          await reload();
        } catch (e) {
          const out = writeOutcome(e);
          if (out.kind === 'refused') {
            setProblem({ text: out.text, tone: 'error' });
          } else {
            await reload();
            setProblem({ text: out.kind === 'stale' ? T.staleReloaded : T.unknownReloaded, tone: 'warn' });
          }
          setBusy(false);
        }
      })();
    },
  };

  const itemsFor = (u: DirectoryUser): MenuItem[] => {
    const self = u.handle === me.handle;
    const items: MenuItem[] = [];
    const active = u.status === 'active';
    if (!self && active) items.push({ label: T.roles.menu, icon: 'shield', onSelect: () => open({ kind: 'roles', handle: u.handle }) });
    items.push({ label: T.fields.menu, icon: 'edit', onSelect: () => open({ kind: 'fields', handle: u.handle }) });
    if (self) return items;
    if (u.sign_in_unconfirmed && !u.host_only) items.push({ label: T.confirm.menu, icon: 'check', onSelect: () => open({ kind: 'confirm', handle: u.handle }) });
    if (active && u.roles.includes('reviewer')) items.push({ label: T.reviewers.menu, icon: 'followups', onSelect: () => open({ kind: 'reviewers', handle: u.handle }) });
    if (u.entra_linked && !u.host_only) items.push({ label: T.unlink.menu(T.unlink.names.entra!), icon: 'link', danger: true, onSelect: () => open({ kind: 'unlink', handle: u.handle, identity: 'entra' }) });
    if (u.atlassian_linked) items.push({ label: T.unlink.menu(T.unlink.names.atlassian!), icon: 'link', danger: true, onSelect: () => open({ kind: 'unlink', handle: u.handle, identity: 'atlassian' }) });
    if (u.github_linked) items.push({ label: T.unlink.menu(T.unlink.names.github!), icon: 'link', danger: true, onSelect: () => open({ kind: 'unlink', handle: u.handle, identity: 'github' }) });
    if (active) items.push({ label: T.deactivate.menu, icon: 'logout', danger: true, onSelect: () => open({ kind: 'deactivate', handle: u.handle }) });
    else items.push({ label: T.reactivate.menu, icon: 'refresh', onSelect: () => open({ kind: 'reactivate', handle: u.handle }) });
    return items;
  };

  const shown = data ? filterUsers(data.users, filter) : [];
  const counts = data ? { all: data.users.length, attention: data.users.filter(needsAttention).length, deactivated: data.users.filter((u) => u.status === 'deactivated').length } : null;
  const pilot = data?.pilot_exception ?? null;
  const conflicts = data ? data.users.filter((u) => u.role_conflict).map((u) => u.handle) : [];

  return (
    <div className="page page-wide">
      <AdminHead section="team" title={copy.admin.sections.team} lede={data && !admin ? T.ledeAuditor : T.lede}>
        {admin && (
          <button type="button" className="btn btn-primary team-invite" data-invite onClick={() => open({ kind: 'invite' })}>
            <Icon name="plus" size={15} />
            {T.invite.button}
          </button>
        )}
      </AdminHead>
      {error && (
        <ErrorNote actionLabel={copy.errors.retry} onAction={() => void reload()}>
          {error}
        </ErrorNote>
      )}
      {data && !admin && (
        <div className="note note-info" role="status">
          <Icon name="info" className="note-icon" />
          <div className="note-body"><p>{T.auditorView}</p></div>
        </div>
      )}
      {pilot?.lapsed && conflicts.length > 0 && (
        <div className="note note-error" role="alert">
          <Icon name="alert" className="note-icon" />
          <div className="note-body"><p>{T.pilotLapsed(longDay(pilot.ends), listWords(conflicts))}</p></div>
        </div>
      )}
      {pilot && pilot.ending_soon && !pilot.lapsed && (
        <div className="note note-warn" role="status">
          <Icon name="clock" className="note-icon" />
          <div className="note-body"><p>{T.pilotEnding(longDay(pilot.ends))}</p></div>
        </div>
      )}
      {note && (
        <div className="note note-info team-note" role="status">
          <Icon name="info" className="note-icon" />
          <div className="note-body">
            <p>{note}</p>
            <button type="button" className="btn btn-sm" onClick={() => setNote(null)}>{T.dismiss}</button>
          </div>
        </div>
      )}
      {data && data.linking === false && (
        <div className="note note-info" role="status">
          <Icon name="info" className="note-icon" />
          <div className="note-body"><p>{T.linkingOff}</p></div>
        </div>
      )}
      <div className="card card-table">
        {!data && !error && <div className="card-body"><Skeleton rows={6} label={T.loading} /></div>}
        {data && counts && (
          <>
            <div className="card-head card-head-sub team-head">
              <div className="tabs tabs-scroll team-tabs" role="tablist" aria-label={T.filtersLabel} onKeyDown={(e) => tabKeys(e, TEAM_FILTERS, filter, (f) => { setFilter(f); tabs.current[f]?.focus(); })}>
                {TEAM_FILTERS.map((f) => (
                  <button
                    key={f}
                    ref={(el) => { tabs.current[f] = el; }}
                    type="button"
                    role="tab"
                    id={`team-tab-${f}`}
                    aria-selected={filter === f}
                    aria-controls="team-panel"
                    tabIndex={filter === f ? 0 : -1}
                    className="tab"
                    onClick={() => setFilter(f)}
                  >
                    {f === 'attention' && !admin ? T.attentionAuditor : T.filters[f]}
                    <span className={`tab-count ${f === 'attention' && counts.attention > 0 ? 'tab-count-warn' : ''}`}>{counts[f]}</span>
                  </button>
                ))}
              </div>
              {admin && (
                <span className={`card-count team-cap-line ${data.deactivations.remaining === 0 ? 'team-cap-out' : ''}`}>
                  {T.deactivationsLeft(data.deactivations.remaining, data.deactivations.limit)}
                </span>
              )}
            </div>
            <section id="team-panel" role="tabpanel" aria-labelledby={`team-tab-${filter}`}>
              {shown.length === 0 ? (
                <Empty icon={filter === 'attention' ? 'check' : 'followups'} small>{T.emptyFilter[filter]}</Empty>
              ) : (
                <div className="table-scroll" role="region" tabIndex={0} aria-label={T.tableLabel}>
                  <table className={`dtable people-table ${admin ? 'people-admin' : 'people-auditor'}`} aria-label={T.tableLabel}>
                    <thead>
                      <tr>
                        <th scope="col" className="c-member">{T.columns.member}</th>
                        <th scope="col" className="c-roles">{T.columns.roles}</th>
                        <th scope="col" className="c-status">{T.columns.status}</th>
                        <th scope="col" className="c-signin">{T.columns.signIn}</th>
                        <th scope="col" className="c-accounts">{T.columns.accounts}</th>
                        <th scope="col" className="c-reviews">{T.columns.reviews}</th>
                        {admin && <th scope="col" className="c-actions"><span className="sr-only">{T.columns.actions}</span></th>}
                      </tr>
                    </thead>
                    <tbody>
                      {shown.map((u) => {
                        const self = u.handle === me.handle;
                        const name = nameOf(u);
                        return (
                          <tr key={u.handle} id={`person-${u.handle}`} className={`${u.status === 'deactivated' ? 'row-off' : ''} ${query.person === u.handle ? 'row-target' : ''}`}>
                            <th scope="row" className="c-member">
                              <span className="member">
                                <Avatar name={name} />
                                <span className="member-text">
                                  <span className="member-name" title={name}>
                                    {name}
                                    {self && <span className="you-tag">{T.you}</span>}
                                  </span>
                                  {name !== u.handle && <span className="member-handle">{u.handle}</span>}
                                </span>
                              </span>
                            </th>
                            <td className="c-roles"><RolesCell u={u} list={data} /></td>
                            <td className="c-status"><StatusCell u={u} /></td>
                            <td className="c-signin">
                              <SignInCell u={u} admin={admin} onConfirm={admin && !self && !u.host_only ? () => open({ kind: 'confirm', handle: u.handle }) : null} />
                            </td>
                            <td className="c-accounts"><AccountsCell u={u} /></td>
                            <td className="c-reviews"><ReviewsCell u={u} /></td>
                            {admin && (
                              <td className="c-actions">
                                <RowMenu label={T.menu.button(name)} handle={u.handle} items={itemsFor(u)} note={self ? T.menu.self : undefined} />
                              </td>
                            )}
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              )}
            </section>
          </>
        )}
      </div>

      {admin && data && action?.kind === 'invite' && <InviteDialog flow={flow} />}
      {admin && data && target && action && action.kind !== 'invite' && (
        <>
          {action.kind === 'roles' && <RolesDialog user={target} list={data} flow={flow} />}
          {action.kind === 'fields' && <FieldsDialog user={target} self={target.handle === me.handle} flow={flow} />}
          {action.kind === 'confirm' && <ConfirmSignInDialog user={target} flow={flow} />}
          {action.kind === 'deactivate' && <DeactivateDialog user={target} list={data} flow={flow} />}
          {action.kind === 'reactivate' && <ReactivateDialog user={target} flow={flow} />}
          {action.kind === 'unlink' && <UnlinkDialog user={target} kind={action.identity} flow={flow} />}
          {action.kind === 'reviewers' && <ReviewersDialog user={target} list={data} flow={flow} />}
        </>
      )}
    </div>
  );
}
