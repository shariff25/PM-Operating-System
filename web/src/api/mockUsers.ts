// Sample data for the Team screen: a user database with every state the screen has to show, and
// the tool server's rules for each write (docs/web-api.md, "Admin: users and roles"). Fake names,
// fake addresses and fake ids only. Used by the sample-data client (tests and the dev server).
import { ApiError, USER_ROLES } from './types';
import type { DirectoryUser, IdentityKind, NewUser, OverviewUsers, PasswordSetupCode, UserFields, UserList, Written } from './types';

const HOUR = 60 * 60 * 1000;
const DAY = 24 * HOUR;
const GRACE_HOURS = 24;

export interface MockUsersOptions {
  /** The pilot exception has passed its end date and nobody switched it off. */
  pilotLapsed?: boolean;
  /** Every write is refused with this code (the dev server's `?mock_users=<code>`). */
  usersError?: string;
  /** The deactivation cap is already reached. */
  deactivationCapReached?: boolean;
  /** Add a person whose display name tries markup. */
  hostile?: boolean;
}

type Seed = Partial<DirectoryUser> & {
  handle: string;
  display_name: string;
  roles: string[];
  sign_in_name?: string | null;
  atlassian_email?: string | null;
  github_login?: string | null;
};

const iso = (ms: number) => new Date(ms).toISOString();

function seeds(now: number, hostile: boolean): Seed[] {
  const list: Seed[] = [
    {
      handle: 'ada', display_name: 'Ada Okafor', roles: ['admin'], entra: 'linked', slack_linked: true, entra_linked: true,
      atlassian_linked: true, github_linked: true, accounts: { atlassian: true, slack: true, github: true },
      sign_in_name: 'ada@contoso.example', atlassian_email: 'ada@contoso.example', github_login: 'ada-okafor',
    },
    {
      handle: 'raj', display_name: 'Raj Mehta', roles: ['admin', 'auditor'], entra: 'linked', slack_linked: true, entra_linked: true,
      atlassian_linked: true, accounts: { atlassian: true, slack: true, github: false },
      sign_in_name: 'raj@contoso.example', atlassian_email: 'raj@contoso.example',
    },
    {
      handle: 'ana', display_name: 'Ana Silva', roles: ['pm'], entra: 'linked', slack_linked: true, entra_linked: true,
      atlassian_linked: true, accounts: { atlassian: true, slack: true, github: false }, team_bots: ['scout'], reviewed_by: ['ben'],
      sign_in_name: 'ana@contoso.example', atlassian_email: 'ana@contoso.example',
      password_set: true, second_step_enrolled: true,
    },
    {
      handle: 'ben', display_name: 'Ben Carter', roles: ['pm', 'reviewer'], entra: 'linked', slack_linked: true, entra_linked: true,
      atlassian_linked: true, github_linked: true, accounts: { atlassian: true, slack: true, github: true }, reviews: ['ana', 'dana'],
      sign_in_name: 'ben@contoso.example', atlassian_email: 'ben@contoso.example', github_login: 'bencarter',
    },
    {
      handle: 'dana', display_name: '', roles: ['pm'], entra: 'linked', slack_linked: false, entra_linked: true,
      atlassian_linked: true, accounts: { atlassian: true, slack: false, github: false }, reviewed_by: ['ben'],
      sign_in_name: 'dana@contoso.example', atlassian_email: 'dana.w@contoso.example',
      password_set: true, second_step_enrolled: true, setup_code_pending: true,
    },
    {
      handle: 'priya', display_name: 'Priya Nair', roles: ['reviewer'], entra: 'linked', slack_linked: true, entra_linked: true,
      accounts: { atlassian: false, slack: true, github: false }, reviews: ['ravi'],
      sign_in_name: 'priya@contoso.example', atlassian_email: 'priya@contoso.example',
      password_set: true, second_step_enrolled: true,
    },
    {
      handle: 'ravi', display_name: 'Ravi Kumar', roles: ['pm'], entra: 'none', slack_linked: true,
      accounts: { atlassian: false, slack: true, github: false }, reviewed_by: ['priya'],
      sign_in_name: 'ravi@contoso.example', atlassian_email: 'ravi@contoso.example',
      setup_code_pending: true,
    },
    {
      handle: 'sam', display_name: 'Sam Lee', roles: ['pm'], entra: 'none', slack_linked: true, sign_in_name_confirmed: false,
      accounts: { atlassian: false, slack: true, github: false },
      sign_in_name: 'sam.lee@contoso.example', atlassian_email: 'sam.lee@contoso.example',
      created_at: iso(now - 2 * HOUR),
    },
    {
      handle: 'audrey', display_name: 'Audrey Nolan', roles: ['auditor'], entra: 'linked', entra_linked: true,
      accounts: { atlassian: false, slack: false, github: false },
      sign_in_name: 'audrey@contoso.example', atlassian_email: 'audrey@contoso.example',
    },
    {
      handle: 'noor', display_name: 'Noor Haddad', roles: ['auditor'], entra: 'needs_host', entra_linked: true,
      accounts: { atlassian: false, slack: false, github: false },
      sign_in_name: 'noor@contoso.example', atlassian_email: 'noor@contoso.example',
    },
    {
      handle: 'tom', display_name: 'Tom Reyes', roles: ['pm'], status: 'deactivated', entra: 'linked', slack_linked: true, entra_linked: true,
      atlassian_linked: true, accounts: { atlassian: true, slack: true, github: false },
      deactivated_at: iso(now - 3 * HOUR), grace_ends_at: iso(now - 3 * HOUR + GRACE_HOURS * HOUR),
      sign_in_name: 'tom@contoso.example', atlassian_email: 'tom@contoso.example',
    },
    {
      handle: 'lena', display_name: 'Lena Fischer', roles: ['pm', 'reviewer'], status: 'deactivated', entra: 'none', slack_linked: true,
      accounts: { atlassian: false, slack: false, github: false },
      deactivated_at: iso(now - 4 * DAY), grace_ends_at: iso(now - 4 * DAY + GRACE_HOURS * HOUR),
      sign_in_name: 'lena@contoso.example', atlassian_email: 'lena@contoso.example',
    },
    {
      handle: 'evals', display_name: 'Evals account', roles: ['pm'], entra: 'none',
      accounts: { atlassian: false, slack: false, github: false },
      sign_in_name: 'evals@contoso.example', atlassian_email: 'evals@contoso.example',
    },
  ];
  if (hostile) {
    list.push({
      handle: 'mallory', display_name: '<img src=x onerror="window.__pwned=1">**Mallory** <b>Admin</b>', roles: ['pm'], entra: 'none',
      accounts: { atlassian: false, slack: true, github: false }, slack_linked: true,
      sign_in_name: 'mallory@contoso.example', atlassian_email: 'mallory@contoso.example',
    });
  }
  return list;
}

