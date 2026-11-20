// Sample data for password sign-in (docs/web-api.md, "Password sign-in"): the tool server's rules
// for the sign-in, setup and signed-in routes, with fake people, fake passwords and fake codes
// only. Used by the sample-data client (tests and the dev server); never part of a production build.
//
// The sample flow, for trying the screens by hand on the dev server (`?mock_providers=password,dev`):
//
// | Who  | Sign-in name          | What works                                                        |
// | ---- | --------------------- | ----------------------------------------------------------------- |
// | ana  | ana@contoso.example   | password `paper-lantern-harbour`, then the authenticator code      |
// | dana | dana@contoso.example  | password `quiet-meadow-lamp-42`; reset code `RESET-DANA1-23456-78900` |
// | ravi | ravi@contoso.example  | no password yet; setup code `SETUP-RAVI1-23456-78900`             |
//
// - The authenticator code is always `246810` here (MOCK_TOTP). A real authenticator code changes
//   every 30 seconds and is accepted once.
// - Ana's recovery codes are MOCK_RECOVERY_CODES; each works once.
// - New recovery codes (after a setup, or made on the security page) are made up from a counter.
// - Wrong guesses are counted: 20 refused guesses in a row answer `429 too_many_attempts` with
//   `Retry-After: 60`, as pmos-web's per-address limit does.
//
// Dev-only switches (api/dev.ts): `?mock_password=<code>` answers that refusal once, on the first
// route in the flow that can send it (`weak_password:<reason>` picks the reason), and
// `?mock_retry_after=<seconds>` sets `Retry-After` for `too_many_attempts`.
import { ApiError } from './types';
import type {
  PasswordChange,
  PasswordEnrolment,
  PasswordSecondStep,
  PasswordSetupDone,
  PasswordSetupStarted,
  PasswordSignedIn,
  PasswordSignInStarted,
  PasswordStatus,
  WeakPasswordReason,
} from './types';

/** The authenticator code the sample data accepts. */
export const MOCK_TOTP = '246810';
/** Ana's ten recovery codes. */
export const MOCK_RECOVERY_CODES = [
  'kite-moss-reed-lamp', 'fern-dusk-opal-brim', 'wren-tide-silk-palm', 'cove-mint-haze-loom', 'gale-iris-plum-sage',
  'lark-dune-rust-flax', 'moth-peak-vale-zinc', 'oath-rook-sand-tarn', 'pike-quay-reef-yarn', 'vine-wolf-yew0-ash1',
];
export const MOCK_PASSWORDS = { ana: 'paper-lantern-harbour', dana: 'quiet-meadow-lamp-42' } as const;
export const MOCK_SETUP_CODES = { ravi: 'SETUP-RAVI1-23456-78900', dana: 'RESET-DANA1-23456-78900' } as const;

const MIN = 12;
const MAX = 128;
/** A few of the common passwords of 12 characters or more the tool server refuses. */
const COMMON = new Set(['password1234', '123456789012', 'qwertyuiopas', 'iloveyou1234', 'passwordpassword', 'correcthorsebatterystaple']);

/** Which route can send which refusal, in the order a person meets the routes (the contract's table). */
const ROUTES = {
  'sign-in': ['sign_in_failed', 'too_many_attempts', 'sign_in_busy', 'users_unavailable', 'not_configured', 'assertion_required'],
  'second-step': ['code_failed', 'sign_in_expired', 'not_a_member', 'too_many_attempts'],
  'setup/begin': ['setup_failed', 'not_a_member', 'sign_in_unconfirmed', 'host_only', 'evals_account', 'too_many_attempts'],
  'setup/password': ['setup_expired', 'weak_password', 'not_a_member', 'sign_in_busy'],
  'setup/authenticator': ['setup_expired', 'wrong_step'],
  'setup/confirm': ['code_failed', 'setup_expired', 'wrong_step', 'host_only', 'sign_in_unconfirmed', 'too_many_attempts'],
  change: ['reauth_failed', 'weak_password', 'no_password', 'too_many_attempts', 'unknown_outcome'],
  'recovery-codes': ['reauth_failed', 'no_password', 'too_many_attempts', 'unknown_outcome'],
} as const satisfies Record<string, readonly string[]>;
type Route = keyof typeof ROUTES;

const STATUS: Record<string, number> = {
  sign_in_failed: 401, code_failed: 401, sign_in_expired: 401, setup_failed: 401, setup_expired: 401,
  wrong_step: 409, weak_password: 422, reauth_failed: 403, no_password: 409, too_many_attempts: 429,
  sign_in_busy: 503, users_unavailable: 503, not_configured: 501, assertion_required: 503, not_a_member: 403,
  host_only: 403, sign_in_unconfirmed: 409, evals_account: 409, unknown_outcome: 502,
};

interface Account {
  handle: string;
  signInName: string;
  password: string | null;
  recovery: { code: string; used: boolean }[];
  /** A live setup or reset code, as typed by nobody yet: normalised. */
  setupCode: string | null;
  /** Someone holding Admin or Auditor: a code made on the web never works for them. */
  hostOnly?: boolean;
}

