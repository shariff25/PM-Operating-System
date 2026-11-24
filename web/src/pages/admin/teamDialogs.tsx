import { useState } from 'react';
import type { DirectoryUser, IdentityKind, PasswordSetupCode, UserFields, UserList, Written } from '../../api';
import { USER_ROLES } from '../../api';
import { useApp } from '../../app-context';
import { Icon } from '../../components/icons';
import { copy } from '../../copy';
import { inGrace, listWords, longDay, nameOf, roleWords, validSlackId, whenUtc, writeOutcome } from '../../admin/team';
import { RoleChecks, TeamDialog, TextField } from './teamParts';
import type { Problem } from './teamParts';

const T = copy.admin.team;

/** What every Team dialog gets from the page: the request in flight, the last refusal, and how to send. */
export interface Flow {
  busy: boolean;
  problem: Problem | null;
  close: () => void;
  /** Send a write with the directory's current version; `done` is the line shown when the server says yes. */
  submit: (call: (version: number) => Promise<Written>, done: (w: Written) => string) => void;
}

const sameSet = (a: readonly string[], b: readonly string[]) => a.length === b.length && a.every((x) => b.includes(x));
const sortRoles = (roles: readonly string[]) => USER_ROLES.filter((r) => roles.includes(r)) as string[];
// ---------- Invite ----------

export function InviteDialog({ flow }: { flow: Flow }) {
  const { api } = useApp();
  const I = T.invite;
  const [handle, setHandle] = useState('');
  const [name, setName] = useState('');
  const [signIn, setSignIn] = useState('');
  const [atlassian, setAtlassian] = useState('');
  const [github, setGithub] = useState('');
  const [slack, setSlack] = useState('');
  const [roles, setRoles] = useState<string[]>(['pm']);
  const slackId = slack.trim();
  const slackBad = slackId !== '' && !validSlackId(slackId);
  const filled = handle.trim() !== '' && signIn.trim() !== '' && atlassian.trim() !== '' && roles.length > 0;
  const ok = filled && !slackBad;

  return (
    <TeamDialog
      title={I.heading}
      intro={I.intro}
      onClose={flow.close}
      busy={flow.busy}
      problem={flow.problem}
      canSubmit={ok}
      hint={ok ? null : filled ? I.fixSlack : I.required}
      submitLabel={I.submit}
      busyLabel={I.submitting}
      testId="invite-dialog"
      onSubmit={() => {
        const body = {
          handle: handle.trim(),
          display_name: name.trim(),
          sign_in_name: signIn.trim(),
          atlassian_email: atlassian.trim(),
          ...(github.trim() ? { github_login: github.trim() } : {}),
          ...(slackId ? { slack_id: slackId } : {}),
          roles: sortRoles(roles),
        };
        flow.submit(() => api.adminInvite(body), (w) => I.done(nameOf(w.user)));
      }}
    >
      <div className="field-grid">
        <TextField label={I.handle} value={handle} onChange={setHandle} hint={I.handleHint} mono />
        <TextField label={I.displayName} value={name} onChange={setName} optional autoComplete="off" />
      </div>
      <div className="field-grid">
        <TextField label={I.signInName} type="email" value={signIn} onChange={setSignIn} hint={I.signInHint} mono />
        <TextField label={I.atlassianEmail} type="email" value={atlassian} onChange={setAtlassian} hint={I.atlassianHint} mono />
        <TextField label={I.githubLogin} value={github} onChange={setGithub} optional mono />
        <TextField
          label={I.slackId}
          value={slack}
          onChange={setSlack}
          optional
          mono
          invalid={slackBad}
          hint={slackBad ? I.slackInvalid : I.slackHint}
        />
      </div>
      <RoleChecks legend={I.roles} roles={['pm', 'reviewer', 'auditor']} value={roles} onChange={setRoles} />
      <p className="quiet-line team-host-line">
        <Icon name="lock" size={14} />
        <span>{I.adminNote} <code className="mono-val">{I.adminCommand}</code>.</span>
      </p>
    </TeamDialog>
  );
}

// ---------- Roles ----------