function build(s: Seed, now: number): DirectoryUser {
  const u: DirectoryUser = {
    handle: s.handle,
    display_name: s.display_name,
    status: s.status ?? 'active',
    roles: [...s.roles],
    created_at: s.created_at ?? iso(now - 40 * DAY),
    deactivated_at: s.deactivated_at ?? null,
    grace_ends_at: s.grace_ends_at ?? null,
    dual_role: false,
    role_conflict: false,
    has_sign_in_name: !!s.sign_in_name,
    sign_in_name_confirmed: s.sign_in_name_confirmed ?? !!s.sign_in_name,
    sign_in_unconfirmed: false,
    entra: s.entra ?? 'none',
    entra_needs_host: false,
    slack_linked: s.slack_linked ?? false,
    entra_linked: s.entra_linked ?? false,
    atlassian_linked: s.atlassian_linked ?? false,
    github_linked: s.github_linked ?? false,
    accounts: s.accounts ?? { atlassian: false, slack: false, github: false },
    host_only: false,
    bot: `pm-${s.handle}`,
    team_bots: s.team_bots ?? [],
    reviews: s.reviews ?? [],
    reviewed_by: s.reviewed_by ?? [],
    sign_in_name: s.sign_in_name ?? null,
    atlassian_email: s.atlassian_email ?? null,
    github_login: s.github_login ?? null,
    password_set: s.password_set ?? false,
    second_step_enrolled: s.second_step_enrolled ?? false,
    setup_code_pending: s.setup_code_pending ?? false,
  };
  return u;
}

