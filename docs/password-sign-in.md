# Password sign-in with a required authenticator code

A second way to sign in to the web app, beside Entra ID: a **sign-in name and password, then
always a code from an authenticator app** (or one of ten single-use recovery codes). It is off
unless `PMOS_WEB_PASSWORD_LOGIN=1` is set on `pmos-web`, and it can run alone, with Entra, or
beside the loopback development sign-in. There is no way to sign in with a password alone and
no setting that turns the second step off.

There is no email. A person gets a password by a **one-time setup code** an Admin makes on the
Team page (or, for an Admin or Auditor, someone with host access makes with
`pmos-admin users password-setup <handle>`), passed on by hand. A forgotten password, a lost
phone or a suspected compromise is the same: a new setup code, which replaces everything.

The contract the front end builds from is `docs/web-api.md` ("Password sign-in"); the tool
server's routes are in `docs/toolserver.md` ("Password sign-in"); the code is
`crates/toolserver/src/password/`, `crates/toolserver/src/app/password.rs`,
`crates/toolserver/src/users/password_pg.rs` (migration `0020_password_sign_in.sql`) and
`crates/web/src/app/password.rs`.

## Where the secrets live

**The tool server owns users, so it owns their password hash, their authenticator secret and
their recovery codes**, in its own tables (migration 0020):

| Table | Holds | Never holds |
| --- | --- | --- |
| `password_credentials` | the Argon2id PHC string, the authenticator secret sealed by the vault, the last accepted authenticator step, the credential's **epoch**, the two failure counters | a password, a secret in the clear |
| `password_recovery_codes` | SHA-256 of each code (bound to the handle), used or not | a code |
| `password_setup_codes` | SHA-256 of each setup or reset code, who issued it (web or host), its expiry, use and replacement | a code |
| `password_pending` | SHA-256 of each token between steps, its owner, purpose, expiry and tries; during a setup the chosen password's hash and the sealed secret being enrolled | a token |

- The **authenticator secret** is sealed with the vault's AES-256-GCM keys
  (`crate::vault::KeyRing`, `PMOS_VAULT_KEY`), bound by its additional data to the person and
  this use (`pmos-vault|<handle>|password|totp`): copied to another person's row it does not
  unseal. A secret sealed with an older key is sealed again at its next use.
- **`pmos-web` never stores or sees a hash or a secret.** It forwards what the person typed to
  the tool server with its own token (`PMOS_WEB_TOKEN`), and for a signed-in person their user
  assertion, and acts on the answer. The tokens between steps travel to the browser in
  `HttpOnly` cookies.
- **Only `pmos-web`'s token reaches the password routes**: the orchestrator's, the console's and
  the Slack bot's get `403`, no token `401` (tested).
- **The routes run only in assertion `require`** with no development override (`503
  assertion_required`), and `pmos-web` refuses to start with `PMOS_WEB_PASSWORD_LOGIN=1` unless
  it signs for the tool server (a `web:toolserver` key) and `PMOS_ASSERTION_MODE` is unset or
  `require`.
- Without `PMOS_VAULT_KEY` the tool server has no way to seal a secret, so password sign-in is
  off there (`501 not_configured`). The key is required with live connectors already.

## The flows

### Getting an account (setup), and a reset

1. **An Admin makes a setup code** for a person on the Team page
   (`POST /api/admin/users/{handle}/password-setup-code`). It is shown to the admin once. Rules,
   checked by the tool server in the person's transaction:
   - the actor is an active Admin (`403 forbidden`), and not the person (`403 self_change`:
     nobody issues a code for themselves);
   - the person exists (`404 not_found`), isn't the `evals` account (`409 evals_account`), is
     active (`409 not_active`) and has a **confirmed** sign-in name (`409 sign_in_unconfirmed`);
   - **for a person holding Admin or Auditor, only the host** may issue one (`403 host_only`):
     `pmos-admin users password-setup <handle>`, which records `PMOS_HOST_USER`, mirroring the
     Entra host-link rule.
   A new code replaces the person's live one (and ends any setup in progress with it). The code
   is a **setup** code for someone with no password yet and a **reset** code for someone with
   one. Recorded: `role_changes` `issue_password_code` (detail `setup` or `reset`) and the audit
   event `password.setup_code`.