export function RolesDialog({ user, list, flow }: { user: DirectoryUser; list: UserList; flow: Flow }) {
  const { api } = useApp();
  const R = T.roles;
  const who = nameOf(user);
  const [roles, setRoles] = useState<string[]>(sortRoles(user.roles));
  const next = sortRoles(roles);
  const changed = !sameSet(next, user.roles);
  const both = next.includes('admin') && next.includes('auditor');
  const pilot = list.pilot_exception;
  const pilotOn = pilot !== null && !pilot.lapsed;

  return (
    <TeamDialog
      title={R.heading(who)}
      intro={<>{R.intro} <span className="team-now">{R.now(user.roles.length ? roleWords(user.roles) : T.noRoles)}</span></>}
      onClose={flow.close}
      busy={flow.busy}
      problem={flow.problem}
      canSubmit={changed}
      hint={changed ? null : R.noChange}
      submitLabel={R.submit}
      busyLabel={R.submitting}
      testId="roles-dialog"
      onSubmit={() =>
        flow.submit(
          (v) => api.adminSetRoles(user.handle, next, v),
          (w) => (w.user.roles.length ? R.done(nameOf(w.user), roleWords(w.user.roles)) : R.doneNone(nameOf(w.user))),
        )
      }
    >
      <RoleChecks
        legend={T.columns.roles}
        roles={USER_ROLES}
        value={roles}
        onChange={setRoles}
        disabled={{ admin: !user.roles.includes('admin') }}
        notes={{ admin: user.roles.includes('admin') ? R.adminRemove : R.adminHost }}
      />
      {user.handle === 'evals' && <p className="field-hint">{R.evals}</p>}
      {both && (pilotOn || changed) && (
        <div className={`note ${pilotOn ? 'note-info' : 'note-warn'}`}>
          <Icon name={pilotOn ? 'info' : 'alert'} className="note-icon" />
          <div className="note-body"><p>{pilotOn && pilot ? R.dualOk(longDay(pilot.ends)) : R.dualNo}</p></div>
        </div>
      )}
      {next.length === 0 && changed && (
        <div className="note note-warn">
          <Icon name="alert" className="note-icon" />
          <div className="note-body"><p>{R.noneWarn}</p></div>
        </div>
      )}
    </TeamDialog>
  );
}

// ---------- Fields ----------

export function FieldsDialog({ user, self, flow }: { user: DirectoryUser; self: boolean; flow: Flow }) {
  const { api } = useApp();
  const F = T.fields;
  const who = nameOf(user);
  const start = {
    display_name: user.display_name,
    sign_in_name: user.sign_in_name ?? '',
    atlassian_email: user.atlassian_email ?? '',
    github_login: user.github_login ?? '',
  };
  const [v, setV] = useState(start);
  const set = (k: keyof typeof start) => (x: string) => setV((s) => ({ ...s, [k]: x }));
  const addressesLocked = self || user.host_only;
  const lock = {
    sign_in_name: addressesLocked,
    atlassian_email: addressesLocked || user.atlassian_linked,
    github_login: addressesLocked || user.github_linked,
  };

  const out: UserFields = {};
  let cleared = false;
  for (const k of ['display_name', 'sign_in_name', 'atlassian_email', 'github_login'] as const) {
    if (k !== 'display_name' && lock[k]) continue;
    const now = k === 'display_name' ? v[k] : v[k].trim();
    if (now === start[k]) continue;
    if (now.trim() === '' && start[k] !== '') cleared = true;
    else if (now.trim() !== '') out[k] = now;
  }
  const changed = Object.keys(out).length > 0;
  const hint = cleared ? F.cantClear : changed ? null : F.noChange;

  const addressHint = (k: 'sign_in_name' | 'atlassian_email' | 'github_login', free: string | undefined) => {
    if (addressesLocked) return undefined;
    if (k === 'sign_in_name') return user.entra_linked ? F.signInLinked : F.signInFree;
    if (k === 'atlassian_email' && user.atlassian_linked) return F.atlassianLinked;
    if (k === 'github_login' && user.github_linked) return F.githubLinked;
    return free;
  };

  return (
    <TeamDialog
      title={F.heading(who)}
      intro={F.intro}
      onClose={flow.close}
      busy={flow.busy}
      problem={flow.problem}
      canSubmit={changed && !cleared}
      hint={hint}
      submitLabel={F.submit}
      busyLabel={F.submitting}
      testId="fields-dialog"
      onSubmit={() => flow.submit((ver) => api.adminSetFields(user.handle, out, ver), (w) => F.done(nameOf(w.user)))}
    >
      <TextField label={T.invite.displayName} value={v.display_name} onChange={set('display_name')} autoFocus />
      {addressesLocked && (
        <div className="note note-info">
          <Icon name="lock" className="note-icon" />
          <div className="note-body"><p>{self ? F.self : F.hostOnly(who)}</p></div>
        </div>
      )}
      <TextField label={T.invite.signInName} type="email" value={v.sign_in_name} onChange={set('sign_in_name')} readOnly={lock.sign_in_name} hint={addressHint('sign_in_name', undefined)} mono />
      <TextField label={T.invite.atlassianEmail} type="email" value={v.atlassian_email} onChange={set('atlassian_email')} readOnly={lock.atlassian_email} hint={addressHint('atlassian_email', undefined)} mono />
      <TextField label={T.invite.githubLogin} value={v.github_login} onChange={set('github_login')} readOnly={lock.github_login} hint={addressHint('github_login', undefined)} mono />
    </TeamDialog>
  );
}