export interface MockPasswordOptions {
  /** Answer this refusal once, on the first route in the flow that can send it. `weak_password:<reason>` picks the reason. */
  passwordRefusal?: string;
  /** `Retry-After` in seconds for `too_many_attempts`. Default 60. */
  retryAfter?: number;
}

export interface MockPassword {
  signIn(name: string, password: string): PasswordSignInStarted;
  secondStep(input: PasswordSecondStep): { handle: string; result: PasswordSignedIn };
  setupBegin(name: string, code: string): PasswordSetupStarted;
  setupPassword(password: string): void;
  setupAuthenticator(): PasswordEnrolment;
  setupConfirm(code: string): { handle: string; done: PasswordSetupDone };
  change(handle: string, input: PasswordChange): void;
  recoveryCodes(handle: string, code: string): string[];
  status(handle: string): PasswordStatus;
  /** An admin made a code on the Team page: it replaces any earlier one. */
  issueCode(handle: string, signInName: string, code: string): void;
  hasPassword(handle: string): boolean;
}

const normCode = (s: string) => s.replace(/[\s-]/g, '').toUpperCase();
const normRecovery = (s: string) => s.replace(/[\s-]/g, '').toLowerCase();
const digits = (s: string) => s.replace(/\s/g, '');

export function createMockPassword(options: MockPasswordOptions = {}): MockPassword {
  const accounts = new Map<string, Account>();
  const fresh = (codes: string[]) => codes.map((code) => ({ code, used: false }));
  accounts.set('ana', { handle: 'ana', signInName: 'ana@contoso.example', password: MOCK_PASSWORDS.ana, recovery: fresh(MOCK_RECOVERY_CODES), setupCode: null });
  accounts.set('dana', {
    handle: 'dana', signInName: 'dana@contoso.example', password: MOCK_PASSWORDS.dana,
    recovery: fresh(MOCK_RECOVERY_CODES.map((c) => c.replace(/^..../, 'dana'))), setupCode: normCode(MOCK_SETUP_CODES.dana),
  });
  accounts.set('ravi', { handle: 'ravi', signInName: 'ravi@contoso.example', password: null, recovery: [], setupCode: normCode(MOCK_SETUP_CODES.ravi) });

  let forced = options.passwordRefusal ?? null;
  const retryAfter = options.retryAfter ?? 60;
  let refused = 0;
  let made = 0;

  let pending: { handle: string; tries: number; expires: number } | null = null;
  let setup: {
    handle: string;
    kind: 'setup' | 'reset';
    step: 'begun' | 'password' | 'authenticator';
    tries: number;
    expires: number;
    password: string | null;
    secret: string | null;
  } | null = null;

  const byName = (name: string) => [...accounts.values()].find((a) => a.signInName === name.trim().toLowerCase()) ?? null;
  const account = (handle: string) => accounts.get(handle) ?? null;

  function refuse(code: string, extra: Record<string, unknown> = {}): never {
    throw new ApiError(STATUS[code] ?? 400, code, 'Refused by the sample data.', extra, code === 'too_many_attempts' ? retryAfter : null);
  }

  /** The dev switch: the forced refusal, once, on the first route that can send it. */
  function forcedFor(route: Route) {
    if (!forced) return;
    const [code, reason] = forced.split(':');
    if (!code || !(ROUTES[route] as readonly string[]).includes(code)) return;
    forced = null;
    if (code === 'code_failed') refuse(code, { attempts_left: route === 'setup/confirm' ? 4 : 2 });
    if (code === 'weak_password') refuse(code, { reason: reason ?? 'common' });
    if (code === 'sign_in_expired') pending = null;
    if (code === 'setup_expired') setup = null;
    refuse(code);
  }

  /** pmos-web's per-address limit: 20 refused guesses, then every unauthenticated route waits. */
  const limited = () => {
    if (refused >= 20) refuse('too_many_attempts');
  };
  const guessed = () => {
    refused += 1;
  };

  function weakness(password: string, a: Account): WeakPasswordReason | null {
    const n = [...password].length;
    if (n < MIN) return 'too_short';
    if (n > MAX) return 'too_long';
    const p = password.trim().toLowerCase();
    if (p === a.handle || p === a.signInName || p === a.signInName.split('@')[0]) return 'matches_name';
    if (COMMON.has(p)) return 'common';
    return null;
  }

  const newCodes = () =>
    Array.from({ length: 10 }, (_, i) => {
      made += 1;
      const n = String(made * 7 + i).padStart(4, '0');
      return `new${n.slice(-1)}-code-${n}-${['ab', 'cd', 'ef', 'gh', 'ij', 'kl', 'mn', 'op', 'qr', 'st'][i]}xy`;
    });

  return {
    signIn(name, password) {
      forcedFor('sign-in');
      limited();
      const a = byName(name);
      // One answer for every reason: no such name, no password, a wrong password.
      if (!a || !a.password || a.password !== password) {
        guessed();
        refuse('sign_in_failed');
      }
      pending = { handle: a.handle, tries: 3, expires: Date.now() + 5 * 60_000 };
      return { next: 'second_step', expires_at: new Date(pending.expires).toISOString() };
    },

    secondStep(input) {
      forcedFor('second-step');
      limited();
      if (!pending || pending.expires < Date.now()) {
        pending = null;
        refuse('sign_in_expired');
      }
      const a = account(pending.handle)!;
      let ok = false;
      let recoveryUsed = false;
      if ('code' in input) {
        ok = digits(input.code) === MOCK_TOTP;
      } else {
        const hit = a.recovery.find((r) => !r.used && normRecovery(r.code) === normRecovery(input.recovery_code));
        if (hit) {
          hit.used = true;
          ok = true;
          recoveryUsed = true;
        }
      }
      if (!ok) {
        guessed();
        pending.tries -= 1;
        const left = pending.tries;
        if (left <= 0) pending = null;
        refuse('code_failed', { attempts_left: left });
      }
      pending = null;
      refused = 0;
      return { handle: a.handle, result: { recovery_code_used: recoveryUsed, recovery_codes_left: a.recovery.filter((r) => !r.used).length } };
    },

    setupBegin(name, code) {
      forcedFor('setup/begin');
      limited();
      const a = byName(name);
      if (!a || !a.setupCode || a.setupCode !== normCode(code)) {
        guessed();
        refuse('setup_failed');
      }
      if (a.hostOnly) refuse('host_only');
      const kind = a.password ? 'reset' : 'setup';
      setup = { handle: a.handle, kind, step: 'begun', tries: 5, expires: Date.now() + 15 * 60_000, password: null, secret: null };
      return { handle: a.handle, kind, expires_at: new Date(setup.expires).toISOString(), password: { min_length: MIN, max_length: MAX } };
    },

    setupPassword(password) {
      forcedFor('setup/password');
      if (!setup || setup.expires < Date.now()) {
        setup = null;
        refuse('setup_expired');
      }
      const weak = weakness(password, account(setup.handle)!);
      if (weak) refuse('weak_password', { reason: weak });
      setup.password = password;
      setup.step = 'password';
    },

    setupAuthenticator() {
      forcedFor('setup/authenticator');
      if (!setup || setup.expires < Date.now()) {
        setup = null;
        refuse('setup_expired');
      }
      if (setup.step === 'begun') refuse('wrong_step');
      // A fake secret: a new one each time, as the real route makes.
      made += 1;
      const secret = `JBSWY3DPEHPK3PXP${String(made).padStart(4, '0')}MOCKSECRETAA`.slice(0, 32).replace(/[^A-Z2-7]/g, 'Q');
      setup.secret = secret;
      setup.step = 'authenticator';
      const a = account(setup.handle)!;
      const label = encodeURIComponent(`PM OS:${a.signInName}`);
      return {
        otpauth_uri: `otpauth://totp/${label}?secret=${secret}&issuer=PM%20OS&algorithm=SHA1&digits=6&period=30`,
        secret,
        account: a.signInName,
        issuer: 'PM OS',
        algorithm: 'SHA1',
        digits: 6,
        period: 30,
      };
    },

    setupConfirm(code) {
      forcedFor('setup/confirm');
      limited();
      if (!setup || setup.expires < Date.now()) {
        setup = null;
        refuse('setup_expired');
      }
      if (setup.step !== 'authenticator' || !setup.password) refuse('wrong_step');
      if (digits(code) !== MOCK_TOTP) {
        guessed();
        setup.tries -= 1;
        const left = setup.tries;
        if (left <= 0) setup = null;
        refuse('code_failed', { attempts_left: left });
      }
      const a = account(setup.handle)!;
      const kind = setup.kind;
      a.password = setup.password;
      a.setupCode = null;
      const codes = newCodes();
      a.recovery = fresh(codes);
      setup = null;
      pending = null;
      refused = 0;
      return { handle: a.handle, done: { kind, recovery_codes: codes } };
    },

    change(handle, input) {
      forcedFor('change');
      const a = account(handle);
      if (!a || !a.password) refuse('no_password');
      if (a.password !== input.current_password || digits(input.code) !== MOCK_TOTP) refuse('reauth_failed');
      const weak = weakness(input.new_password, a);
      if (weak) refuse('weak_password', { reason: weak });
      a.password = input.new_password;
    },

    recoveryCodes(handle, code) {
      forcedFor('recovery-codes');
      const a = account(handle);
      if (!a || !a.password) refuse('no_password');
      if (digits(code) !== MOCK_TOTP) refuse('reauth_failed');
      const codes = newCodes();
      a.recovery = fresh(codes);
      return codes;
    },

    status(handle) {
      const a = account(handle);
      const set = !!a?.password;
      return { enabled: true, password_set: set, second_step_enrolled: set, recovery_codes_left: a ? a.recovery.filter((r) => !r.used).length : 0 };
    },

    issueCode(handle, signInName, code) {
      const a = account(handle) ?? { handle, signInName, password: null, recovery: [], setupCode: null };
      a.signInName = signInName;
      a.setupCode = normCode(code);
      accounts.set(handle, a);
      if (setup?.handle === handle) setup = null;
    },

    hasPassword(handle) {
      return !!account(handle)?.password;
    },
  };
}