const HANDLE = /^[a-z0-9][a-z0-9._-]{0,63}$/;
const EMAIL = /^[^\s@<>]+@[^\s@<>]+\.[^\s@<>]+$/;
const GITHUB = /^[A-Za-z0-9-]{1,39}$/;
const SLACK = /^[UW][A-Z0-9]{2,}$/;
const RESERVED = new Set(['host', 'import', 'system']);

/** As the tool server cleans a display name: control characters, < and > removed, spaces collapsed, at most 100 characters. */
function cleanName(raw: string): string {
  const kept = [...raw].filter((c) => {
    const n = c.codePointAt(0) ?? 0;
    return n >= 0x20 && n !== 0x7f && c !== '<' && c !== '>';
  });
  return kept.join('').replace(/\s+/g, ' ').trim().slice(0, 100);
}

const refuse = (status: number, code: string, message: string) => new ApiError(status, code, message);

export interface MockUsers {
  list(viewer: string, asAdmin: boolean): UserList;
  overview(): OverviewUsers;
  invite(actor: string, input: NewUser): Written;
  setRoles(actor: string, handle: string, roles: string[], expected: number | undefined): Written;
  setFields(actor: string, handle: string, fields: UserFields, expected: number | undefined): Written;
  confirmSignIn(actor: string, handle: string, name: string, expected: number | undefined): Written;
  deactivate(actor: string, handle: string, expected: number | undefined): Written;
  reactivate(actor: string, handle: string, expected: number | undefined): Written;
  unlink(actor: string, handle: string, kind: IdentityKind): Written;
  assignReviewer(actor: string, reviewer: string, pms: string[], expected: number | undefined): Written;
  /** A password setup or reset code, shown once; the sign-in name goes to the password sample data with it. */
  passwordSetupCode(actor: string, handle: string): { code: PasswordSetupCode; signInName: string };
  /** A setup or reset finished in the password sample data. */
  passwordSet(handle: string): void;
}