// ---------- Confirm a sign-in name ----------

export function ConfirmSignInDialog({ user, flow }: { user: DirectoryUser; flow: Flow }) {
  const { api } = useApp();
  const C = T.confirm;
  const who = nameOf(user);
  const [name, setName] = useState(user.sign_in_name ?? '');
  const ok = name.trim() !== '';
  return (
    <TeamDialog
      title={C.heading(who)}
      intro={C.intro(who)}
      onClose={flow.close}
      busy={flow.busy}
      problem={flow.problem}
      canSubmit={ok}
      submitLabel={C.submit}
      busyLabel={C.submitting}
      testId="confirm-dialog"
      onSubmit={() => flow.submit((v) => api.adminConfirmSignIn(user.handle, name.trim(), v), (w) => C.done(nameOf(w.user)))}
    >
      <TextField label={C.field} type="email" value={name} onChange={setName} hint={C.hint} mono autoFocus />
    </TeamDialog>
  );
}

// ---------- Deactivate and reactivate ----------

export function DeactivateDialog({ user, list, flow }: { user: DirectoryUser; list: UserList; flow: Flow }) {
  const { api } = useApp();
  const D = T.deactivate;
  const who = nameOf(user);
  const { remaining, limit, grace_hours } = list.deactivations;
  const capped = remaining <= 0;
  const owns = user.team_bots.length > 0;
  return (
    <TeamDialog
      danger
      title={D.heading(who)}
      onClose={flow.close}
      busy={flow.busy}
      problem={flow.problem}
      canSubmit={!capped && !owns}
      submitLabel={D.submit}
      busyLabel={D.submitting}
      testId="deactivate-dialog"
      onSubmit={() => flow.submit((v) => api.adminDeactivate(user.handle, v), (w) => D.done(nameOf(w.user)))}
    >
      <div className="team-consequences">
        <h3>{D.now}</h3>
        <ul className="checklist">
          <li className="check-item check-info"><Icon name="logout" size={15} className="check-icon" /><span>{D.sessions}</span></li>
          <li className="check-item check-info"><Icon name="lock" size={15} className="check-icon" /><span>{D.accounts}</span></li>
          <li className="check-item check-info"><Icon name="inbox" size={15} className="check-icon" /><span>{D.drafts}</span></li>
          {user.roles.includes('admin') && (
            <li className="check-item check-warn"><Icon name="shield" size={15} className="check-icon" /><span>{D.admin}</span></li>
          )}
        </ul>
      </div>
      <p className="quiet-line team-undo">
        <Icon name="clock" size={15} />
        <span>{D.undo(grace_hours)}</span>
      </p>
      {owns ? (
        <div className="note note-warn">
          <Icon name="alert" className="note-icon" />
          <div className="note-body"><p>{D.ownsBots(listWords(user.team_bots))}</p></div>
        </div>
      ) : capped ? (
        <div className="note note-warn">
          <Icon name="alert" className="note-icon" />
          <div className="note-body"><p>{D.capped}</p></div>
        </div>
      ) : (
        <p className="team-cap">{D.left(remaining, limit)}</p>
      )}
    </TeamDialog>
  );
}