2. **The person opens the setup page** and enters their sign-in name and the code
   (`…/setup/begin`). Every refusal before the code matched is one answer, `401 setup_failed`:
   no such name, no code, a wrong, expired, used or replaced code, or an account waiting out
   its delay. Once the code matched, a refusal can say why: deactivated (`403 not_a_member`),
   an unconfirmed name (`409 sign_in_unconfirmed`), or an Admin's or Auditor's code that the host
   didn't issue (`403 host_only`; the person's roles are checked again at every step). A setup
   token (15 minutes, never past the code's expiry) goes into the `__Host-pmos-setup` cookie.
3. **A password** (`…/setup/password`): checked against the rules below, hashed, kept with the
   setup (not yet in force).
4. **The authenticator** (`…/setup/authenticator`): the tool server makes a 160-bit secret and
   answers it once as base32 text and an `otpauth://` URI; the front end draws the QR code (the
   server has no image library). Calling it again makes a new secret.
5. **A code from the app** (`…/setup/confirm`). Then, in one transaction: the password, the
   secret and ten new recovery codes **replace** whatever the person had; the setup code is
   spent; every pending token of theirs goes; the credential's **epoch** moves, so every
   password session of theirs ends; recorded `role_changes` `set_password` (`setup` or `reset`)
   and `password.setup_complete`. `pmos-web` also ends every session the person has (Entra and
   development ones included) and starts a new one in this browser. The recovery codes are
   shown once.

Until a reset code is used, the old password keeps working: to shut a person out at once,
deactivate them.

### Signing in

1. **The password step** (`…/sign-in`): sign-in name and password. Every refusal is the same
   answer, `401 sign_in_failed`, with the same status, body and work: no such user, no password
   set, a wrong password, a deactivated user, an account waiting out its delay. When there is no
   real hash to check, a hash of a random password is checked instead, so the time spent is the
   same class. What happened is in the audit log, by reason (`unknown_name`, `no_password`,
   `wrong_password`, `deactivated`, `locked`), never by value.
2. A correct password gives **only a pending token** (`__Host-pmos-pending` cookie): single
   purpose (the second step of this one person), five minutes, three tries, stored hashed,
   worthless after a reset or a password change (it carries the epoch). **It grants nothing
   else.**
3. **The second step** (`…/second-step`): an authenticator code, accepted for the step before,
   the current step or the step after, and only for a step later than the last one accepted (a
   code works once), or one of the recovery codes (each works once). A wrong code is
   `401 code_failed` with `attempts_left`. Then the session, exactly as the other sign-ins make
   it (`__Host-pmos`, rotated, with its CSRF token), holding the credential's epoch; the sign-in
   event `web.sign_in` with `provider: password`.

### Signed in

- **Change the password**: the current password and a fresh authenticator code. The epoch
  moves: every other session of the person ends; this browser gets a new session.
- **New recovery codes**: an authenticator code; the ten old ones stop working.
- **Every request** of a password session resolves the person in the user database (at most 10
  seconds old) like an Entra session: not active, every session of theirs ends
  (`403 not_a_member`); the epoch moved (a reset, a change), the session ends
  (`401 signed_out`). The epoch is in `resolve`'s answer to `pmos-web` only.

## The rules for a password

- **12 to 128 characters** (Unicode scalar values), any characters, no composition rules.
- Not the handle, not the sign-in name, not the part of the sign-in name before `@`
  (case-insensitive, trimmed), and not one of a built-in list of the 60 most common passwords of
  12 characters or more (`crates/toolserver/src/password/hash.rs`, `COMMON`).
- A refused one is `422 weak_password` with `reason`: `too_short`, `too_long`,
  `matches_name` or `common`.
- **Never logged, never in an audit detail, never in an error.** Inputs are held in zeroizing
  buffers in the tool server, and Argon2's own buffers are zeroized by the crate.

## Hashing

**Argon2id**, version 0x13, **m = 19,456 KiB (19 MiB), t = 2, p = 1**, a 16-byte random salt per
hash and a 32-byte output: OWASP's Password Storage Cheat Sheet's first-listed minimum ("Use
Argon2id with a minimum configuration of 19 MiB of memory, an iteration count of 2, and 1 degree
of parallelism"), checked on 2026-11-20 against the cheat sheet's source in the OWASP
CheatSheetSeries repository (the cheat-sheet site itself couldn't be reached from the build
session). The parameters are stored in the PHC string with the hash
(`$argon2id$v=19$m=19456,t=2,p=1$…`): raise them in `hash::CURRENT` and each person's hash is
recomputed at their next correct sign-in (`password; rehashed` in the audit log).

The work runs on the blocking pool, never on the async executor, **two hashes at a time**; a
request waits at most 10 seconds for a slot, then `503 sign_in_busy`.

## Limits

| Limit | Value | Where |
| --- | --- | --- |
| Password length | 12 to 128 characters | tool server |
| Request body on the password routes | 4 KiB | tool server |
| Failures per account before a wait (passwords and second-step codes **counted together**) | 5 | tool server, per person |
| The wait | 30 s after the 5th failure, doubling with each further one (1, 2, 4, 8 min), **at most 15 min** from the 10th | tool server |
| An attempt during the wait | refused with the same answer, **not checked and not counted** | tool server |
| Failures forgotten | 24 hours after the last one | tool server |
| Back to zero | a full sign-in, a finished setup or reset, a password change | tool server |
| Setup-code guesses | the same schedule (5, then 30 s doubling to 15 min), a counter of their own | tool server, per person |
| All unauthenticated password routes together | 300 attempts in any rolling minute (`429 too_many_attempts`) | tool server, whole process |
| Hashing | 2 at a time; a 10-second wait at most (`503 sign_in_busy`) | tool server |
| Refused guesses per source address | 20 in any rolling 15 minutes; then every unauthenticated password route answers `429 too_many_attempts` with `Retry-After` | `pmos-web`, per process; at most 10,000 addresses tracked |
| Pending token (after the password) | 5 minutes, 3 tries, single use | tool server |
| Setup token | 15 minutes (never past the code), 5 tries at the confirmation | tool server |
| Setup or reset code | 100 bits, 24 hours, single use; a new one replaces the old | tool server |
| Recovery codes | 10, 80 bits each, single use | tool server |
| Authenticator | TOTP (RFC 6238): HMAC-SHA-1, 6 digits, 30-second steps, the step before and after accepted, each step once; a 160-bit secret | tool server |
| Pending sign-ins per person | 5 at once (the oldest go) | tool server |

The per-account delay is never a permanent lock: it ends on its own, and attempts during it
don't extend it. An attacker who knows a sign-in name can keep an account waiting by guessing
once at the end of each wait (one guess per 15 minutes); the owner can still sign in in between,
with Entra if that is on, and an admin can issue a reset code, which clears the counter. The
per-address limit and the global limit bound how fast anyone can guess at all.

**Behind a reverse proxy**, set `PMOS_WEB_TRUSTED_PROXIES` to the proxy's address: `pmos-web`
then counts each request by the right-most `X-Forwarded-For` address that isn't a trusted proxy
(an address the client wrote further left is not believed). Without it every request counts by
its peer, which behind a proxy is the proxy itself, so the per-address limit acts as one shared
limit for everyone.

## Threats

| Threat | How the design answers it |
| --- | --- |
| Guessing passwords online | Argon2id per guess; 5 failures per account then a growing wait to 15 min; 20 refused guesses per address per 15 min; 300 attempts a minute across the tool server; 2 hashes at a time. Guesses in parallel can't pass the threshold: a failure is counted before the hash is checked and taken back when the password was right (tested with 12 at once). |
| Telling which accounts exist | One answer, status and work at the password step and the setup's first step for every reason; a dummy hash where there is no real one; a waiting account answers the same. |
| A stolen password | Useless alone: the second step is required, always, with no switch to turn it off. |
| A replayed authenticator code | Each step is accepted once (the last accepted step is stored); a code older than the last accepted one is refused too. |
| Guessing authenticator codes | Only after a correct password, 3 tries per pending token, counted with password failures toward the same wait. |
| Stolen recovery codes | Single use, 80 bits, stored as SHA-256 bound to the person; regenerating replaces all ten. |
| A copy of the database | Argon2id hashes; sealed secrets that need the vault key and don't unseal for another person; SHA-256 of high-entropy codes and tokens only. |
| A stolen setup code | 24 hours, single use, needs the person's sign-in name; an Admin's or Auditor's must come from the host; a new code replaces it. The password-step limits apply to setup-code guesses too. |
| An admin taking over another admin | Web-issued codes never work for an Admin or Auditor (checked at issue and again at every setup step), so a compromised `pmos-web` or admin session can't reset an Admin's or Auditor's password. Nobody issues a code for themselves. |
| A compromised `pmos-web` | It can issue setup codes for non-protected people as any admin it signs for (as it can already change their roles), each recorded in `role_changes` with `client = web`. It never sees a hash, a secret or a stored code. |
| Another service's token | The password routes answer only `pmos-web`'s token. |
| Login CSRF and cross-site posts | Every password write needs the app's origin and a JSON body; the signed-in ones the session's CSRF token too; the step cookies are `SameSite=Strict`, `HttpOnly`, `__Host-`. |
| Session theft after a reset or change | The session carries the credential's epoch and ends when it moves (≤ 10 s; at once in the `pmos-web` that made the change, which also ends the person's other sessions). |
| A deactivated person | Can't sign in (the uniform refusal), can't finish a second step or a setup, and their sessions end within 10 seconds. At the end of the 24-hour grace their password credential is deleted with their linked accounts. |
| A lockout used to shut someone out | The wait is capped at 15 minutes and ends on its own; attempts during it aren't counted; an admin's reset clears it. |
| Secrets in logs or the audit log | No password, code, token, secret or sign-in name is logged or audited; details are fixed words and counts (`password::is_safe_detail`), tested by capturing the log and reading every audit event. |
| Exhausting the CPU through sign-in | Two hashes at a time, a 10-second wait, the global and per-address limits. |
| The `evals` account | Can never have a password: refused at issue, and by the tables' checks. |

## What is recorded

Audit events (`client` `web`, or `host` for a host-issued code; `task_id` `password`), metadata
only:

| `action` | `actor` | `outcome` | `target` | `detail` |
| --- | --- | --- | --- | --- |
| `password.sign_in` | `anonymous` | `ok` / `denied` | the person, or none for an unknown name | `password` (`; rehashed`) / `unknown_name`, `no_password`, `wrong_password`, `deactivated`, `locked`, `stale` |
| `password.second_step` | the person / `anonymous` | `ok` / `denied` | the person | `authenticator`, `recovery_code; recovery_left 9` / `wrong_code`, `locked`, `deactivated`, `expired`, `stale` |
| `password.lockout` | `anonymous` | `denied` | the person | `failures 5` (`; setup` for setup-code guesses) |
| `password.setup_code` | the admin / `host` | `ok` / `denied` | the person | `setup` or `reset` (`host_user <name>; …` from the host) / the refusal's code |
| `password.setup_begin` | `anonymous` | `ok` / `denied` | the person, or none | `begin; setup` / `unknown_name`, `wrong_code`, `expired`, `locked`, `deactivated`, … |
| `password.setup_complete` | the person / `anonymous` | `ok` / `denied` | the person | `setup` or `reset` / `wrong_code`, `locked`, `host_only`, … |
| `password.change` | the person | `ok` / `denied` | the person | `change` / `reauth_failed`, `locked` |
| `password.recovery_codes` | the person | `ok` / `denied` | the person | `regenerated` / `reauth_failed`, `locked` |
| `web.sign_in`, `web.sign_out` | the person | `ok` | none | `provider: password` (from `pmos-web`) |

`role_changes`: `issue_password_code` and `set_password`, each with detail `setup` or `reset`,
the actor, the client and, from the host, `host_user`. The usual retention applies: audit
events go at 90 days, `role_changes` rows 90 days after the person's deactivation. Pending
tokens and setup codes are deleted once past their expiry; a deactivated person's credential at
the end of the 24-hour grace.

## Configuration

| Variable | Service | What it does |
| --- | --- | --- |
| `PMOS_WEB_PASSWORD_LOGIN=1` | `pmos-web` | Turns password sign-in on (off by default); `GET /api/auth/providers` then lists `password`. Refuses to start without a `web:toolserver` signing key or with `PMOS_ASSERTION_MODE` other than `require`. |
| `PMOS_WEB_TRUSTED_PROXIES` | `pmos-web` | The reverse proxies whose `X-Forwarded-For` names the client, comma-separated addresses. |
| `PMOS_VAULT_KEY` | tool server | The vault's keys; password sign-in needs them to seal authenticator secrets. Required with live connectors; optional with fake ones. |
| `PMOS_DEV_PASSWORD_SETUP_FOR`, `PMOS_DEV_PASSWORD_SETUP_FILE` | tool server | **Development only**: at start-up, issue a setup code for that seeded user, as the host would, into a new 0600 file. Refused with a database, live connectors, no seed or no vault key. `scripts/claude-test-stack.sh` uses it for `PMOS_TEST_PASSWORD_LOGIN=1`. |

In the deploy kit, `deploy/compose.yaml` passes `PMOS_WEB_PASSWORD_LOGIN`,
`PMOS_WEB_TRUSTED_PROXIES` and `PMOS_ASSERTION_MODE` to the web app, and `deploy/pmos-admin up
--web` starts the web app with password sign-in as its provider too.
`pmos-admin users password-setup <handle>` issues a code on the host.

## The screens

Built in `web/` from `docs/web-api.md`: the sign-in card with the code step, the setup page
(`#/setup`, the QR code drawn in the browser), "Sign-in and security" (`#/security`: change
password, new recovery codes) and the Team page's setup or reset code action.

## Not built

- **Self-service reset by email**, by design: there is no email sending.
- **WebAuthn or passkeys**; only TOTP and recovery codes.
- **Breached-password lookups** (for example Have I Been Pwned): only the built-in list, so no
  password, or part of its hash, leaves PM OS.
- **Shared limits across several `pmos-web` or tool server processes**: the per-address and
  global limits are per process (one of each in the deploy kit). The per-account delay is in
  the database and holds across processes.
- **Not verified:** a real authenticator app scanning the URI (the codes are tested against
  RFC 6238's vectors and the URI against the Key Uri Format); a deployment behind a real reverse
  proxy.