export function createMockUsers(now0: number, options: MockUsersOptions = {}): MockUsers {
  const users = new Map<string, DirectoryUser>();
  for (const s of seeds(now0, options.hostile ?? false)) users.set(s.handle, build(s, now0));
  let version = 42;
  /** Setup codes made so far, so each one differs. */
  let codes = 0;
  /** When each web or Slack deactivation happened: the hourly cap counts these. */
  const deactivations: number[] = options.deactivationCapReached ? [now0 - 5 * 60_000, now0 - 15 * 60_000, now0 - 25 * 60_000] : [now0 - 20 * 60_000];
  const LIMIT = 3;
  const pilotEnds = options.pilotLapsed ? now0 - 2 * DAY : now0 + 10 * DAY;
  const pilotEndsAt = new Date(new Date(pilotEnds).toISOString().slice(0, 10) + 'T00:00:00Z').getTime();
  const now = () => Date.now();
  const pilotInForce = () => now() < pilotEndsAt;
  const holdsBoth = (roles: string[]) => roles.includes('admin') && roles.includes('auditor');

  /** The derived flags, as the tool server computes them. */
  function refresh(u: DirectoryUser) {
    const active = u.status === 'active';
    u.dual_role = active && holdsBoth(u.roles);
    u.role_conflict = u.dual_role && !pilotInForce();
    u.has_sign_in_name = !!u.sign_in_name;
    u.sign_in_unconfirmed = active && u.has_sign_in_name && !u.sign_in_name_confirmed;
    u.entra_needs_host = u.entra === 'needs_host';
    u.host_only = u.roles.includes('admin') || u.roles.includes('auditor');
    u.roles = USER_ROLES.filter((r) => u.roles.includes(r));
  }
  for (const u of users.values()) refresh(u);

  const sorted = () => [...users.values()].sort((a, b) => a.handle.localeCompare(b.handle));
  const lastHour = () => deactivations.filter((t) => t > now() - HOUR).length;
  const clone = <T,>(v: T): T => structuredClone(v);

  function auditorView(u: DirectoryUser): DirectoryUser {
    const c = clone(u);
    delete c.sign_in_name;
    delete c.atlassian_email;
    delete c.github_login;
    return c;
  }

  function find(handle: string): DirectoryUser {
    const u = users.get(handle);
    if (!u) throw refuse(404, 'not_found', 'There is no such person.');
    return u;
  }

  /** Who may write at all: an active admin, never through a forced refusal. */
  function begin(actor: string) {
    if (options.usersError) {
      const code = options.usersError;
      const status = code === 'unknown_outcome' || code === 'upstream_unavailable' ? 502
        : ['assertion_required', 'unavailable', 'users_unavailable'].includes(code) ? 503
        : code === 'deactivation_limit' ? 429
        : code === 'not_configured' ? 501
        : ['forbidden', 'host_only', 'self_change', 'bad_origin', 'bad_csrf'].includes(code) ? 403
        : code === 'invalid' ? 422
        : code === 'not_found' ? 404
        : code === 'bad_request' ? 400
        : 409;
      throw refuse(status, code, 'Refused by the sample data.');
    }
    const a = users.get(actor);
    if (!a || a.status !== 'active' || !a.roles.includes('admin')) throw refuse(403, 'forbidden', 'Only an admin may do that.');
  }

  const checkVersion = (expected: number | undefined) => {
    if (expected !== undefined && expected !== version) throw refuse(409, 'stale', 'The team changed since you loaded it.');
  };

  /** The invariants the database trigger checks: an active admin is left, and nobody holds both without the exception. */
  function invariants(target: DirectoryUser, roles: string[], status: DirectoryUser['status']) {
    if (status === 'active' && holdsBoth(roles) && !pilotInForce()) throw refuse(409, 'dual_role', 'Admin and Auditor together needs the pilot exception.');
    const admins = sorted().filter((u) => u.status === 'active' && u.roles.includes('admin') && u.handle !== target.handle);
    if (admins.length === 0 && !(status === 'active' && roles.includes('admin'))) throw refuse(409, 'last_admin', 'PM OS needs an active admin.');
  }

  function done(u: DirectoryUser, note?: string): Written {
    version += 1;
    refresh(u);
    return { version, user: clone(u), ...(note ? { note } : {}) };
  }

  function addressTaken(field: 'sign_in_name' | 'atlassian_email' | 'github_login', value: string, except: string) {
    const v = field === 'github_login' ? value.toLowerCase() : value;
    for (const u of users.values()) {
      if (u.handle === except) continue;
      const have = u[field];
      if (have && (field === 'github_login' ? have.toLowerCase() : have) === v) throw refuse(409, 'taken', 'That belongs to someone else.');
    }
  }

  return {
    list(viewer, asAdmin) {
      void viewer;
      const last = lastHour();
      return {
        version,
        view: asAdmin ? 'admin' : 'auditor',
        users: sorted().map((u) => (asAdmin ? clone(u) : auditorView(u))),
        deactivations: { last_hour: last, limit: LIMIT, remaining: Math.max(0, LIMIT - last), grace_hours: GRACE_HOURS },
        pilot_exception: {
          ends: iso(pilotEndsAt).slice(0, 10),
          ends_at: iso(pilotEndsAt),
          ending_soon: pilotInForce() && pilotEndsAt - now() <= 14 * DAY,
          lapsed: !pilotInForce(),
        },
        linking: true,
      };
    },

    overview() {
      const all = sorted();
      const active = all.filter((u) => u.status === 'active');
      const last = lastHour();
      return {
        active: active.length,
        deactivated: all.length - active.length,
        sign_in_unconfirmed: active.filter((u) => u.sign_in_unconfirmed).map((u) => u.handle),
        dual_role: active.filter((u) => u.dual_role).map((u) => u.handle),
        pilot_exception: { ends: iso(pilotEndsAt).slice(0, 10), ending_soon: pilotInForce() && pilotEndsAt - now() <= 14 * DAY, lapsed: !pilotInForce() },
        role_conflicts: active.filter((u) => u.role_conflict).map((u) => u.handle),
        entra_needs_host: active.filter((u) => u.entra_needs_host).map((u) => u.handle),
        no_slack: active.filter((u) => u.roles.includes('pm') && !u.slack_linked && u.handle !== 'evals').map((u) => u.handle),
        identity_conflicts: 1,
        identity_conflict_users: ['ana'],
        resolve_miss_warning: options.pilotLapsed ? ['console'] : [],
        resolve_miss_warning_threshold: 100,
        deactivations_last_hour: last,
        deactivation_limit: LIMIT,
        deactivation_limit_reached: last >= LIMIT,
        deactivation_grace_hours: GRACE_HOURS,
        bots_without_active_owner: all.filter((u) => u.status !== 'active').map((u) => u.bot),
      };
    },

    invite(actor, input) {
      begin(actor);
      const roles = input.roles ?? [];
      if (roles.includes('admin')) throw refuse(403, 'host_only', 'Admin is granted on the host.');
      const handle = (input.handle ?? '').trim();
      if (!HANDLE.test(handle) || handle.startsWith('bots.') || RESERVED.has(handle)) throw refuse(422, 'invalid', 'That handle is not valid.');
      if (!EMAIL.test(input.sign_in_name ?? '') || !EMAIL.test(input.atlassian_email ?? '')) throw refuse(422, 'invalid', 'An address is not valid.');
      if (input.github_login && !GITHUB.test(input.github_login)) throw refuse(422, 'invalid', 'That GitHub login is not valid.');
      if (input.slack_id && !SLACK.test(input.slack_id)) throw refuse(422, 'invalid', 'That Slack id is not valid.');
      if (roles.length === 0 || roles.some((r) => !['pm', 'reviewer', 'auditor'].includes(r))) throw refuse(422, 'invalid', 'Pick at least one role.');
      if (users.has(handle)) throw refuse(409, 'exists', 'That handle is taken.');
      const sign = input.sign_in_name.toLowerCase();
      const atl = input.atlassian_email.toLowerCase();
      addressTaken('sign_in_name', sign, handle);
      addressTaken('atlassian_email', atl, handle);
      if (input.github_login) addressTaken('github_login', input.github_login, handle);
      const u = build(
        {
          handle,
          display_name: cleanName(input.display_name ?? ''),
          roles,
          created_at: iso(now()),
          sign_in_name: sign,
          atlassian_email: atl,
          github_login: input.github_login || null,
          slack_linked: !!input.slack_id,
          accounts: { atlassian: false, slack: false, github: false },
        },
        now(),
      );
      users.set(handle, u);
      return done(u);
    },

    setRoles(actor, handle, roles, expected) {
      begin(actor);
      const u = find(handle);
      if (handle === actor) throw refuse(403, 'self_change', 'Nobody changes their own roles.');
      if (roles.some((r) => !(USER_ROLES as readonly string[]).includes(r)) || new Set(roles).size !== roles.length) throw refuse(422, 'invalid', 'Unknown role.');
      if (roles.includes('admin') && !u.roles.includes('admin')) throw refuse(403, 'host_only', 'Admin is granted on the host.');
      if (handle === 'evals' && roles.some((r) => r !== 'pm')) throw refuse(409, 'evals_account', 'The evals account holds pm only.');
      const next = USER_ROLES.filter((r) => roles.includes(r));
      if (next.join() === u.roles.join()) throw refuse(409, 'unchanged', 'Nothing to change.');
      checkVersion(expected);
      invariants(u, next, u.status);
      const gainsAuditor = next.includes('auditor') && !u.roles.includes('auditor');
      u.roles = next;
      if (gainsAuditor && u.entra === 'linked') u.entra = 'needs_host';
      return done(u);
    },

    setFields(actor, handle, fields, expected) {
      begin(actor);
      const u = find(handle);
      const keys = (Object.keys(fields) as (keyof UserFields)[]).filter((k) => fields[k] !== undefined);
      if (keys.length === 0) throw refuse(422, 'invalid', 'No field to change.');
      const addresses = keys.filter((k) => k !== 'display_name');
      if (handle === actor && addresses.length > 0) throw refuse(403, 'self_change', 'Nobody changes their own addresses.');
      if (u.host_only && addresses.length > 0) throw refuse(403, 'host_only', 'Host only.');
      if (fields.sign_in_name !== undefined && !EMAIL.test(fields.sign_in_name)) throw refuse(422, 'invalid', 'Not an address.');
      if (fields.atlassian_email !== undefined && !EMAIL.test(fields.atlassian_email)) throw refuse(422, 'invalid', 'Not an address.');
      if (fields.github_login !== undefined && !GITHUB.test(fields.github_login)) throw refuse(422, 'invalid', 'Not a GitHub login.');
      if (handle === 'evals' && addresses.length > 0) throw refuse(409, 'evals_account', 'The evals account is never linked.');
      const next = {
        sign_in_name: fields.sign_in_name?.toLowerCase(),
        atlassian_email: fields.atlassian_email?.toLowerCase(),
        github_login: fields.github_login,
        display_name: fields.display_name === undefined ? undefined : cleanName(fields.display_name),
      };
      const same =
        (next.sign_in_name === undefined || (next.sign_in_name === u.sign_in_name && u.sign_in_name_confirmed)) &&
        (next.atlassian_email === undefined || next.atlassian_email === u.atlassian_email) &&
        (next.github_login === undefined || next.github_login === u.github_login) &&
        (next.display_name === undefined || next.display_name === u.display_name);
      if (same) throw refuse(409, 'unchanged', 'Nothing to change.');
      if (next.atlassian_email !== undefined && next.atlassian_email !== u.atlassian_email && u.atlassian_linked) throw refuse(409, 'linked', 'Unlink Atlassian first.');
      if (next.github_login !== undefined && next.github_login !== u.github_login && u.github_linked) throw refuse(409, 'linked', 'Unlink GitHub first.');
      if (next.sign_in_name) addressTaken('sign_in_name', next.sign_in_name, handle);
      if (next.atlassian_email) addressTaken('atlassian_email', next.atlassian_email, handle);
      if (next.github_login) addressTaken('github_login', next.github_login, handle);
      checkVersion(expected);
      if (next.sign_in_name !== undefined) {
        u.sign_in_name = next.sign_in_name;
        u.sign_in_name_confirmed = true;
      }
      if (next.atlassian_email !== undefined) u.atlassian_email = next.atlassian_email;
      if (next.github_login !== undefined) u.github_login = next.github_login;
      if (next.display_name !== undefined) u.display_name = next.display_name;
      return done(u);
    },

    confirmSignIn(actor, handle, name, expected) {
      begin(actor);
      const u = find(handle);
      if (handle === actor) throw refuse(403, 'self_change', 'Nobody confirms their own sign-in name.');
      if (u.host_only) throw refuse(403, 'host_only', 'Host only.');
      if (!EMAIL.test(name)) throw refuse(422, 'invalid', 'Not an address.');
      const lower = name.toLowerCase();
      if (lower === u.sign_in_name && u.sign_in_name_confirmed) throw refuse(409, 'unchanged', 'Already confirmed.');
      addressTaken('sign_in_name', lower, handle);
      checkVersion(expected);
      u.sign_in_name = lower;
      u.sign_in_name_confirmed = true;
      return done(u);
    },

    deactivate(actor, handle, expected) {
      begin(actor);
      const u = find(handle);
      if (handle === actor) throw refuse(403, 'self_change', 'Nobody deactivates themselves.');
      if (u.status === 'deactivated') throw refuse(409, 'unchanged', 'Already deactivated.');
      if (u.team_bots.length > 0) throw refuse(409, 'owns_bots', 'They own team bots.');
      if (lastHour() >= LIMIT) throw refuse(429, 'deactivation_limit', 'The hourly cap is reached.');
      checkVersion(expected);
      invariants(u, u.roles.filter((r) => r !== 'admin'), 'deactivated');
      const t = now();
      u.status = 'deactivated';
      u.roles = u.roles.filter((r) => r !== 'admin');
      u.deactivated_at = iso(t);
      u.grace_ends_at = iso(t + GRACE_HOURS * HOUR);
      deactivations.push(t);
      return done(u, `\`${handle}\` is deactivated: their sessions are ending and their linked accounts are suspended. Their personal bot's sandbox keeps idling until \`pmos-admin pause ${u.bot}\`.`);
    },

    reactivate(actor, handle, expected) {
      begin(actor);
      const u = find(handle);
      if (handle === actor) throw refuse(403, 'self_change', 'Nobody reactivates themselves.');
      if (u.status === 'active') throw refuse(409, 'unchanged', 'Already active.');
      checkVersion(expected);
      invariants(u, u.roles, 'active');
      const inGrace = u.grace_ends_at !== null && Date.parse(u.grace_ends_at) > now();
      u.status = 'active';
      u.deactivated_at = null;
      u.grace_ends_at = null;
      if (!inGrace) {
        u.accounts = { atlassian: false, slack: false, github: false };
        u.atlassian_linked = false;
        u.github_linked = false;
      }
      return done(u);
    },

    unlink(actor, handle, kind) {
      begin(actor);
      const u = find(handle);
      if (handle === actor) throw refuse(403, 'self_change', 'Nobody unlinks their own accounts.');
      if (kind === 'entra' && u.host_only) throw refuse(403, 'host_only', 'Host only.');
      const linked = kind === 'entra' ? u.entra_linked : kind === 'atlassian' ? u.atlassian_linked : u.github_linked;
      if (!linked) throw refuse(409, 'unchanged', 'Nothing of that kind is linked.');
      if (kind === 'entra') {
        u.entra_linked = false;
        u.entra = 'none';
      } else if (kind === 'atlassian') {
        u.atlassian_linked = false;
        if (u.accounts) u.accounts.atlassian = false;
      } else {
        u.github_linked = false;
        if (u.accounts) u.accounts.github = false;
      }
      return done(u);
    },

    passwordSetupCode(actor, handle) {
      begin(actor);
      const u = find(handle);
      if (handle === actor) throw refuse(403, 'self_change', 'Nobody makes a setup code for themselves.');
      if (u.host_only) throw refuse(403, 'host_only', 'Host only: pmos-admin users password-setup.');
      if (handle === 'evals') throw refuse(409, 'evals_account', 'The evals account never has a password.');
      if (u.status !== 'active') throw refuse(409, 'not_active', 'They are deactivated.');
      if (!u.sign_in_name || !u.sign_in_name_confirmed) throw refuse(409, 'sign_in_unconfirmed', 'Confirm their sign-in name first.');
      codes += 1;
      const body = `${handle.slice(0, 4).toUpperCase().padEnd(4, 'X')}${codes}`.slice(0, 5);
      const code = `K7Q4M-P9X3T-${body}-W2R8N`;
      u.setup_code_pending = true;
      return {
        code: { setup_code: code, handle, kind: u.password_set ? 'reset' : 'setup', expires_at: iso(now() + DAY) },
        signInName: u.sign_in_name,
      };
    },

    passwordSet(handle) {
      const u = users.get(handle);
      if (!u) return;
      u.password_set = true;
      u.second_step_enrolled = true;
      u.setup_code_pending = false;
    },

    assignReviewer(actor, reviewer, pms, expected) {
      begin(actor);
      const r = find(reviewer);
      if (reviewer === actor) throw refuse(403, 'self_change', 'Nobody assigns themselves.');
      if (new Set(pms).size !== pms.length || pms.includes(reviewer) || pms.length > 200) throw refuse(422, 'invalid', 'A PM twice, or the reviewer.');
      const targets = pms.map(find);
      if (pms.length > 0) {
        if (r.status !== 'active') throw refuse(409, 'not_active', 'The reviewer is deactivated.');
        if (!r.roles.includes('reviewer')) throw refuse(409, 'not_reviewer', 'Not a reviewer.');
        for (const p of targets) {
          if (r.reviews.includes(p.handle)) continue;
          if (p.status !== 'active') throw refuse(409, 'not_active', 'A PM is deactivated.');
          if (!p.roles.includes('pm')) throw refuse(409, 'not_pm', 'Not a PM.');
        }
      }
      const next = [...pms].sort();
      if (next.join() === r.reviews.join()) throw refuse(409, 'unchanged', 'Nothing to change.');
      checkVersion(expected);
      for (const h of r.reviews) {
        const p = users.get(h);
        if (p) p.reviewed_by = p.reviewed_by.filter((x) => x !== reviewer);
      }
      for (const p of targets) p.reviewed_by = [...new Set([...p.reviewed_by, reviewer])].sort();
      r.reviews = next;
      return done(r);
    },
  };
}