export function ReactivateDialog({ user, flow }: { user: DirectoryUser; flow: Flow }) {
  const { api } = useApp();
  const R = T.reactivate;
  const who = nameOf(user);
  const grace = inGrace(user);
  const ends = user.grace_ends_at ? whenUtc(user.grace_ends_at) : '';
  return (
    <TeamDialog
      title={R.heading(who)}
      intro={grace ? R.inGrace(ends) : R.afterGrace(ends)}
      onClose={flow.close}
      busy={flow.busy}
      problem={flow.problem}
      submitLabel={R.submit}
      busyLabel={R.submitting}
      testId="reactivate-dialog"
      onSubmit={() => flow.submit((v) => api.adminReactivate(user.handle, v), (w) => R.done(nameOf(w.user)))}
    >
      <p>{R.kept(user.roles.length ? roleWords(user.roles) : T.noRoles)}</p>
      <p className="quiet-line team-host-line">
        <Icon name="lock" size={14} />
        <span>{R.noAdmin}</span>
      </p>
    </TeamDialog>
  );
}

// ---------- Unlink ----------

export function UnlinkDialog({ user, kind, flow }: { user: DirectoryUser; kind: IdentityKind; flow: Flow }) {
  const { api } = useApp();
  const U = T.unlink;
  const who = nameOf(user);
  const what = U.names[kind]!;
  return (
    <TeamDialog
      danger
      title={U.heading(what, who)}
      intro={kind === 'entra' ? U.entra : U.other(what, U.fieldNames[kind]!)}
      onClose={flow.close}
      busy={flow.busy}
      problem={flow.problem}
      submitLabel={U.submit(what)}
      busyLabel={U.submitting}
      testId="unlink-dialog"
      onSubmit={() => flow.submit(() => api.adminUnlink(user.handle, kind), (w) => U.done(what, nameOf(w.user)))}
    />
  );
}

// ---------- Password setup and reset codes ----------

/**
 * A setup code (no password yet) or a reset code (replaces everything), made by an admin and shown
 * to them once (docs/web-api.md, "Password sign-in"). Someone holding Admin or Auditor gets theirs
 * from the host, so the dialog says that instead of offering to make one.
 */
export function SetupCodeDialog({ user, onClose, onMade }: { user: DirectoryUser; onClose: () => void; onMade: () => void }) {
  const { api } = useApp();
  const SC = T.setupCode;
  const who = nameOf(user);
  const reset = user.password_set === true;
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<Problem | null>(null);
  const [made, setMade] = useState<PasswordSetupCode | null>(null);
  const [copied, setCopied] = useState<'yes' | 'failed' | null>(null);

  if (made) {
    return (
      <TeamDialog
        key="made"
        title={made.kind === 'reset' ? SC.doneReset(who) : SC.doneSetup(who)}
        onClose={onClose}
        onSubmit={onClose}
        busy={false}
        problem={null}
        submitLabel={SC.done}
        busyLabel={SC.done}
        hideCancel
        testId="setup-code-made"
      >
        <div className="setup-code-box">
          <span className="field-label" id="setup-code-label">{SC.codeLabel}</span>
          <div className="setup-code-row">
            <code className="setup-code" aria-labelledby="setup-code-label">{made.setup_code}</code>
            <button
              type="button"
              className="btn btn-sm"
              data-autofocus
              onClick={() => {
                if (!navigator.clipboard?.writeText) return setCopied('failed');
                navigator.clipboard.writeText(made.setup_code).then(() => setCopied('yes'), () => setCopied('failed'));
              }}
            >
              <Icon name={copied === 'yes' ? 'check' : 'docs'} size={14} />
              {copied === 'yes' ? SC.copied : SC.copy}
            </button>
          </div>
          <span className="sr-only" role="status">{copied === 'yes' ? SC.copied : ''}</span>
          {copied === 'failed' && <p className="field-hint" role="alert">{SC.copyFailed}</p>}
          <p className="setup-code-once">
            <Icon name="alert" size={14} />
            <span>{SC.once} {SC.expires(whenUtc(made.expires_at))}</span>
          </p>
        </div>
        <ul className="checklist setup-code-steps">
          <li className="check-item check-info"><Icon name="lock" size={15} className="check-icon" /><span>{SC.handOff}</span></li>
          <li className="check-item check-info"><Icon name="arrowRight" size={15} className="check-icon" /><span>{SC.whatTheyDo(who)}</span></li>
        </ul>
      </TeamDialog>
    );
  }

  if (user.host_only) {
    return (
      <TeamDialog
        title={SC.headingHost(who)}
        onClose={onClose}
        onSubmit={onClose}
        busy={false}
        problem={null}
        submitLabel={SC.close}
        busyLabel={SC.close}
        hideCancel
        testId="setup-code-host"
      >
        <div className="note note-info">
          <Icon name="lock" className="note-icon" />
          <div className="note-body">
            <p>{SC.hostOnly(who)}</p>
            <code className="mono-val team-command">{SC.hostCommand(user.handle)}</code>
          </div>
        </div>
      </TeamDialog>
    );
  }

  const create = () => {
    if (busy) return;
    setBusy(true);
    setProblem(null);
    void (async () => {
      try {
        setMade(await api.adminPasswordSetupCode(user.handle));
        onMade();
      } catch (e) {
        const out = writeOutcome(e);
        if (out.kind === 'refused') setProblem({ text: Object.hasOwn(SC.errors, out.code) ? SC.errors[out.code]! : out.text, tone: 'error' });
        else {
          onMade();
          setProblem({ text: T.unknownReloaded, tone: 'warn' });
        }
      } finally {
        setBusy(false);
      }
    })();
  };

  return (
    <TeamDialog
      title={reset ? SC.headingReset(who) : SC.headingSetup(who)}
      intro={reset ? SC.introReset(who) : SC.introSetup(who)}
      onClose={onClose}
      onSubmit={create}
      busy={busy}
      problem={problem}
      submitLabel={SC.submit}
      busyLabel={SC.submitting}
      testId="setup-code-dialog"
    >
      {(reset || user.setup_code_pending) && (
        <ul className="checklist">
          {reset && <li className="check-item check-info"><Icon name="clock" size={15} className="check-icon" /><span>{SC.stillWorks}</span></li>}
          {user.setup_code_pending && <li className="check-item check-info"><Icon name="refresh" size={15} className="check-icon" /><span>{SC.replaces}</span></li>}
        </ul>
      )}
    </TeamDialog>
  );
}

// ---------- Reviewer assignments ----------

export function ReviewersDialog({ user, list, flow }: { user: DirectoryUser; list: UserList; flow: Flow }) {
  const { api } = useApp();
  const R = T.reviewers;
  const who = nameOf(user);
  const [pms, setPms] = useState<string[]>([...user.reviews]);
  // Active PMs, and anyone already assigned (who may since have been deactivated or lost the role).
  const options = list.users.filter(
    (u) => u.handle !== user.handle && ((u.status === 'active' && u.roles.includes('pm')) || user.reviews.includes(u.handle)),
  );
  const changed = !sameSet(pms, user.reviews);
  return (
    <TeamDialog
      title={R.heading(who)}
      intro={R.intro}
      onClose={flow.close}
      busy={flow.busy}
      problem={flow.problem}
      canSubmit={changed}
      hint={changed ? null : R.noChange}
      submitLabel={R.submit}
      busyLabel={R.submitting}
      testId="reviewers-dialog"
      onSubmit={() => flow.submit((v) => api.adminAssignReviewer(user.handle, [...pms].sort(), v), (w) => R.done(nameOf(w.user), w.user.reviews.length))}
    >
      {options.length === 0 ? (
        <p className="muted">{R.none}</p>
      ) : (
        <fieldset className="role-checks pm-checks">
          <legend className="field-label">{R.pmsLabel}</legend>
          {options.map((u) => {
            const on = pms.includes(u.handle);
            const flag = u.status !== 'active' ? R.inactive : !u.roles.includes('pm') ? R.notPm : null;
            return (
              <label key={u.handle} className="role-check">
                <input type="checkbox" checked={on} onChange={() => setPms(on ? pms.filter((x) => x !== u.handle) : [...pms, u.handle])} />
                <span className="role-check-text">
                  <span className="role-check-name">{nameOf(u)}</span>
                  <span className="role-check-hint mono-val">
                    {u.handle}
                    {flag && <span className="pm-flag"> · {flag}</span>}
                  </span>
                </span>
              </label>
            );
          })}
        </fieldset>
      )}
    </TeamDialog>
  );
}
