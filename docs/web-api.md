# Web app API: the contract between `web/` and `pmos-web`

The web app (`web/`, a TypeScript single-page app) talks only to `pmos-web` (`crates/web`), a
small backend that holds the session and calls the bot runtime and the tool server for the
signed-in PM. The browser never sees a service token, a connector token or another PM's data.
The product plan is `docs/web-client.md`.

Every path below is under `/api`. Bodies are JSON. Times are RFC 3339 in UTC.

## Sign-in and safety

- **Session:** a `__Host-pmos` cookie (`Secure`, `HttpOnly`, `SameSite=Lax`, `Path=/`), set by
  sign-in. Every route except the sign-in routes below answers `401 {"code":"signed_out"}`
  without one.
- **CSRF:** `GET /api/me` returns `csrf_token`. Every `POST`, `PUT` and `DELETE` sends it as
  `X-Pmos-Csrf`; a missing or wrong token is `403 {"code":"bad_csrf"}`. The Entra callback is
  a cross-site `GET` from Microsoft, so no origin check applies to it: its CSRF defence is
  `state`, matched once against a signed `__Host-pmos-entra` cookie set at `/api/auth/login`.
- **Origin:** every `POST`, `PUT` and `DELETE` (development sign-in and logout included) must
  carry an `Origin` (or else a `Referer`) that is the app's own origin, and no
  `Sec-Fetch-Site` other than `same-origin` or `none`; otherwise `403 {"code":"bad_origin"}`.
  **A write with neither `Origin` nor `Referer` is refused.** Browsers send `Origin` on every
  `fetch` with these methods, so the app needs to do nothing; a script or test that calls the
  API must send it. Both checks apply: origin, then the CSRF token.
- **Providers:** sign-in sits behind one interface. **Entra ID** (`docs/entra-sign-in.md`) is
  the production provider, on when `PMOS_WEB_ENTRA_TENANT_ID`, `PMOS_WEB_ENTRA_CLIENT_ID`,
  `PMOS_WEB_ENTRA_CLIENT_SECRET` and `PMOS_WEB_PUBLIC_URL` are all set. **Development
  sign-in** (`PMOS_WEB_DEV_LOGIN=1`) enables the development route below and logs a warning;
  it starts only on a loopback `PMOS_WEB_BIND` (`127.0.0.1`, `::1`). `pmos-web` refuses to
  start with neither provider, with Entra half-configured, or with both unless
  `PMOS_WEB_DEV_LOGIN_WITH_ENTRA=1` is also set (a development override; it then logs a loud
  warning). **Password sign-in** (`PMOS_WEB_PASSWORD_LOGIN=1`, **Password sign-in** below) is a
  third provider, off by default, on alone, with Entra, or beside development sign-in.
  `GET /api/auth/providers` tells the app which buttons to show.
- **Who you are:** the user database (`docs/user-database-spec.md`), which the tool server
  owns; `pmos-web` asks it with `GET /internal/users/resolve` (`docs/toolserver.md`, "The user
  database's read API") and keeps each answer 10 seconds. There is no team file (`team.yaml`
  was retired in slice U7). The
  signed-in person must be an **active** user; roles come from there and are looked up on
  every request (sessions hold no roles), so a deactivation or a removed role reaches the
  app within 10 seconds. **Entra ID** signs people in by the token's `(tid, oid)`; a person's
  first Microsoft sign-in links that key to them by the token's `preferred_username` only
  (never `email`), and only when an admin confirmed that sign-in name and they hold neither
  Admin nor Auditor (those are linked on the host). Guests, other identity providers and a
  registration without the `acct` claim are refused before any lookup. The rules, and what
  Microsoft's documentation says about each claim, are in `docs/entra-sign-in.md`. An Entra
  session is checked by its `(tid, oid)` on every request: if that key is unlinked, or now
  names someone else, it ends (`401 signed_out`); if the person holds Admin or Auditor and
  the host hasn't confirmed the link, it ends with `403 entra_needs_host`. **Development
  sign-in** resolves by handle. **Password sign-in** resolves by handle too, and its session
  also holds the password credential's epoch: a reset or a password change moves it and the
  session ends (`401 signed_out`).
- **When the tool server can't be reached** (spec §2, "When the store is unavailable"):
  sign-in fails closed (`503 users_unavailable`, or the callback's
  `reason=users_unavailable`). An existing session may keep using the person's
  last-known-good answer **for at most 5 minutes, for `GET /api/me` and `GET /api/brief`
  only** (pmos-web's own store); the brief then leaves out every item whose `system` is
  `slack` and says so (`slack_withheld: true`, see **The daily brief and follow-ups**).
  Every other route, every write, and anything that reaches the tool server or the runtime
  answers `503 users_unavailable`. Past the 5 minutes, reads do too. **A password session
  never uses the last-known-good answer**: its credential's epoch must be checked on every
  request, so it answers `503 users_unavailable` at once (a reset can't be outlived while the
  tool server is down).
- **HTTPS:** with an `https` `PMOS_WEB_PUBLIC_URL`, every response carries
  `Strict-Transport-Security: max-age=31536000`.
- **Health:** `GET /healthz` (outside `/api`, no session) answers `ok` while the process runs.
  `GET /readyz` answers `200` when it can serve and `503` when it can't, with
  `{"ready": bool, "checks": {"store": "ok"|"down", "bot_runtime": …, "tool_server": …},
  "unrecorded_sign_in_events": n, "untrusted_proxy_refusals": n, "warnings": [...]}`: its own store answers, and the bot runtime and the tool
  server (which answers who may sign in) answer their `/healthz` within two seconds. With
  Entra sign-in on, `checks` also has `entra_link` (since the review of `b469d5b`, U1): `ok`
  only when the tool server validates first-link ID tokens for the same tenant and client id
  (`docs/entra-sign-in.md`); without that a first Microsoft sign-in can't link, so it is
  `down` and the server is not ready. `unrecorded_sign_in_events` counts, since start-up, sign-in events the audit log didn't take
  (see **Sign-in events**); it never makes the server unready. `untrusted_proxy_refusals`
  counts, since start-up, password requests refused `403 proxy_not_trusted` because the proxy
  setting doesn't match where they came from (`docs/password-sign-in.md`); above zero, the
  setting is wrong or stale; it never makes the server unready either, but while the last such
  refusal is under 15 minutes old `warnings` holds `"untrusted_proxy_refusals"` (the gate
  review of `04c175f`, GL3), which `pmos-admin status` shows as `WARN`. No detail and no
  secrets; the reason goes to the server's log.
- **Sign-in events.** Every sign-in, sign-out and refused sign-in is recorded in the tool
  server's audit log (`POST /internal/admin/events`, `docs/toolserver.md`, **Sign-in
  events**), and shows in the admin audit list with the usual fields:

  | `action` | `actor` | `outcome` | `target` | `detail` |
  | --- | --- | --- | --- | --- |
  | `web.sign_in` | the member | `ok` | `null` | `provider: entra` (or `password`, `dev`) |
  | `web.sign_out` | the member | `ok` | `null` | `provider: entra` (or `password`, `dev`) |
  | `web.sign_in_refused` | `anonymous` | `denied` | `null` | `provider: entra; reason: not_a_member; attempt: 3f9a1c2b4d5e` |
  | `users.identity_conflict` | `anonymous` | `denied` | the user whose sign-in name was presented | `entra` |

  `client` is `web` and `task_id` `web-auth`. `reason` is the signed-out page's code
  (`not_a_member`, `deactivated`, `sign_in_unconfirmed`, `entra_needs_host`,
  `entra_misconfigured`, `users_unavailable`, `failed`, `unavailable`, …). `attempt` (on
  `not_a_member` only) is the first 12 hex characters of an HMAC-SHA256, under a key derived
  from `pmos-web`'s tool server token, of the name tried (trimmed, lowercased): repeated
  attempts with one name share it, and the name itself (an email or handle) is never sent or
  stored. No IP address (PM OS records none), no token, no `oid`. `users.identity_conflict`
  is `pmos-web` flagging a known Microsoft account that signed in with another user's
  sign-in name (it signs in as itself; spec §7 step 3); a first sign-in whose name is
  already linked to a different account is refused (`not_a_member`), and the tool server's
  first-link route records that conflict itself. Both count on the admin overview. An Entra
  refusal is recorded only once
  the browser's `state` matched a sign-in this server started; a callback with no flow behind
  it (which anyone can send) is only logged. **If the event can't be recorded, sign-in and
  sign-out go ahead** (after at most 3 seconds): the failure is logged as an error and counted
  in `/readyz`. Sign-out is never blocked; sign-in isn't because a session alone reaches
  nothing: everything a signed-in person does goes through the tool server or the runtime,
  which audit each call themselves, so a missing sign-in event hides no action, while failing
  closed would lock everyone out (the admins included) whenever the tool server blips.
- **Old links:** `GET /drafts/{id}` and `GET /d/{id}` (outside `/api`) answer `303` to the
  app's own route for that draft, `/#/d/{id}`, when `id` is shaped like an approval id (`apr_`
  and 1 to 64 letters or digits), and `303` to `/` otherwise. They are for links Slack cards
  posted before the cards used `/#/d/{id}`; the redirect is always a same-origin path.
- **Errors:** `{"code": "<snake_case>", "error": "<plain sentence for the person>"}`.
  `404 not_found` is also the answer for someone else's draft or task, so the app never
  confirms that it exists. Any unknown path under `/api` is a JSON `404 not_found`, never
  the app's `index.html`; a known path with a method it doesn't take is
  `405 method_not_allowed`.
- **The app's icon** is a file, `/favicon.svg` (or `/favicon.ico`) in the built app's
  directory, linked with `<link rel="icon" href="/favicon.svg" type="image/svg+xml">`: the
  content security policy allows no `data:` images. It is served as `image/svg+xml` with a
  week's cache. A missing file with an asset extension is a `404`, not the app's page.

| Method and path | Body | Answer |
| --- | --- | --- |
| `GET /api/auth/providers` | | `{providers: ["entra", "password", "dev"]}`: the providers that are on, in that order. No session needed. |
| `GET /api/auth/login[?return_to=/path]` | | `303` to Microsoft's authorize endpoint, setting the `__Host-pmos-entra` flow cookie (ten minutes). `return_to` is kept only if it is a same-origin path (`/…`, not `//…`); else `/`. `404` unless Entra is on. If Microsoft's metadata can't be fetched: `303` to `/#/signed-out?reason=unavailable`. |
| `GET /api/auth/callback?code&state` | | Microsoft's redirect back. `303` to the `return_to` path (default `/`) with the session cookie, or `303` to `/#/signed-out?reason=<reason>` with no session. Always clears the flow cookie. `404` unless Entra is on. |
| `POST /api/auth/dev-login` | `{handle}` | `204` and the cookie. `404` unless development sign-in is on. `400 bad_request` for anything but a handle. `403 not_a_member` for a handle that isn't an active user in the user database. `503 users_unavailable` when the tool server can't be asked. |
| `POST /api/auth/logout` | none | `204`; the session ends. Only PM OS's session: Microsoft's own sign-in session is untouched, so "Sign in with Microsoft" may then sign straight back in without a prompt. |

The callback's `reason`s, for the app's signed-out page:

| `reason` | Meaning | Suggested text |
| --- | --- | --- |
| `not_a_member` | Microsoft signed them in, but they aren't a user: no user has that Microsoft account or (for a first sign-in) that confirmed sign-in name; a guest of the tenant; another identity provider; or a sign-in name already linked to a different Microsoft account (audited and flagged). | "You're not on the team list yet. Ask a platform owner to add you." |
| `deactivated` | Their user is deactivated. | "Your access to PM OS is turned off. A platform owner can turn it back on." |
| `sign_in_unconfirmed` | A first sign-in whose sign-in name came through Slack and no admin has confirmed it yet. | "An admin needs to confirm your Microsoft sign-in name in PM OS first." |
| `entra_needs_host` | They hold Admin or Auditor, and the host hasn't set or confirmed their Microsoft account (`pmos-admin users set-entra`). | "Someone with host access needs to confirm your Microsoft account." |
| `entra_misconfigured` | The ID token has no `acct` claim: the app registration lacks the optional claim (`docs/entra-sign-in.md`). Nobody can sign in until it is added. | "Microsoft sign-in isn't set up quite right yet. Tell a platform owner." |
| `users_unavailable` | The tool server (the user database) couldn't be asked. | "PM OS can't check who you are right now. Try again in a minute." |
| `cancelled` | They cancelled at Microsoft (`access_denied`). | "Sign-in was cancelled." |
| `expired` | No sign-in in progress in this browser, or it took over ten minutes. | "That sign-in expired. Try again." |
| `unavailable` | Microsoft couldn't be reached. | "Microsoft sign-in isn't answering. Try again in a minute." |
| `wrong_account` | From before the user database (`team.yaml`'s `entra-oid`); no longer sent. An app may keep the text. | "That Microsoft account isn't the one on the team list. Sign in with your own work account, or ask a platform owner." |
| `failed` | Anything else (a wrong or reused `state`, a refused code, a token that failed a check). | "Sign-in didn't work. Try again." |

An unknown `reason` should read as `failed`.

## Password sign-in

Email-and-password sign-in with a **required** authenticator-app code, on when `pmos-web` runs
with `PMOS_WEB_PASSWORD_LOGIN=1` (`GET /api/auth/providers` then lists `password`). The design,
every limit and the threats are in `docs/password-sign-in.md`. **The tool server decides every
step**; `pmos-web` holds no password, hash or secret. When password sign-in is off, every route
here is `404 not_found`.

**There is no way to sign in with a password alone.** A correct password gives a short-lived
pending sign-in for the second step only (an authenticator code, or a recovery code), and only
the second step makes a session. There is no email: a person gets a password with a one-time
**setup code** an admin makes for them on the Team page and passes on; a forgotten password or
a lost phone is a new code from an admin (a **reset**).

### The flows, step by step

**Set up (or reset)**, with a setup code:

1. `POST /api/auth/password/setup/begin` `{sign_in_name, setup_code}` → `200 SetupStarted`, and
   the `__Host-pmos-setup` cookie.
2. `POST /api/auth/password/setup/password` `{password}` → `204`. A refused password is
   `422 weak_password` with `reason`; send another.
3. `POST /api/auth/password/setup/authenticator` `{}` → `200 Enrolment`. Show the QR code of
   `otpauth_uri` (drawn by the app; the server sends no image) and `secret` as text for typing
   in. Calling it again makes a **new** secret (say so: "scan the new code"); only the last one
   counts.
4. `POST /api/auth/password/setup/confirm` `{code}` (6 digits from the app) → `200 SetupDone`:
   the ten recovery codes, **shown once**, and the session cookie (signed in). The setup cookie
   is cleared. Every other session the person had ends (a reset signs them out everywhere).

Steps 2 and 3 must come in that order before 4 (`409 wrong_step` otherwise); step 2 may be
repeated (the last password wins) up to 3 times and step 3 up to 5 times in one setup. The
setup lasts 15 minutes from step 1 (never past the code's own expiry); after that, after 5
wrong codes at step 4, or past those repeats, `401 setup_expired`: start at step 1 again with
the same code (it is spent when step 4 succeeds). One code starts at most 5 setups and sets at
most 6 passwords across them; past that `409 setup_code_spent`: an admin makes a new code.
Every call of steps 2 and 3 counts against the browser's address (20 in 15 minutes), even when
it succeeds.

**Sign in:**

1. `POST /api/auth/password/sign-in` `{sign_in_name, password}` → `200 SignInStarted` and the
   `__Host-pmos-pending` cookie. No session yet.
2. `POST /api/auth/password/second-step` `{code}` **or** `{recovery_code}` (exactly one) →
   `200 SignedIn` and the session cookie; the pending cookie is cleared. `401 code_failed` with
   `attempts_left`: try again (3 tries in all); `401 sign_in_expired`: start at step 1.

After step 2 the app continues as after any sign-in: `GET /api/me` for the CSRF token.

**Signed in:** change the password, make new recovery codes, read one's own status, dismiss a
reset notice (below).

**A reset is never silent.** When an admin (or someone with host access) makes a reset code for
someone who already has a password, their `PasswordStatus.reset_notice` names who and when from
that moment, through the reset, until they dismiss it
(`POST /api/auth/password/reset-notice/dismiss`). Show it on every page from their next sign-in
and on the security page, plainly: "Your sign-in was reset by <name> on <date>. If this was not
you, tell an admin." Their old password keeps working until the code is used.

### Routes

| Method and path | Body | Session | Answer |
| --- | --- | --- | --- |
| `POST /api/auth/password/sign-in` | `{sign_in_name: string, password: string}` | none | `200 SignInStarted`; sets `__Host-pmos-pending` |
| `POST /api/auth/password/second-step` | `{code: string}` or `{recovery_code: string}` | none; the pending cookie | `200 SignedIn`; sets `__Host-pmos`, clears `__Host-pmos-pending` |
| `POST /api/auth/password/setup/begin` | `{sign_in_name: string, setup_code: string}` | none | `200 SetupStarted`; sets `__Host-pmos-setup` |
| `POST /api/auth/password/setup/password` | `{password: string}` | none; the setup cookie | `204` |
| `POST /api/auth/password/setup/authenticator` | `{}` | none; the setup cookie | `200 Enrolment` |
| `POST /api/auth/password/setup/confirm` | `{code: string}` | none; the setup cookie | `200 SetupDone`; sets `__Host-pmos`, clears `__Host-pmos-setup` |
| `POST /api/auth/password/change` | `{current_password: string, code: string, new_password: string}` | yes, with CSRF | `204`; sets a new `__Host-pmos` (this browser stays signed in, every other session ends) |
| `POST /api/auth/password/recovery-codes` | `{code: string}` | yes, with CSRF | `200 {recovery_codes: string[10]}`, shown once; the old ones stop working |
| `GET /api/auth/password/status` | | yes | `200 PasswordStatus` |
| `POST /api/auth/password/reset-notice/dismiss` | `{}` | yes, with CSRF | `204`: the reset notice goes |
| `POST /api/admin/users/{handle}/password-setup-code` | none or `{}` | yes, with CSRF; `manage-members` | `201 SetupCode`, shown once to the admin |

Every `POST` needs the app's own `Origin` and a JSON body (`Content-Type: application/json`;
send `{}` where there is nothing to say), the sign-in routes included (`403 bad_origin`,
`415 bad_content_type`). The signed-in ones also need `X-Pmos-Csrf`. A field a route doesn't
take, or a missing one, is `400 bad_request`; a body over 4 KiB is `413`. Every answer is
`Cache-Control: no-store`. With no trusted proxy (`PMOS_WEB_DIRECT=1`), a request to the
unauthenticated routes that carries `X-Forwarded-For`, `Forwarded` or `X-Real-IP` is
`403 proxy_not_trusted`.

**Cookies** (set and read by the server; the app never sees their values): `__Host-pmos-pending`
(5 minutes) and `__Host-pmos-setup` (15 minutes), both `Path=/; Secure; HttpOnly;
SameSite=Strict`. A refusal that ends the step (`sign_in_expired`, `setup_expired`, the last
try used, `not_a_member`, …) clears its cookie. The session cookie is the same `__Host-pmos` as
every sign-in's.

### Bodies

```json
// SignInStarted
{"next": "second_step", "expires_at": "2026-11-20T09:05:00Z"}

// SignedIn
{"recovery_code_used": false, "recovery_codes_left": 10}

// SetupStarted
{"handle": "ana", "kind": "setup", "expires_at": "2026-11-20T09:15:00Z",
 "password": {"min_length": 12, "max_length": 128}}

// Enrolment (shown once)
{"otpauth_uri": "otpauth://totp/PM%20OS:ana@example.com?secret=JBSWY3DPEHPK3PXP…&issuer=PM%20OS&algorithm=SHA1&digits=6&period=30",
 "secret": "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP", "account": "ana@example.com",
 "issuer": "PM OS", "algorithm": "SHA1", "digits": 6, "period": 30}

// SetupDone (the codes shown once)
{"kind": "setup", "recovery_codes": ["abcd-efgh-ijkl-mnop", "…ten in all…"]}

// PasswordStatus
{"enabled": true, "password_set": true, "second_step_enrolled": true, "recovery_codes_left": 9,
 "needs_host_setup": false,
 "reset_notice": {"by": "ada", "by_name": "Ada Lovelace", "at": "2026-11-20T08:00:00Z"}}

// SetupCode (shown once, to the admin)
{"setup_code": "ABCDE-FGHIJ-KLMNO-PQRST", "handle": "ana", "kind": "reset",
 "expires_at": "2026-11-21T09:00:00Z"}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `SignInStarted.next` | `"second_step"` | Always: show the code form. |
| `…expires_at` | string, RFC 3339 | When this step's token ends (sign-in: 5 minutes; setup: 15 minutes, or the code's own expiry if sooner; a setup code: 24 hours). |
| `SignedIn.recovery_code_used` | boolean | They signed in with a recovery code: suggest making new ones, and say how many are left. |
| `SignedIn.recovery_codes_left` | integer, 0–10 | Unused recovery codes now. At 0, warn that a lost phone will need an admin. |
| `SetupStarted.handle` | string | Whose setup this is (show "Setting up as ana"). |
| `SetupStarted.kind` | `"setup"` or `"reset"` | A first password, or replacing one ("This replaces your password, authenticator and recovery codes, and signs you out everywhere"). |
| `SetupStarted.password` | object | The length rules to state: `min_length` 12, `max_length` 128. |
| `Enrolment.otpauth_uri` | string | Draw as a QR code. **A secret**: never log it or keep it after the step. |
| `Enrolment.secret` | string | The same secret as base32 text, for typing into the app. **A secret.** |
| `Enrolment.account`, `issuer` | string | What the authenticator app shows (the sign-in name, `PM OS`). |
| `Enrolment.algorithm`, `digits`, `period` | `"SHA1"`, `6`, `30` | For apps that ask. |
| `SetupDone.recovery_codes` | string[10] | `xxxx-xxxx-xxxx-xxxx`, lower-case. **Shown once**: offer copy and download; say each works once. |
| `PasswordStatus.enabled` | boolean | The tool server has password sign-in on. |
| `PasswordStatus.password_set`, `second_step_enrolled` | boolean | They have a password and an authenticator (always both, or neither). |
| `PasswordStatus.recovery_codes_left` | integer, 0–10 | Unused recovery codes. |
| `PasswordStatus.needs_host_setup` | boolean | They hold Admin or Auditor and their password came from a code an admin made in the web app: it doesn't work (`password_set` is then false) until someone with host access makes them a setup code. |
| `PasswordStatus.reset_notice` | object or `null` | A reset someone made for them that they haven't dismissed: `by` (an admin's handle, or `host` for someone with host access), `by_name` (the admin's display name or `null`; plain text), `at` (RFC 3339). |
| `SetupCode.setup_code` | string | `XXXXX-XXXXX-XXXXX-XXXXX`, 20 letters and digits. **Shown once** to the admin, who passes it on (it is never sent by PM OS). Spaces, hyphens and case don't matter when typed. |
| `SetupCode.kind` | `"setup"` or `"reset"` | `reset` when the person already has a password: using it replaces everything and signs them out everywhere. |

A `code` is the 6 digits the app shows (spaces are ignored). A `recovery_code` is one of the
ten (case, spaces and hyphens ignored).

### What a screen should say

- **Passwords:** 12 to 128 characters, any characters (spaces and symbols welcome), no
  composition rules; not your handle or sign-in name, and not a common password. On
  `422 weak_password`, word `reason`: `too_short`, `too_long`, `matches_name`, `common`.
- **A wrong password, name or setup code** never says which was wrong, nor whether the account
  exists, has a password, is turned off or is waiting (`401 sign_in_failed`,
  `401 setup_failed`). After **5** failures (passwords and codes together) from one address,
  that address waits 30 seconds for that account, then longer with each further failure, at
  most **15 minutes**; the wait ends on its own, and the person can still sign in from their own
  address meanwhile. Say "wait a few minutes" rather than a number.
- **Codes:** 3 tries per sign-in (`attempts_left`), 5 at the setup's confirmation. Each code
  works once; if one was just used, wait for the next.
- **Too many tries from one place:** `429 too_many_attempts` with `Retry-After` (seconds): 20
  refused attempts from one address (an IPv6 address counts by its /64) in 15 minutes, or the
  server's own limit. **Every** refused attempt counts: a wrong password or code, an unknown or
  expired sign-in or setup, a step out of order, a body that isn't the route's JSON; only PM OS's
  own failures (`5xx`) don't. The tool server has limits of its own behind the same answer
  (since the gate review of `04c175f`): a share of its hashing allowance per network (an IPv4
  /24, an IPv6 /48), and 100 wrong setup codes per network in 15 minutes. A `429` or `503` from
  them never costs a setup code or a setup token one of its tries: try again after
  `Retry-After`.
- **Setup codes** last 24 hours and work once; a new one replaces the old one. Wrong guesses
  by anyone never void a code or end a setup in progress (since the gate review of `04c175f`).
- **Recovery codes:** ten, each works once; making new ones replaces all ten.

### Errors on these routes

Each is in **Error codes**; `error` is `pmos-web`'s own sentence.

| Route | Refusals |
| --- | --- |
| `sign-in` | `401 sign_in_failed` (one answer for every reason), `429 too_many_attempts`, `503 sign_in_busy`, `503 users_unavailable`, `501 not_configured`, `503 assertion_required` |
| `second-step` | `401 code_failed` (`attempts_left`), `401 sign_in_expired`, `403 not_a_member` (deactivated since the password), `429 too_many_attempts` |
| `setup/begin` | `401 setup_failed` (one answer for every reason until the code matched), then `403 not_a_member`, `409 sign_in_unconfirmed`, `403 host_only` (an Admin's or Auditor's code must come from the host), `409 evals_account`, `409 setup_code_spent`; `429 too_many_attempts` |
| `setup/password` | `401 setup_expired`, `409 setup_code_spent`, `422 weak_password` (`reason`), `403 not_a_member`, `429 too_many_attempts`, `503 sign_in_busy` |
| `setup/authenticator` | `401 setup_expired`, `409 wrong_step`, `429 too_many_attempts` |
| `setup/confirm` | `401 code_failed` (`attempts_left`), `401 setup_expired`, `409 wrong_step`, `403 host_only`, `409 sign_in_unconfirmed`, `429 too_many_attempts` |
| `change` | `403 reauth_failed` (the current password or the code), `422 weak_password`, `409 no_password`, `429 too_many_attempts`, `502 unknown_outcome` |
| `recovery-codes` | `403 reauth_failed`, `409 no_password`, `429 too_many_attempts`, `502 unknown_outcome` |
| `password-setup-code` | `403 forbidden` (not an active admin), `403 self_change`, `403 host_only` (an Admin or Auditor: `pmos-admin users password-setup`), `404 not_found`, `409 not_active`, `409 sign_in_unconfirmed` (no confirmed sign-in name), `409 evals_account`, `501 not_configured`, `503 assertion_required` |

Every route can also answer `400 bad_request`, `403 bad_origin`, `415 bad_content_type`,
`502 upstream_unavailable`, `503 unavailable`; the signed-in ones `401 signed_out` and
`403 bad_csrf`.

### The Team page

Each `User` in `GET /api/admin/users` (**Admin: users and roles**) carries three booleans,
for an admin's and an auditor's view alike, and never anything secret:
`password_set`, `second_step_enrolled` (always equal to `password_set` once a setup finished)
and `setup_code_pending` (a setup or reset code was made, not yet used, not expired). Offer
"Make a setup code" (or "Make a reset code" when `password_set`) for an active person with a
confirmed sign-in name; for someone with `host_only` say it takes `pmos-admin users
password-setup <handle>` instead; never for oneself. A fourth, `password_needs_host_setup`,
is true when the person holds Admin or Auditor and their password came from a code made in the
web app: it no longer works (`password_set` is then false) and their password sessions have
ended. A role change's answer carries it, so the page can say at once "They will need a
host-issued setup code".

## Me

| Method and path | Body | Answer |
| --- | --- | --- |
| `GET /api/me` | | `Me` |
| `PUT /api/me/settings` | `{nudges?: bool, mascot?: bool, ask_persona?: key}` | `Me`. `ask_persona` (the picker's bot, **Bots**) needs `ask-bot` (`403 not_allowed`), a key (`400 bad_request`), and a bot on the PM's team (`409 bot_not_on_team`) that is still offered (`409 bot_unavailable`); `"nudge"` always works. The team is read from the tool server for it (`503 unavailable`, `502 upstream_unavailable` when it can't be). |
| `POST /api/me/hints` | `{seen: "sources" \| "ask" \| "approve" \| "tour_started" \| "tour_asked" \| "tour_opened" \| "tour_decided" \| "tour_skipped" \| "tour_cheered" \| "team_picked" \| "team_skipped" \| "team_announced"}` | `Me` |
| `DELETE /api/me/hints` | | `Me`, with `hints_seen` empty ("Show hints again"; it restarts the guided start too) |

```json
{"handle": "ana", "display_name": "Ana Silva", "name": "Ana Silva", "roles": ["pm"],
 "permissions": ["ask-bot", "view-own-tasks", "manage-own-routines", "decide-own-approval",
                 "view-own-traces", "link-own-accounts"],
 "reviewer": false,
 "csrf_token": "…", "first_visit": true, "hints_seen": ["sources"], "nudges": true, "mascot": true,
 "team": ["nudge", "chief"], "ask_persona": "chief", "team_unavailable": false,
 "show_team_step": false, "show_team_note": false}
```

`reviewer` is true when the person holds the `reviewer` role: show the Reviews entry
(**Reviews**). It is the role, not the `recommend-on-approval` permission, which an admin
holds too without reviewing anyone. An older server leaves it out: treat that as `false`.

`first_visit` is true until the PM first opens a draft. `nudges: false` also hides hints and
the guided start.

`hints_seen` also carries the guided start's marks, which the app writes and reads; the server
only keeps them. `tour_started` means the guided start is on (written when a first visit shows
the welcome card, and after "Show hints again"); `tour_asked`, `tour_opened` and
`tour_decided` are its three steps (ask for something; open a draft; approve, reject or change
one); `tour_skipped` ends it early; `tour_cheered` records the one "Approved!" moment.

**The team fields** (the bot marketplace, **Bots**). An older server leaves them out: treat
that as `team: ["nudge"]`, `ask_persona: "nudge"`, and both `show_…` false.

| Field | Type | Meaning |
| --- | --- | --- |
| `team` | [string] | The keys of the bots on the PM's team that are offered now, `"nudge"` first, then in catalog order. `[]` for someone without `ask-bot` (an Auditor-only person). `["nudge"]` while `team_unavailable`. A withdrawn bot still on the team is not listed here (it is in `GET /api/bots`'s `withdrawn`). |
| `ask_persona` | string or null | **The picker's default**: the bot the PM last asked (kept by `POST /api/tasks` and `PUT /api/me/settings`) when it is in `team`; otherwise `"nudge"`, so a bot removed from the team, or withdrawn, falls back to Nudge. `null` without `ask-bot`. `"nudge"` while `team_unavailable`. |
| `team_unavailable` | bool | The team or the catalog couldn't be read from the tool server just now (or `/api/me` was served on the last-known-good answer, **Sign-in and safety**). Then `team` is `["nudge"]`, `ask_persona` `"nudge"`, both `show_…` false; the app should offer only Nudge and may say it can't show the team. `false` without `ask-bot`. |
| `show_team_step` | bool | Route Today to the "Pick your team" step (`/start/team`) once. Exactly when **all** of: the person holds `ask-bot` (never an Auditor-only person; a Reviewer or Admin who may ask does see it); `team_unavailable` is false; the catalog offers at least one bot besides Nudge; neither `team_picked` nor `team_skipped` is in `hints_seen`; and **the guided start is on**, worked out as the app's `tourState(me).active` does: `nudges` is true, and `tour_skipped` is not in `hints_seen`, and at least one of `first_visit`, `tour_started`, `tour_asked`, `tour_opened`, `tour_decided` holds, and not all three of `tour_asked`, `tour_opened` and `tour_decided` are in `hints_seen`. So a returning person (no `first_visit`, no guided start marks) never gets the step. |
| `show_team_note` | bool | Show the one dismissible "New: you can add Chief, Sprout and Echo to your team" note on Today (with "Pick your team" to the Bots page, and a dismiss that marks `team_announced`). Exactly when: the person holds `ask-bot`; `team_unavailable` is false; the catalog offers at least one bot besides Nudge; none of `team_picked`, `team_skipped`, `team_announced` is in `hints_seen`; `show_team_step` is false (the guided start is off, finished or skipped); and `team` is `["nudge"]` (nobody who already added a bot is told they can). Never together with `show_team_step`. |

The app marks `team_picked` after a successful "Continue" (`PUT /api/me/team` with `via:
"onboarding"`), `team_skipped` for "Skip for now" (and nothing else), and `team_announced` when the
note is dismissed. `DELETE /api/me/hints` clears them with the rest, so "Show hints again" shows
the step again (with the guided start).

`mascot` is "Show Nudge": whether the app draws its mascot. `false` hides the character
everywhere and nothing else: hints, empty states and the guided start keep their text. It
defaults to `true`; an older server leaves it out, which means `true`.

`display_name` is the person's display name in the user database (`""` when none is set),
**untrusted text**: render it as plain text, never as HTML or Markdown. `name` is what to
call them: the display name, or the handle when there is none. `roles` and `display_name`
come from the user database for this request (at most 10 seconds old; on the
last-known-good path, see **Sign-in and safety**, at most 5 minutes old).

`permissions` lists what the person's roles allow (the permission matrix,
`crates/contract/src/permissions.rs`), kebab-case, in a fixed order; an unknown role adds
nothing. It is for showing and hiding parts of the app; the server checks every request
itself. Show the Admin area when it contains `view-audit-log` or `manage-connectors` (see
**Admin and auditor**); show the policy screens with `view-policies` and their propose
buttons with `edit-policies` (**Admin: policy**); show the content and harness screens with
`view-content` and their propose buttons with `edit-content` (**Admin: content and
harness**). An older server leaves `permissions` out: treat that as `[]`. An auditor's is
`["view-audit-log", "export-audit", "view-content", "view-policies"]`; an admin's has all 21.

## Drafts

A draft is a pending write (a Confluence page, a Jira issue, a Slack reply or message) that a
bot prepared and the PM has not decided yet: the tool server's approval item, shown as a
document.

| Method and path | Body | Answer |
| --- | --- | --- |
| `GET /api/drafts` | | `{drafts: [DraftSummary], truncated, total_pending?}`, newest first; every pending one, then the 20 most recently decided. `truncated` is `true` only if the tool server left pending drafts out past its safety cap (1,000), and `total_pending` then says how many there are |
| `GET /api/drafts/{id}` | | `Draft` |
| `PUT /api/drafts/{id}` | `{title?, body}` | `Draft`, with `edited: true` and a new `version`. Saves the PM's edit **on the draft itself, at the tool server**: the Slack card and the console show it from then on, and approving anywhere (here, the Slack card, the console) writes it. Nothing is sent to the target system. A `title` is kept only for drafts that have one (Confluence pages, Jira issues); without one the saved title stays. Citation markers in `title` or `body` are removed before it is saved (see **Citations**). `409 not_pending` once decided, `409 expired` for an expired Slack draft, `422 bad_edit` if the tool server refuses the edited text for its target. A practice draft's edit stays in `pmos-web`. |
| `POST /api/drafts/{id}/changes` | `{instruction}` | `202 {suggestion: Suggestion}`. Asks the bot to revise the draft. One at a time: `409 change_in_progress`. |
| `GET /api/drafts/{id}/suggestion` | | `{suggestion: Suggestion \| null}` |
| `POST /api/drafts/{id}/suggestion/accept` | | `Draft`, with the suggested text as its body, saved as an edit is (`PUT` above): at the tool server, so every surface shows it. The save names the revision task, so the tool server raises the draft's sensitivity to what that task read (it never lowers it): a revision that read a restricted document can't then be posted to a channel with people outside the team (`409 audience_blocked`). The revision task itself starts at the draft's own level |
| `POST /api/drafts/{id}/suggestion/reject` | | `Draft`, unchanged |
| `POST /api/drafts/{id}/approve` | `{confirm_external?: bool, version?: string}` | `Draft` with `status: "approved"` and `result_url`. Refusals (nothing was sent): `409 needs_external_confirm`, `409 audience_blocked`, `409 expired`, `409 not_pending`, `409 stale`, `409 not_approvable`, `409 held`, `422 bad_edit`, `503 audience_unavailable`, `503 unavailable`, `403 not_a_member`. **Outcome unknown:** `502` or `504 unknown_outcome` (see **Error codes**). |
| `POST /api/drafts/{id}/reject` | `{reason?}` | `Draft` with `status: "rejected"`. `reason` is one of `wrong_facts`, `wrong_target`, `tone`, `incomplete`, `should_not_draft`, `other`. The same refusals and `unknown_outcome` as approve, where they apply. |
| `POST /api/drafts/practice` | | `Draft`, a practice draft for this PM. Idempotent while one exists. |
| `DELETE /api/drafts/{id}` | | `204`, practice drafts only; `409 not_practice` otherwise. |

**Held drafts** (slice A1; `docs/bot-marketplace-spec.md` §6, "Removal"). A pending draft
whose bot was withdrawn from the catalog (or deleted from it), and that no Admin released, is
**on hold**: `held: true` on its `DraftSummary` (and so on its `Draft`), `false` on every
other draft. Its owner may still reject it. Approving it, saving an edit (`PUT`), asking for a
change (`POST …/changes`) and accepting a suggestion are refused by `pmos-web` itself with `409
held` before anything is sent; a draft whose bot is withdrawn between the read and the decision
gets the tool server's own refusal, answered as the same `409 held`. It is decidable again once
the bot is offered again or an Admin releases it (**Admin: bots**).

```json
// DraftSummary
{"id": "apr_…", "kind": "confluence_page", "title": "PRD: TCB grace period setting",
 "bot": "pm-ana", "status": "pending", "created_at": "…", "expires_at": null,
 "task_id": "tsk_…", "practice": false, "persona": "chief", "persona_offered": true,
 "held": false}

// Draft = DraftSummary plus
{"body": "markdown", "edited": false, "edited_at": null,
 "target": {"system": "confluence", "label": "Space PM, under “Specs”", "url": null},
 "sources": [Source], "result_url": null, "external": false, "version": "7",
 "restricted_sources": true, "audience": null,
 "marker": "Drafted with PM OS · run tsk_01JD…",
 "recommendations": [Recommendation]}

// Recommendation: a reviewer's advice on this draft (Reviews)
{"reviewer": "rex", "reviewer_name": "Rex Ito", "verdict": "approve",
 "note": "Looks right to me." | null, "note_removed": false,
 "created_at": "2026-11-16T10:00:00Z", "stale": false}

// A Slack draft's audience, once PM OS has checked the channel (both null before)
{"audience": {"members": 14, "external": true}}

// Source
{"id": "s1", "system": "jira", "title": "PAY-212: TCB grace period", "url": "https://… or null",
 "excerpt": null, "cited": true}

// Suggestion
{"status": "working", "instruction": "shorten the risks section", "body": null, "error": null}
```

- `kind` is `confluence_page`, `jira_issue`, `slack_reply` or `slack_message`.
- `status` is `pending`, `approved`, `rejected`, `expired` or `failed`.
- **`persona` and `persona_offered`: which bot drafted it** (**Bots**, "Attribution"), on every
  `DraftSummary` and so on every `Draft` (the list, a draft, and every answer that returns a
  draft: an edit, a suggestion accepted or rejected, an approve, a reject, a practice draft).
  `persona` is the bot's key, or `null` for Nudge (and for a practice draft, and every draft made
  before the marketplace). `persona_offered` is `true` for `null` and for a key the catalog offers
  now; `false` for a key of a bot that was withdrawn or deleted, and for any key while the catalog
  can't be read (fail closed: the app never claims a bot it can't check). The tool server copies
  `persona` from the drafting session and nothing changes it.
- `Suggestion.status` is `working`, `ready` or `failed`. `body` is set when `ready`, `error`
  (a plain sentence) when `failed`.
- `expires_at` is set for Slack drafts, which expire after 24 hours.
- `version` is optional and opaque: it changes whenever the draft text or title changes (an edit,
  an accepted suggestion, from this app or another tab). The app sends back the `version` of the
  draft the PM was looking at in the approve body. When it is sent and does not match the stored
  draft, the server answers `409 stale` and publishes nothing; the app reloads the draft and
  tells the PM to read it again. A server that does not set `version` ignores the field, and an
  approve without it skips this check. Either way the backend tells the tool server the
  `args_hash` of the draft it just checked (`expected_args_hash`), so a save that lands between
  that check and the decision (another tab) is refused there too, atomically, and answered
  `409 stale`. If the draft was approved elsewhere (the Slack card) with
  the very text of that `version`, by the same PM, the approve answers the approved draft (`200`,
  `status: "approved"`) and sends nothing; approved elsewhere with other text, or rejected, it is
  `409 not_pending`. Either way the app should show the draft as decided.
- `edited` is true when a person changed the bot's text: an edit saved here (`PUT`), an accepted
  suggestion, or an edit made on the Slack card. `edited_at` is when the PM last saved an edit
  here, else `null`. An edited draft is the PM's text: it has no citations.
- `external` is true when approving would post to a channel shared outside the company; the
  app must then ask before sending `confirm_external: true`.
- **What the approve dialog shows** (the Slack card shows the same):
  - `restricted_sources`: the run that drafted it read restricted sources (or it predates that
    record, which counts the same). A Slack post of it is refused (`409 audience_blocked`)
    where anyone outside the product team can see the channel. Show "Built from restricted
    sources".
  - `audience`: for a Slack draft (`slack_reply`, `slack_message`), `{members, external}`;
    `null` for Confluence and Jira. `members` (how many people are in the channel) and
    `external` (shared outside the company) are `null` until PM OS has checked the channel,
    which it does when an approve is tried; after a `needs_external_confirm` or
    `audience_blocked` they are filled in. Show "Visible to 14 people", "External channel".
  - `marker`: the line PM OS adds to what it writes, so readers can tell: a Slack post ends with
    "_Drafted with PM OS_"; a Jira issue's description and a Confluence page end with "Drafted
    with PM OS · run <run id>". `null` for a practice draft. Show "Posts as you, marked
    “Drafted with PM OS”" (Slack) or "Ends with “Drafted with PM OS”" (Jira, Confluence).
- **`target.label`** names where the draft goes in words. Confluence and Jira labels use ids
  (space and project keys) and, when the run read them, the names the tool server recorded:
  "Space PM, under “Specs”", "Page “PRD: Billing retries”", "Project PAY, a new Story under
  PAY-212: TCB grace period"; an id the run didn't read stays an id ("under page 163841").
  **Slack labels never show a channel id.** They name the channel from what the tool server
  last read from Slack (its `target_channel`: when the draft was made, and again at each
  approve or edit, so a renamed channel shows its new name after the next one):

  | Channel | Message (`slack_message`) | Reply (`slack_reply`) |
  | --- | --- | --- |
  | A channel or private channel with a name | "#launch-acme" | "A thread in #launch-acme" |
  | A private channel without a usable name | "A private channel" | "A thread in a private channel" |
  | A DM with a team member | "Direct message with ben" (their handle) | "A direct message thread with ben" |
  | Any other DM | "Direct message" | "A direct message thread" |
  | A group DM | "A group direct message" | "A group direct message thread" |
  | Not read yet, or a public channel with no usable name | "A Slack channel" (`D…`: "A direct message") | "A Slack thread" (`D…`: "A direct message thread") |

  A private channel's name is shown: the draft is the owner's own, and they are a member of
  the channel it goes to. A name is shown only if it is shaped like a Slack channel name
  (letters, digits, `-`, `_`, `.`, at most 80), a DM partner only if it is a team handle;
  anything else falls back to the row without a name. The titles stay "Reply in a Slack
  thread", "Message to a Slack channel" and "Direct message". `target.url` opens the channel
  in Slack. Anything that isn't shaped like an id reads "An unrecognised channel" (or space,
  page, project, epic).
- **`recommendations`** is what reviewers assigned to the PM recommend on this draft
  (**Reviews**), oldest first, one per reviewer; `[]` when none has (always for a practice
  draft, and from an older tool server). Advice only: it never decides the draft, and the
  owner's buttons work the same with or without it.
  - `reviewer` is their handle; `reviewer_name` their display name, or their handle when they
    have none (**untrusted text**: plain text only).
  - `verdict` is `approve` ("Rex recommends approving") or `changes` ("Rex suggests changes").
  - `note` is the reviewer's note, **untrusted plain text** (no Markdown, HTML or links made
    from it), or `null`: none given, or removed with a Slack draft's text after 24 hours, which
    `note_removed: true` says.
  - `stale: true` means the draft changed since the advice was given (an edit saved here, an
    accepted suggestion, an edit on the Slack card): show "Rex recommended approving an
    earlier version" and grey the note. The Slack card shows the same line without the note.
  - When the owner then decides, the audit log records whether there was advice and on which
    version (**Reviews**, "What gets recorded").
- **A practice draft** (`practice: true`) lives only in `pmos-web`. Approving it creates
  nothing anywhere: it becomes `approved` with `result_url: null`.
- **Sources** are what the bot read for the task that made the draft. When the runtime cannot
  say, `sources` is empty and the app says so. `title` is the item's own title where PM OS
  recorded one at the read: `PAY-212: TCB grace period` for a Jira issue, the page title for
  Confluence and the docs, the file's title for the context library. Slack sources are
  always `Slack thread` or `Slack message`, never with a channel id (Slack text is kept at
  most 24 hours, so no title is recorded; the `url` opens it in Slack), and reads from before
  titles were recorded show the id (`PAY-212`, `Confluence page 12345`). `excerpt` is always `null`: no source text is kept.
  `url` may be `null`. `cited` is true when the body has a citation marker for this source
  (see **Citations**): the draft relies on it, rather than merely having read it.
- **`body`** is shown as the bot wrote it, except that a body written entirely on one line
  with the characters `\n` where line breaks belong is shown with real line breaks (and
  `\t` as tabs), and that the bot's source markers are shown as citation markers (see
  **Citations**). Both are for display only: approving without an edit sends the bot's own
  text, from which the tool server removes every marker as it writes. Saving an edit
  (`PUT`) makes what the PM saw the text that is sent, from every surface.

### Citations

A bot may end a sentence of a draft with a marker naming a source it read for the task. The
`body` of a `Draft` carries these as **citation markers**, `[[s3]]`, where `s3` is the `id` of
a source in the same draft's `sources`. They are placed right after the text they support,
with no space before them, and several in a row (`[[s1]][[s4]]`) mean several sources.

```json
{"body": "## Grace period[[s1]]\n\nCustomers get 7 days to pay[[s1]][[s4]], as the pricing principles say.\n\n- Retries stop after the grace period[[s2]].\n\nWrite `[[src-3f9a1c]]` to cite.",
 "sources": [
   {"id": "s1", "system": "jira", "title": "PAY-212: TCB grace period", "url": "https://…", "excerpt": null, "cited": true},
   {"id": "s2", "system": "confluence", "title": "PRD: Billing retries", "url": "https://…", "excerpt": null, "cited": true},
   {"id": "s3", "system": "slack", "title": "Slack thread", "url": "https://…", "excerpt": null, "cited": false},
   {"id": "s4", "system": "context", "title": "Pricing principles", "url": null, "excerpt": null, "cited": true}]}
```

**What the app does.** Render `body` as markdown. In **text nodes only** (never inside inline
code or code blocks, which the markdown parser already keeps apart), split on the regular
expression `/\[\[s(\d{1,3})\]\]/g` and render each match as a small marker linked to the
source with `id` `s` + the digits; its number can simply be those digits (`[[s3]]` → 3, the
source's position in the list). Markers can appear in paragraphs, list items, headings,
table cells, emphasis and link text. A marker whose id is not in `sources` cannot happen;
if it ever does, render nothing for it. `cited` tells which sources to show as used.
To show the body as plain text (a copy button, the edit box), remove the matches.

**What the server guarantees:**

- Every `[[sN]]` outside code in `body` is one the server put there, for a listed source the
  task read. The bot writes markers in another form (`[[src-3f9a1c]]`, the ref each read
  gave it, `crates/contract/src/cite.rs`); only those that name a source in `sources` become
  `[[sN]]`. Markers naming a source the task did not read (or one past the 20 listed),
  malformed ones, and any `[[sN]]` the bot wrote itself are removed, cleanly (no double
  spaces, no space before punctuation), and counted in a log line that carries no text.
- Text inside inline code and fenced code is left exactly as written: a marker there is
  code, not a citation, and is never rewritten (so ``Write `[[src-3f9a1c]]` `` stays).
- Titles are never cited: a marker in the bot's title or summary is removed.
- A draft with no markers is exactly as before: the same `body`, every `cited` false.
- With no sources (the tool server can't say, or an older one sends no refs), every marker
  is removed and nothing is cited.
- `version` is unchanged by all this: it is still the version of what an approve would send.
- What is approved never contains a marker: the tool server removes them from every field
  before writing to Jira, Confluence or Slack, and Slack approval cards and the console
  show the text without them.

**Edits.** The PM's text has no citations. The edit box should show the body with the
`[[sN]]` markers removed; a `PUT` removes any marker it is sent anyway (`[[sN]]` or
`[[src-…]]`, outside code), so an edited draft (`edited: true`) has a `body` with no markers
and every source `cited: false`. The same holds for an accepted suggestion: `Suggestion.body`
never has markers, and once accepted the draft is edited. A practice draft never has any.

## Asking

| Method and path | Body | Answer |
| --- | --- | --- |
| `POST /api/tasks` | `{text, persona?}` | `201 Task`. The PM's personal bot takes it, as `persona` (a bot's key, **Bots**; absent or `"nudge"` is Nudge). A bot not on the PM's team is `409 bot_not_on_team`, one no longer offered `409 bot_unavailable`, and neither reaches the runtime. On success the picker's default (`Me.ask_persona`) becomes this bot. |
| `GET /api/tasks/{id}` | | `Task` |
| `GET /api/tasks` | | `{tasks: [Task]}`, the PM's 20 most recent, without `events` |

```json
{"id": "tsk_…", "bot": "pm-ana", "text": "…", "status": "working",
 "result": null, "error": null, "draft_ids": [], "created_at": "…", "updated_at": "…",
 "progress": {"done": 2, "total": 3}, "retrying": false,
 "persona": "chief", "persona_offered": true,
 "events": [{"at": "…", "kind": "milestone", "text": "Reading Jira"},
            {"at": "…", "kind": "milestone", "text": "Drafting in Confluence"}]}
```

`status` is `queued`, `working`, `needs_approval`, `done`, `failed` or `cancelled`. A failed
task carries a plain sentence in `error`. The app polls `GET /api/tasks/{id}` every two
seconds until the task settles. `result` never has citation markers: markers are for
drafts, and any the bot put in its answer are removed.

- `events` are the human-readable steps only: every event is `kind: "milestone"`, in order,
  each text once (its first time). The runtime's bookkeeping ("queued", "claimed by worker
  …", "started on …", "2 of 3 done") is never passed on, so no worker or host name reaches
  the app. Show them as a list of steps; there is nothing else to filter.
- `progress` is `{done, total}` from the bot's latest checklist, or `null` when it has none
  (most short tasks). Show it as "2 of 3 done" or a bar; don't show `0 of 0`.
- `retrying` is true while the task is queued or working again after the runtime retried it
  (a restart interrupted it). The app may say "Retrying after an interruption". Nothing
  else is signalled today: the model gateway's own short retries are invisible here.
- In `GET /api/tasks`, each task has `progress` and `retrying` (from the task alone) but no
  `events`.
- `persona` is the bot the task ran as (`null` for Nudge, and for tasks from before the
  marketplace or from a runtime that doesn't say), and `persona_offered` whether it is offered
  now, exactly as on drafts (**Bots**, "Attribution"). Activity shows each row's bot from them.

## Bots

The bot marketplace (`docs/bot-marketplace-spec.md`, slice W1). Besides Nudge, the always-on
generalist, an Admin offers bots (Chief, Sprout, Echo, …) through content, and each PM picks
which are on **their own** team: during onboarding ("Pick your team"), on the Bots page, or from
the ask box's picker. In the API a bot is a **persona** and is named by its **key** (`"chief"`;
lowercase letters, digits and single hyphens, at most 32). `"nudge"` is Nudge's key, and Nudge is
always on every team. A team grants nothing: every ask, draft and write is still the PM's own
bot's, checked by the tool server, and every write still waits for the PM's approval.

Same session, origin and CSRF rules as every route (**Sign-in and safety**). **Who:** every route
here needs `ask-bot` (`403 not_allowed` otherwise: an Auditor-only person never has a team, and
`pmos-web` refuses them before any upstream call). A PM, Reviewer, Admin or skill author who may
ask has a team like anyone. **Nobody reads or changes anyone else's team, an Admin included:**
there is no route that takes a handle; every call `pmos-web` makes goes to the signed-in
person's own team at the tool server, with a user assertion for them bound to the exact request,
and the tool server checks the person again in its own tables. (Who uses which bot, for Admins
and Auditors, is `GET /api/admin/bots`, slice A1; it is not built yet.)

| Method and path | Body | Answer |
| --- | --- | --- |
| `GET /api/bots` | | `200 {bots: [BotCard], withdrawn: [WithdrawnBot], brief: BriefWriter \| null}` |
| `PUT /api/me/team` | `{add?: [key], remove?: [key], via: "onboarding" \| "bots_page" \| "picker", routines?: "nudge" \| "pause"}` | `200 {bots, withdrawn, brief}`, the same as `GET /api/bots` after the change |
| `PUT /api/me/brief` | `{persona: key}` (`"nudge"` for Nudge) | `200 {bots, withdrawn, brief}`, the same as `GET /api/bots` after the change (**Who writes the daily brief**, below) |
| `POST /api/tasks` | `{text, persona?: key}` | `201 Task` (**Asking**) |
| `PUT /api/me/settings` | `{ask_persona: key}` | `Me` (**Me**) |

```json
// BotCard: an offered bot, Nudge first, then in the catalog's order
{"key": "chief", "display_name": "Chief", "title": "Chief of Staff", "mascot": "chief",
 "always_on": false,
 "summary": "Runs your week: what needs you, what is slipping, and what to tell your leadership.",
 "examples": ["Prep me for the staff meeting", "What slipped since Monday?"],
 "can_do": ["daily-brief", "meeting-prep", "week-review"],
 "reads": ["jira", "confluence", "slack", "docs"],
 "drafts": ["Jira issues", "Confluence pages", "Slack replies"],
 "needs": [],
 "on_team": true, "added_at": "2026-11-20T09:01:00Z", "added_via": "onboarding", "new": false,
 "drafts_this_month": 6, "last_used_at": "2026-11-21T09:00:00Z",
 "routines": [Routine]}

// Need: a data source the bot would use but PM OS doesn't have (Sprout: analytics)
{"source": "analytics", "label": "an analytics source",
 "without": "I work from numbers you paste in or that a Confluence page holds, and I say so.",
 "met": false}

// WithdrawnBot: a row on the PM's team whose bot is no longer offered (or no longer exists)
{"key": "ghost", "added_at": "2026-11-20T09:01:00Z", "added_via": "bots_page",
 "drafts_this_month": 1, "last_used_at": null, "routines": []}

// Routine: one of the PM's own routines that runs as this bot (read only here)
{"id": "rtn_…", "enabled": true, "cron": "0 8 * * 1-5", "tz": "Europe/London",
 "prompt": "[daily-brief] Write my daily brief with the daily-brief skill.",
 "next_run_at": "2026-11-24T08:00:00Z"}

// BriefWriter: who writes the PM's daily brief (slice R2)
{"routine_id": "rtn_…", "writer": "chief", "enabled": true}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `key` | string | The bot's key: what every route, `Me.team`, `Me.ask_persona`, `persona` on drafts and tasks use. |
| `display_name` | string | Its name ("Chief"). **Untrusted text** (content): plain text only. |
| `title` | string | Its role line ("Chief of Staff"). Untrusted text. |
| `mascot` | string | Which drawing: `nudge`, `chief`, `sprout` or `echo` today. Choose from the app's own fixed set; an unknown value draws the neutral face, never a guess. |
| `always_on` | bool | `true` for Nudge only: "Always on your team", no Add or Remove. |
| `summary` | string | One line for the card. Untrusted text. |
| `examples` | [string] | 1 to 6 example asks: the card's examples and the chips under the ask box. Untrusted text. A chip fills the ask box and never sends. |
| `can_do` | [string] | Skill keys ("What it can do"); the app labels them from its own fixed map, and shows a key it has no label for as the key. |
| `reads` | [string] | The systems it reads (`jira`, `confluence`, `slack`, `docs`), worked out from its tools. |
| `drafts` | [string] | What it may draft, as labels ("Jira issues", "Confluence pages", "Slack replies", "Slack messages", "bets register entries", "Confluence page updates"). "Every one waits for your approval." |
| `needs` | [Need] | Data sources it would use but PM OS lacks; `[]` for none. `label` and `without` are untrusted text. `met: false` shows the badge "Needs <label>", with `without` as its explanation; the bot can still be added. |
| `on_team` | bool | On the PM's team now. Always `true` for Nudge. |
| `added_at`, `added_via` | string or null | When and from where it was added (`onboarding`, `bots_page`, `picker`, or `seed` for the `evals` account); `null` for Nudge and for a bot not on the team. "On your team since…". |
| `new` | bool | "New" label under Available. **Always `false` in this version**: no service records when a bot was first offered (see the spec's §13, "W1 is built"). |
| `drafts_this_month` | integer | Drafts this bot made for this PM since the first of this month (UTC), any status. `0` when none. |
| `last_used_at` | string or null | When the PM last asked this bot (their newest task that ran as it, from the runtime's `GET /v1/tasks?persona=`), or `null`: never, or the runtime couldn't say. A `null` hides the part ("· last used today"). |
| `routines` | [Routine] or null | The PM's own routines that run as this bot (Nudge's: those that name no bot); `null` when the runtime couldn't say. For Settings (read only) and for the removal question. The daily brief is a routine whose `prompt` starts with `[daily-brief]`. |

`brief` (`BriefWriter`, beside `bots` and `withdrawn`) says who writes the PM's daily brief:
`writer` is `"nudge"` or a bot's key, from the PM's first routine whose prompt starts with
`[daily-brief]` (the Slack service keeps one); `routine_id` is that routine and `enabled`
whether it is on. With no brief routine it is `{routine_id: null, writer: null, enabled:
false}`; `brief: null` when the runtime couldn't list the routines. An older server leaves the
field out: show no brief switch.

The text fields are passed on **exactly as the tool server served them from the content files**,
JSON string for string: `pmos-web` builds no HTML, Markdown or instructions from them, and keeps
only the fields above (never the persona file's role, owner or body). Render them as React text
nodes, never `dangerouslySetInnerHTML` or Markdown: a summary can hold `<img onerror>` and must
show as those characters. A persona the tool server lists in another shape is left out whole.

**`GET /api/bots`** reads the catalog and the PM's team from the tool server, fresh, and the PM's
recent tasks and routines from the runtime. The tool server failing fails the route (`502
upstream_unavailable`, `503 unavailable`); the runtime failing does not: `last_used_at` and
`routines` are then `null`. `withdrawn` lists the team's rows whose bot isn't offered now (an
Admin withdrew it, or its file is gone): show "No longer offered", with Remove only; it has no
name to show ("A bot no longer offered", the neutral face).

**`PUT /api/me/team`** changes the PM's own team, at once and atomically. "Add to my team" and
"Continue with 2 bots" are `{add: [...], via: "onboarding"}`; the Bots page uses `via:
"bots_page"`, the picker's Add `via: "picker"`. In order:

1. Checked here, before anything is sent: `ask-bot` (`403 not_allowed`); the body has exactly these
   fields (`400 bad_request` otherwise, a `handle` included); `via` is one of the three; `routines`,
   if given, is `nudge` or `pause`; every key is a key; at most 16 in each list; at least one key in
   all; no key in both; `nudge` (or any always-on bot) in neither (`409 always_on`); every key to
   **add** offered in a fresh read of the catalog (`409 bot_unavailable`). Removing a bot that is
   no longer offered is allowed (that is the only thing a withdrawn bot allows); adding one already
   there, or removing one that isn't, changes nothing and is not an error.
2. **A bot's routines move before the bot leaves** (spec §6, "Removal"). When `remove` names any
   key, `pmos-web` reads the PM's routines from the runtime and, for each that runs as a removed
   bot, either moves it to Nudge (`routines: "nudge"`, the default) or pauses it (`"pause"`; it
   keeps its bot, so adding the bot back and resuming it works). **The daily brief is always moved
   to Nudge, never paused**, whichever was chosen for the others (slice R2): the brief is the
   day's one message, and Nudge writes it as it did before. The Bots page asks only when the
   bot has routines (`BotCard.routines`; when that is `null`, ask anyway). If the runtime can't be
   read or refuses a change, **the team is not changed**: `502 upstream_unavailable` when no
   routine had been changed yet, `502 unknown_outcome` when some may have (reload the Bots page to
   see).
3. The change goes to the tool server as `{add, remove, via}` (never `routines`), which decides
   again. Its refusals keep their codes: `400 bad_request`, `403 not_allowed`, `403 not_a_member`,
   `409 always_on`, `409 bot_unavailable`, `409 team_full` (16 bots), `429 rate_limited` (30 team
   changes an hour per person, refused ones included), `503 assertion_required` (a development
   stack without signed assertions), `503 unavailable`. Every one is "nothing changed" on the team
   (routines moved in step 2 stay moved, to Nudge or paused). A change whose answer was lost is `502
   unknown_outcome`: reload the Bots page.
4. The answer is the Bots page as it now is. A failed save keeps the page and says so; the app
   marks no hint for it.

Removing a bot keeps everything it made: its drafts stay the PM's, decidable and attributed to it;
a task it already started finishes; a queued one fails when it is claimed with the tool server's
plain sentence ("Chief isn't on your team any more…"), which arrives as the task's `error`.

**Who writes the daily brief** (slice R2). Nudge writes it unless the PM chooses Chief in Chief's
Settings on the Bots page. `PUT /api/me/brief {persona}` sets who writes it: `"nudge"`, or a bot
that is on the PM's team, still offered and whose `can_do` lists `daily-brief` (Chief today). It
changes only the signed-in PM's own routines whose prompt starts with `[daily-brief]`, and of
those only who runs them: the schedule, the prompt and whether it is on stay as they are, and the
brief's content is the same `daily-brief` skill. In order:

1. Checked here, before anything is sent: `ask-bot` (`403 not_allowed`); the body is exactly
   `{persona}` (`400 bad_request`); a key (`400 bad_request`); for a bot other than Nudge, a fresh
   read of the catalog lists it (`409 bot_unavailable`), it writes briefs and isn't always-on
   (`409 cant_write_brief`), and a fresh read of the PM's team has it, offered (`409
   bot_not_on_team`, `409 bot_unavailable`).
2. The PM's routines are read from the runtime; none with the tag is `409 no_brief` ("say
   `brief me at 8` in Slack"). Each brief routine not already run by that bot is changed with the
   runtime's `PATCH /v1/routines/{id} {persona}` (`null` for Nudge). A refusal before any change is
   `502 upstream_unavailable` (`409 bot_unavailable` for the runtime's `400`: its catalog doesn't
   offer the bot); once one may have changed, `502 unknown_outcome`.
3. For a bot other than Nudge, the team is read **again**: if the bot left it in between (a
   removal in another tab), the brief is moved straight back to Nudge and the answer is `409
   bot_not_on_team`. The tool server still decides at every run of the routine; a run it refuses
   pauses the routine with its sentence (`docs/bots-runtime.md`, "Personas").
4. The answer is the Bots page as it now is.

What changes for the PM: the next brief's DM in Slack says it is Chief's ("Chief: …", and the
Preview line while Chief is a Preview), and Today's card says "Written by Chief". The brief's
block still reaches the panels on Today. A reply in the brief's Slack thread goes to Nudge unless
it starts with "Chief:" (a routine's run starts no thread to inherit; `docs/bots-runtime.md`).
Moving the brief to a new time in Slack (`brief me at 9`) keeps the writer; `brief now` runs as
the writer.

**The picker.** The ask box's bot picker lists `Me.team` (selectable) and the other offered bots
from `GET /api/bots` (greyed, with "Add", which sends `PUT /api/me/team {add: [key], via:
"picker"}` and then selects it). **Its default is `Me.ask_persona`**: the bot the PM last asked,
kept by the server on every successful `POST /api/tasks` (Nudge when the ask named none), or set
with `PUT /api/me/settings {ask_persona}`; when that bot is no longer on the team (or no longer
offered) it reads `"nudge"`. The example chips are the selected bot's `examples`.

**Asking a bot.** `POST /api/tasks {text, persona}` (**Asking**): `persona` absent or `"nudge"`
asks Nudge. For another key, `pmos-web` reads the PM's team from the tool server first: not on
it is `409 bot_not_on_team`, on it but no longer offered `409 bot_unavailable`, and the team
unreadable fails the ask (`503 unavailable` or `502 upstream_unavailable`); in each case **no task
is made and the runtime is never called**. The tool server checks the team again when the task is
claimed. If the runtime itself refuses the bot (`unknown_persona`), the answer is `409
bot_unavailable`; if it refuses because the PM's bot can't run personas
(`persona_needs_personal_bot`), `409 task_refused`.

**Attribution.** Drafts (`DraftSummary`, `Draft`), tasks (`Task`, so Activity's rows) and the
reviewer's views (`ReviewSummary`, `ReviewDraft`, restricted ones included: it is metadata)
carry `persona` (the key, `null` for Nudge) and `persona_offered`. To show it:

- `persona: null`: Nudge ("Drafted by Nudge", Nudge's face).
- A key with `persona_offered: true`: look it up in `GET /api/bots` for its `display_name` and
  `mascot` ("Drafted by Chief").
- `persona_offered: false`, or a key not in `GET /api/bots`: "A bot no longer offered", with the
  neutral face. Never guess a name or a mascot from the key.

`persona_offered` is checked against a catalog read at most 10 seconds old (5 minutes while the
tool server can't be read); with no catalog at all every key reads `false`.

**Errors** (see **Error codes**): `400 bad_request`, `401 signed_out`, `403 bad_origin` /
`bad_csrf` (writes), `403 not_allowed`, `403 not_a_member`, `409 bot_not_on_team`, `409
bot_unavailable`, `409 always_on`, `409 team_full`, `409 no_brief`, `409 cant_write_brief`, `429
rate_limited`, `502 upstream_unavailable`, `502 unknown_outcome` (a team or brief change), `503
assertion_required`, `503 unavailable`, `503 users_unavailable`.

## The daily brief and follow-ups

| Method and path | Body | Answer |
| --- | --- | --- |
| `GET /api/brief` | | `Brief`: whether a brief has been delivered, and the PM's open follow-ups and `changed` items, with empty lists when nothing is stored; or `501 {"code":"not_built"}` while the server's `PMOS_WEB_BRIEF` is off, and the app then hides that part of the home page |
| `POST /api/followups/{id}/dismiss` | | `204` ("Not mine"), the same again if repeated; `404` for an id that isn't the PM's, or is dismissed or expired |
| `POST /api/followups/{id}/draft` | | `201 Task`, a reply (waiting on you) or gentle nudge (you wait on) draft that arrives as a draft to approve; `404` as above |

```json
{"date": "2026-11-16", "delivered_at": "2026-11-16T08:31:02Z", "status": "today",
 "changed": [{"system": "jira", "title": "PAY-212 moved to Blocked", "summary": "…", "url": "…"}],
 "waiting_on_you": [{"id": "fu_…", "system": "slack", "who": "Ben", "summary": "asked about the pricing page", "url": "…", "age_days": 2}],
 "you_wait_on": [{"id": "fu_…", "system": "slack", "who": "Legal", "summary": "the DPA question", "url": "…", "age_days": 3}],
 "slack_withheld": false, "persona": "chief", "persona_offered": true}
```

- `persona` and `persona_offered` (slice R2) say who wrote the day's brief, as drafts do
  (**Bots**, "Attribution"): `null` is Nudge; a key is looked up in `GET /api/bots`, and
  `persona_offered: false` (or a key it doesn't list) reads "a bot no longer offered". It is the
  bot that wrote the brief that filled the day, as the Slack service reported it, kept with the
  day's `changed` items: once those expire (`stale`, `never`) it is `null`. It is not who writes
  the next one (that is `GET /api/bots`'s `brief`). An older server leaves both out: read Nudge.

- `slack_withheld` is `true` when the tool server couldn't be reached and the brief was
  served on the PM's last-known-good answer (**Sign-in and safety**): every item whose
  `system` is `slack`, in all three lists, is left out, and `note` says
  `"Slack items are hidden until PM OS can check your access"`. The app shows its own
  sentence for it. Dismissing and drafting are writes, so they answer
  `503 users_unavailable` then. An older server leaves the field out: read it as `false`.

- `delivered_at` is when a daily brief last filled this PM's day (the producer route below),
  however long ago, or `null` if none ever has.
- `status` says what to say about it:

  | `status` | Meaning | What the home page says |
  | --- | --- | --- |
  | `never` | No brief has ever been delivered for this PM (`delivered_at: null`). The lists are empty. | Nothing about "your brief is in". Offer to set one up ("brief me at 8" in Slack). |
  | `today` | The last brief came within the last 24 hours; `changed` is its list. Empty lists mean it found nothing. | "Your daily brief is in", and if all lists are empty, that it found nothing ("Suspiciously calm"). |
  | `stale` | A brief came before, more than 24 hours ago. `changed` is empty; the follow-ups still listed are the ones not yet expired (non-Slack ones last a week). | "No brief yet today" with the time of the last one. |

  An older server sends neither field: treat that as `never`.
- `date` is the date the last brief was for while `status` is `today`, else today's date.

Every row has a `url` to its source. Nothing is posted from a follow-up without the PM
approving the draft it produces. **Draft** (`POST /api/followups/{id}/draft`) asks the PM's
bot for a reply or a nudge with fixed wording; the follow-up's own fields (who, summary,
link), which the brief wrote from other people's messages, go to the bot as quoted data
between markers, with an instruction never to follow anything in them.

A follow-up is a one-line summary, never message text. Slack-derived ones expire after 24
hours, all others after a week, and expired ones are deleted, not just hidden; `changed`
items last 24 hours.

**Producer route (server to server, not for the app).** `PUT /internal/brief/{handle}` with
`Authorization: Bearer <PMOS_WEB_BRIEF_TOKEN>` replaces that member's follow-ups and `changed`
items: `{"date"?: "2026-11-16", "changed": [{system, title, summary?, url}], "waiting_on_you":
[{system, who, summary, url, created_at?}], "you_wait_on": [...], "persona"?: key}`. `persona`
(slice R2) is who wrote the brief, from the runtime's notification (absent or `"nudge"` is
Nudge; anything not shaped like a key is `422 bad_brief`); it is stored with the day and replaced
by each brief. **Deploy pmos-web before the Slack service**: an older pmos-web refuses the field
(`422`), so a Chief-written brief would not fill the panels. At most 50 items per list,
a 128 KiB body, one-line text (`summary` at most 200 characters, `who` 80), `system` made of
`a-z`, digits, `-` and `_`, every `url` `http` or `https` without credentials; anything else
is `422 bad_brief` (`413` for an oversize body, `401 bad_token`, `404` for a handle that isn't
an active user in the user database, `503 users_unavailable` when that can't be asked) and
nothing is stored. A follow-up sent again keeps its age and a dismissal; one left
out is removed. A call that looks like a browser's (it carries a `__Host-pmos` session cookie
or an `Origin` header) is `403 forbidden`, whatever its token. With `PMOS_WEB_INTERNAL_BIND`
set, the route is served only on that second listener (and `404` on the browser-facing one).
**The caller is the Slack service** (`pmos-slack`, `crates/slack/src/brief_feed.rs`): when a
PM's daily brief finishes (a run of their brief routine, or a `brief now`; never any other
task, whatever its answer contains), it sends the items of the brief's machine-readable block
(`docs/daily-brief.md`, "The web app's panels") here, with `PMOS_WEB_URL` and
`PMOS_WEB_BRIEF_TOKEN` set on it. It sends nothing until a brief runs, so a PM with no brief
routine has empty lists and `status: "never"`. A brief that found nothing still calls it, with
empty lists, so the home page can tell "nothing found" (`today`) from "never ran" (`never`).

## Error codes

Every error is `{"code", "error"}`: `code` is one of these, and `error` a plain sentence for
the person, fixed by `pmos-web` (never another service's text). **"Nothing changed"** means
the request was refused before anything was written anywhere, so the app may say so (and
"Nothing was sent" for a draft). **"Unknown"** means it may have happened: the app must not
say nothing was sent, and should reload the draft to see where it stands.

| Status | `code` | When | Nothing changed? |
| --- | --- | --- | --- |
| `400` | `bad_request` | A body or query parameter isn't what the route takes. | Yes |
| `401` | `signed_out` | No live session. | Yes |
| `401` | `bad_token` | The producer route's token is wrong (server to server only). | Yes |
| `403` | `bad_origin` | A write from another origin, or with neither `Origin` nor `Referer`. | Yes |
| `403` | `bad_csrf` | A write without the session's CSRF token. | Yes |
| `403` | `proxy_not_trusted` | Password sign-in: a request that carries a forwarded header (`X-Forwarded-For`, `Forwarded`, `X-Real-IP`) while `pmos-web` trusts no proxy (`PMOS_WEB_DIRECT=1`), or from a peer not in `PMOS_WEB_TRUSTED_PROXIES`, or from a trusted proxy that names no client: the proxy setting doesn't match the deployment. | Yes |
| `403` | `not_a_member` | Not (or no longer) an active user in the user database; every session of theirs ends. Also when the tool server doesn't count them a member on a decision. | Yes |
| `403` | `not_allowed` | Their role doesn't allow drafts, tasks or asking (also the Bots page and team changes: **Bots**). | Yes |
| `403` | `forbidden` | Admin and auditor routes their role doesn't allow; the reviewer routes for someone who isn't an active reviewer; the producer route called like a browser. | Yes |
| `403` | `own_draft` | A reviewer reading or recommending on their own draft: they decide it as its owner (**Reviews**). | Yes |
| `403` | `restricted_draft` | A recommendation on a draft built from restricted sources: a reviewer sees its metadata only (**Reviews**). | Yes |
| `404` | `not_found` | Unknown path or id, or someone else's draft, task or follow-up; for a reviewer, a draft of a PM not assigned to them. | Yes |
| `405` | `method_not_allowed` | A known path with another method. | Yes |
| `409` | `not_pending` | The draft was already decided (perhaps in Slack). Read it again to show how. | Yes |
| `409` | `stale` | The draft changed since the `version` the PM approved. Also an admin's user or role change made against a directory `version` that has moved since (**Admin: users and roles**). | Yes |
| `409` | `expired` | A Slack draft past its 24 hours. | Yes |
| `409` | `draft_changed` | A reviewer's recommendation on a version of the draft that isn't current any more (its owner saved other text since it was read). Read it again (**Reviews**). | Yes |
| `409` | `needs_external_confirm` | The channel is shared outside the company; ask, then approve with `confirm_external: true`. `audience` is now filled in. | Yes |
| `409` | `audience_blocked` | Built from restricted sources, and people outside the product team can see the channel. Only rejecting is left. | Yes |
| `409` | `not_approvable` | The tool server won't approve it as it stands (its tool or bot is no longer allowed). | Yes |
| `409` | `change_in_progress` | A change to this draft is already being written. | Yes |
| `409` | `no_suggestion` | Accepting a suggestion that isn't ready. | Yes |
| `409` | `not_practice` | Deleting a draft that isn't the practice draft. | Yes |
| `409` | `task_refused` | The runtime won't take the PM's task (not onboarded, no bot). | Yes |
| `409` | `bot_not_on_team` | Asking a bot, or making it the picker's default, while it isn't on the PM's team (**Bots**). Add it first. The ask never reached the runtime. | Yes |
| `409` | `bot_unavailable` | A bot that isn't offered any more (withdrawn, deleted, or unknown): adding it, asking it, or making it the picker's default (**Bots**). | Yes |
| `409` | `always_on` | A team change naming Nudge: it is always on the team (**Bots**). | Yes |
| `409` | `team_full` | A team change that would put more than 16 bots on the team (**Bots**). | Yes |
| `409` | `no_brief` | Choosing who writes the daily brief when the PM has no brief routine (**Bots**, "Who writes the daily brief"). One is started in Slack (`brief me at 8`). | Yes |
| `409` | `cant_write_brief` | Choosing a bot to write the daily brief that doesn't write briefs (its `can_do` lacks `daily-brief`) (**Bots**). | Yes |
| `409` | `held` | Approving, editing or asking for a change to a draft on hold: the bot that drafted it is no longer offered (**Drafts**, "Held drafts"). It can still be rejected. | Yes |
| `409` | `not_held` | An admin releasing a draft that isn't on hold (decided, its bot offered again, or already released) (**Admin: bots**). | Yes |
| `409` | `github_not_linked` | An admin proposal: the admin hasn't linked GitHub in PM OS. Pull requests are opened as them; `error` says how to link. | Yes |
| `409` | `stale_base` | An admin proposal or validation made from a copy that differs from the server's or the default branch's (**Admin: policy**). | Yes |
| `422` | `invalid` | An admin proposal that doesn't pass validation (`errors`, `warnings`). Also a malformed value in an admin's user or role change, and a reviewer's note over 1,000 characters. | Yes |
| `422` | `policy_mismatch` | An admin proposal: the tool server regenerates different policy files from it (`paths`). | Yes |
| `429` | `rate_limited` | Too many admin proposals this hour; `Retry-After`. Also more than 30 team changes in an hour (**Bots**). | Yes |
| `501` | `not_configured` | Admin proposals are off at the tool server. Also password sign-in on in `pmos-web` while the tool server has no vault key for it. | Yes |
| `502` | `audit_unavailable` | The audit log couldn't record an admin proposal, or an admin's read of a personal or restricted file, so it wasn't done or shown. | Yes |
| `502` | `github_refused`, `github_unavailable` | GitHub refused the admin's account, or didn't take the proposal. | Yes |
| `503` | `github_rate_limited` | GitHub's own rate limit. | Yes |
| `413` | (no body) | The producer route's body is over 128 KiB. | Yes |
| `415` | `bad_content_type` | A write with a body that isn't JSON. | Yes |
| `422` | `bad_edit` | The edited text isn't valid for where the draft goes. | Yes |
| `422` | `bad_brief` | The producer route's body failed a check. | Yes |
| `501` | `not_built` | The daily brief is off on this server. | Yes |
| `502` | `upstream_unavailable` | The runtime or tool server couldn't be reached, refused, or answered something unreadable. On a decision, only when the request never reached the tool server, or it refused it. | Yes on a decision; for a task (`POST /api/tasks`), the task may have been made: reload the list |
| `502` | `unknown_outcome` | A decision was sent, but the answer was lost (the connection dropped, or the tool server failed with a `5xx`), and reading the draft again didn't show it decided. Also an admin proposal whose answer was lost: reload the proposals list. Also an admin's user or role change whose answer was lost: reload the team list. Also a reviewer's recommendation whose answer was lost: read the draft again. Also a password change or new recovery codes whose answer was lost: sign in again to see. Also a team change whose answer was lost, or a removal that stopped after moving some of the bot's routines: reload the Bots page (**Bots**). | **Unknown** |
| `504` | `unknown_outcome` | The same, when the tool server didn't answer in time (10 seconds). | **Unknown** |
| `503` | `audience_unavailable` | Slack couldn't say who is in the channel. | Yes |
| `503` | `unavailable` | `pmos-web`'s own store, or the tool server's, is down. | Yes |
| `503` | `users_unavailable` | The tool server (the user database) can't be asked who the person is: sign-in, and every route but the last-known-good reads (**Sign-in and safety**). | Yes |
| `403` | `entra_needs_host` | A Microsoft session of someone holding Admin or Auditor whose link the host hasn't confirmed: the session ends. | Yes |
| `503` | `assertion_required` | An admin's user or role change, a reviewer's recommendation, a team change (**Bots**), or any password sign-in route, while the tool server runs without signed user assertions (a development stack): it makes none. | Yes |
| `403` | `host_only` | A user or role change only someone with host access may make: granting `admin`, an Admin's or Auditor's addresses or Microsoft link, or their password setup code (also when a code the web made is used by someone who holds Admin or Auditor now). | Yes |
| `403` | `self_change` | An admin changing their own roles, status, addresses, links or reviewer assignments, or making a password setup code for themselves. | Yes |
| `409` | `exists` | Inviting a handle that is already taken (handles are never reused). | Yes |
| `409` | `taken` | An address or account that belongs to another user. | Yes |
| `409` | `unchanged` | A user or role change that changes nothing. | Yes |
| `409` | `last_admin` | A change that would leave PM OS without an active admin. | Yes |
| `409` | `dual_role` | A change that would leave someone holding Admin and Auditor without the pilot exception in force. | Yes |
| `409` | `evals_account` | A change to the `evals` account beyond `pm`, or a password for it. | Yes |
| `409` | `not_active` | A reviewer assignment naming someone deactivated, or a password setup code for them. | Yes |
| `409` | `linked` | Changing an Atlassian email or GitHub login while that account is linked. | Yes |
| `409` | `owns_bots` | Deactivating someone who still owns a team bot. | Yes |
| `409` | `not_reviewer` | Assigning PMs to someone without the reviewer role. | Yes |
| `409` | `not_pm` | Assigning a reviewer someone without the pm role. | Yes |
| `429` | `deactivation_limit` | The hourly deactivation cap (3 by default, web and Slack together) is reached. | Yes |
| `401` | `sign_in_failed` | Password sign-in's first step, for every reason alike: no such user, no password, a wrong password, deactivated, or the account waiting out its delay (**Password sign-in**). | Yes |
| `401` | `code_failed` | A wrong authenticator or recovery code; `attempts_left` says how many tries this step has left. | Yes |
| `401` | `sign_in_expired` | No password sign-in in progress (none started, five minutes passed, or its tries used up): start again with the password. | Yes |
| `401` | `setup_failed` | A password setup's first step, for every reason alike: no such user, no code, a wrong, expired or replaced code, or the account waiting. | Yes |
| `401` | `setup_expired` | No password setup in progress (fifteen minutes passed, or its tries used up): start again with the setup code. | Yes |
| `409` | `setup_code_spent` | A password setup code that gave all the work it allows (5 setups started with it, or 6 passwords set with it): an admin makes a new one. | Yes |
| `409` | `wrong_step` | A password setup's steps out of order: the password, then the authenticator, then the code. | Yes |
| `422` | `weak_password` | A new password isn't accepted; `reason` is `too_short`, `too_long`, `matches_name` or `common`. | Yes |
| `403` | `reauth_failed` | Changing a password or making new recovery codes: the current password or the authenticator code wasn't right. | Yes |
| `409` | `no_password` | Changing a password, or making new recovery codes, for someone without a password. | Yes |
| `409` | `sign_in_unconfirmed` | A password setup code for someone whose sign-in name no admin has confirmed, or a setup started with one. | Yes |
| `429` | `too_many_attempts` | Password sign-in: too many refused attempts from one address (20 in 15 minutes), the server's own limit, or a wait on a signed-in change; `Retry-After`. | Yes |
| `503` | `sign_in_busy` | Password sign-in: every password check is busy; try again in a moment. | Yes |

After an `unknown_outcome` the server has already read the draft once: if it showed this PM's
approval (or rejection), the answer would have been that draft, `200`. Approving again is safe:
if the first approve went through, the second answers the approved draft and sends nothing.

## Admin and auditor

Read-only views for the two roles beyond PM. Every route is `GET`; any other method is
`405 method_not_allowed`. Roles come from the user database (at most 10 seconds old), and the
tool server checks them again in its own tables:

| Route | `admin` | `auditor` | anyone else (`pm`, `reviewer`, `skill-author`) |
| --- | --- | --- | --- |
| `GET /api/admin/audit` | yes | yes | `403 forbidden` |
| `GET /api/admin/audit/export` | yes | yes | `403 forbidden` |
| `GET /api/admin/team` | yes | yes | `403 forbidden` |
| `GET /api/admin/users` | yes (addresses included) | yes (no addresses) | `403 forbidden` |
| `GET /api/admin/overview` | yes | yes | `403 forbidden` |
| `GET /api/admin/connectors` | yes | `403 forbidden` | `403 forbidden` |
| `GET /api/admin/bots` | yes | yes | `403 forbidden` |

`GET /api/admin/bots` and its one write are described in **Admin: bots**.

The checks are the permission matrix: `audit` needs `view-audit-log`, `audit/export`
`export-audit`, `team` and `overview` `view-audit-log` or `manage-members`, `connectors`
`manage-connectors`. A person with several roles gets the union.

**Errors** (always `{"code", "error"}`):

| Status and code | When |
| --- | --- |
| `401 signed_out` | No live session. |
| `403 not_a_member` | The person is no longer an active user (every session is ended). |
| `403 forbidden` | The role doesn't allow this route. Also when the tool server, which checks the role again on its side, refuses. |
| `400 bad_request` | A query parameter is malformed (see each route). Nothing is sent upstream. |
| `502 upstream_unavailable` | The tool server didn't answer, or answered something unreadable. |
| `503 users_unavailable` | The tool server can't be asked who the viewer is. |

**What the auditor sees, and doesn't.** Metadata only: who did what, when, through which
client, to which system and target id, with what outcome. Never a draft body, Slack text, a
prompt, a model reply, a task result, a source title, an argument hash or a token. Audit
events are kept (the table is append-only); nothing here deletes them. The auditor does not
see connectors at all, and sees no PM's drafts or tasks (those routes need PM permissions an
auditor doesn't have).

**Every admin read is itself audited.** The tool server records each call as an event of its
own (`action` `admin.audit`, `admin.audit_export`, `admin.users` for the team page,
`admin.connectors`, `admin.overview`), with the viewer as `actor`, `client` `web`, `task_id`
`admin`, and `outcome` `ok` or `denied`; no query values are recorded. These events show up in
the audit log like any other. `pmos-web` also logs each read (handle, route and the names of
the filters used, never their values).

### `GET /api/admin/audit`

Audit events, newest first, one page at a time.

| Parameter | Format | Meaning |
| --- | --- | --- |
| `limit` | integer 1–100, default 50 | Page size. |
| `before` | the `next_cursor` of the previous page | The next (older) page. **Opaque**: don't parse, build or keep it; send it back with the same filters. At most 128 characters of `A–Z a–z 0–9 . _ -`. |
| `actor` | a handle (`A–Z a–z 0–9 . _ -`, at most 64) | Only this person's events (a team bot's service identity is `bots.<name>`). |
| `action` | a tool name such as `jira.create_issue` (`A–Z a–z 0–9 . _ -`, at most 100) | Only this action. |
| `outcome` | one of the outcomes below | Only this outcome. |
| `persona` | a marketplace bot's key, such as `chief` (slice A1) | Only the events of that bot's sessions, drafts and decisions. `nudge` is `400 bad_request`: Nudge's events carry no key (filter by `actor` or `action`). |
| `from` | RFC 3339 time | Events at or after it. |
| `to` | RFC 3339 time | Events strictly before it. |

Unknown parameters are ignored; an empty value counts as absent. `400 bad_request` for a
`limit` outside 1–100 or not a number, an unknown `outcome`, a malformed time, `from` after
`to`, a `persona` that isn't a key (or is `nudge`), or an `actor`, `action` or `before` with
other characters. A `before` the tool server
doesn't recognise is also `400 bad_request`.

```json
{"events": [
  {"id": "ev_8812", "at": "2026-11-16T09:05:12.481Z", "actor": "ana", "client": "web",
   "action": "jira.create_issue", "system": "jira",
   "target": "https://acme.atlassian.net/browse/PAY-301", "outcome": "approved",
   "detail": "edited before approval", "sources": 3, "titled_sources": 2,
   "skill": null, "bot": "pm-ana", "task_id": "tsk_01JD…"},
  {"id": "ev_8809", "at": "2026-11-16T09:04:58Z", "actor": "ben", "client": "mcp",
   "action": "slack.post_reply", "system": "slack", "target": null, "outcome": "denied",
   "detail": null, "sources": 0, "titled_sources": 0,
   "skill": "reply", "bot": null, "task_id": "run_7c1…"}
 ],
 "next_cursor": "c1.MTc2MzI4MzQ5ODAwMDAwMDAwMDo4ODA5"}
```

`next_cursor` is `null` on the last page; a page is shorter than `limit` only when it is the
last. Every field below is always present.

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | Unique and stable; use it as a list key. Not an input anywhere. |
| `at` | string, RFC 3339 UTC | When it happened. |
| `actor` | string | The handle the event is about: the PM whose session or approval it was, the admin who read the log, who signed in or out. `anonymous` for a refused sign-in, `pmos-retention` for a purge. |
| `client` | string | Who made the call: `mcp` (a bot or skill run, from its sandbox), `orchestrator` (the bot runtime or a platform-owner tool), `bot` (the Slack app), `web` (this web app), `console` (the operator console), `system` (the tool server itself: the audit retention purge). |
| `action` | string | What was done: a tool name (`jira.search`, `confluence.create_draft`, `slack.post_reply`, `memory.write`, …), `credentials.unlink`, or `admin.*` for these reads. |
| `system` | string | The part of `action` before the first dot (`jira`, `confluence`, `slack`, `docs`, `context`, `memory`, `credentials`, `admin`, …). |
| `target` | string or `null` | What was written or decided: a page or issue URL, an approval id (`apr_…`), a memory note (`memory:…`), the provider of an unlink. `null` for reads and most refusals. |
| `outcome` | string | `ok` (ran), `denied` (refused: not allowed, above the data ceiling, wrong person), `pending` (a write queued for approval), `approved` (approved and written), `rejected`, `granted` (approved in the bot's own flow), `started` (an approved write about to run), `error` (failed), `failed` (an approved member couldn't be committed), `expired` (a Slack draft nobody decided; its text was removed). |
| `detail` | string or `null` | A short note, only when it is one of the fixed notes the tool server writes itself: `edited before approval`, `reason: <wrong_facts\|wrong_target\|tone\|incomplete\|should_not_draft\|other>`, `invalid draft (1 of 2)`, `approved in the inbox; writing`, `undecided after 24 h: …`, `purged 1200 events older than <time>` (a purge), `provider: entra; reason: not_a_member; attempt: <hex>` (a sign-in event) and a few more. Otherwise `null`: a refusal's or a connector's message can quote arguments or content, so it is never served. |
| `sources` | integer ≥ 0 | How many items the call read (context files, Jira issues, pages, Slack messages). The items are not served. |
| `titled_sources` | integer ≥ 0 | How many of those had a title recorded. **Only the count:** a title is a Jira issue's summary or a page's title, which can carry content, so titles are never served. |
| `skill` | string or `null` | The run's skill, for a skill session. |
| `bot` | string or `null` | The run's bot, for a bot session. |
| `persona` | string or `null` | The marketplace bot the event belongs to, by key (slice A1): a persona session's calls, its drafts, their decisions and releases. `null` for Nudge and for events of no bot; a value not shaped like a key is served as `null`. |
| `task_id` | string | The run: a task id (`tsk_…`) for bot tasks, a run id otherwise, `admin` for admin reads. Not a link: an auditor can't open tasks. |

### `GET /api/admin/audit/export`

The same events as CSV, newest first. Takes the same filters as `GET /api/admin/audit`
(`from`, `to`, `actor`, `action`, `outcome`, `persona`, with the same `400`s), not `limit` or
`before`.
Needs `export-audit`.

- `200`, `Content-Type: text/csv; charset=utf-8`,
  `Content-Disposition: attachment; filename="pmos-audit.csv"`.
- At most **10,000 rows**. `X-Pmos-Truncated: true` when more matched and were left out (narrow
  `from` and `to` and export again), `false` otherwise. `X-Pmos-Row-Count` is the number of
  rows, the header row not counted.
- A header row, then one row per event, every line ending `\r\n`. Columns, in order: `at`,
  `actor`, `client`, `action`, `system`, `target`, `outcome`, `detail`, `sources`,
  `titled_sources`, `skill`, `bot`, `task_id`, `id`, `persona` (the fields above; `null` is an
  empty cell). `persona` (slice A1) is last, so the columns before it keep their places.
- Every cell is quoted, with `"` doubled. **Formula-safe:** a cell starting with `=`, `+`, `-`,
  `@`, a tab or a carriage return gets a leading `'`, so a spreadsheet shows it as text.
- Errors are JSON, as above.

```
"at","actor","client","action","system","target","outcome","detail","sources","titled_sources","skill","bot","task_id","id","persona"
"2026-11-16T09:05:12.481Z","ana","web","jira.create_issue","jira","https://acme.atlassian.net/browse/PAY-301","approved","edited before approval","3","2","","pm-ana","tsk_01JD…","ev_8812","chief"
```

The app downloads it with `fetch` (the session cookie goes along) and saves the blob, or links
to it directly.

### `GET /api/admin/team`

The Team page's member list, kept in its first shape for the app until the Team screen moves
to `GET /api/admin/users` (**Admin: users and roles**). Since slice U5a it comes from the user
database (the tool server's `GET /internal/admin/users`), not `team.yaml`: **active** users
only, by handle, and no addresses for anyone. When the tool server can't answer, the page
fails like every admin read (`502 upstream_unavailable`); there is no file to fall back to.

```json
{"members": [
  {"handle": "ana", "name": "Ana Silva", "roles": ["pm"], "has_email": true, "bot": "pm-ana",
   "accounts": {"atlassian": true, "slack": true, "github": false}},
  {"handle": "audrey", "name": "audrey", "roles": ["auditor"], "has_email": false,
   "bot": "pm-audrey", "accounts": {"atlassian": false, "slack": false, "github": false}}
 ],
 "accounts_available": true,
 "linking": true}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `handle` | string | The user's handle. |
| `name` | string | Their display name (untrusted text: render it plain), or the handle when there is none. |
| `roles` | string[] | Their roles in the user database. |
| `has_email` | boolean | Whether a Microsoft sign-in name is on file (`has_sign_in_name`). The address is not served. |
| `bot` | string | Their personal bot's name. It may not exist yet. |
| `accounts` | object | Whether each account is linked, as booleans: `atlassian` (Jira and Confluence share one Atlassian link), `slack`, `github`. Never a token, scope, site or account id. |
| `accounts_available` | boolean | Always `true` now (kept for the app). |
| `linking` | boolean | Whether account linking is on (live connectors). With fake connectors nothing can be linked and every account is `false`. |

### `GET /api/admin/connectors`

Admins only. Each connector, checked now.

```json
{"connectors": [
  {"name": "confluence", "mode": "live", "health": "ok", "error_class": null,
   "checked_at": "2026-11-16T09:10:00.120Z"},
  {"name": "docs", "mode": "live", "health": "down", "error_class": "timeout",
   "checked_at": "2026-11-16T09:10:00.120Z"}
 ]}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `name` | string | The system: `confluence`, `docs`, `jira`, `slack`, sorted. |
| `mode` | string | `live` (the real system) or `fake` (fixtures, `PMOS_CONNECTORS=fake`). |
| `health` | string | `ok` or `down`. Jira, Confluence and Slack act only with each PM's own token, so they have no shared dependency to check and read `ok`; a PM's broken link shows in their own onboarding status, not here. The support docs server is really checked. |
| `error_class` | string or `null` | When `down`: `timeout`, `auth`, `unreachable` or `error`. Never the error's text (that goes to the tool server's log). `null` when `ok`. |
| `checked_at` | string, RFC 3339 UTC | When the check ran. Every request checks afresh, at most 2 seconds per connector; there is no stored "last error". |

### `GET /api/admin/overview`

Counts for the admin home.

```json
{"since": "2026-11-09T09:10:00.120Z", "pending_drafts": 4, "held_drafts": 1,
 "approved": 37, "rejected": 5, "denied": 2,
 "retention_days": 90, "oldest_event_at": "2026-08-19T03:07:41.500Z",
 "last_purge": {"at": "2026-11-17T03:07:12.004Z", "deleted": 1200},
 "users": {"active": 9, "deactivated": 1,
           "sign_in_unconfirmed": ["sam"], "dual_role": ["raj"],
           "pilot_exception": {"ends": "2026-12-31", "ending_soon": false, "lapsed": false},
           "role_conflicts": [], "entra_needs_host": [], "no_slack": [],
           "identity_conflicts": 1, "identity_conflict_users": ["ana"],
           "resolve_miss_warning": [], "resolve_miss_warning_threshold": 100,
           "deactivations_last_hour": 1, "deactivation_limit": 3,
           "deactivation_limit_reached": false, "deactivation_grace_hours": 24,
           "bots_without_active_owner": [],
           "first_links": [{"handle": "ben", "at": "2026-11-16T08:02:11.000Z", "provider": "entra"}],
           "password_resets": [{"handle": "ana", "by": "ada", "at": "2026-11-16T08:00:00Z",
                                "dismissed_at": null}]}}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `since` | string, RFC 3339 UTC | Seven days before now: the window of the three counts below. |
| `pending_drafts` | integer | Drafts waiting for a decision now, across every active user. |
| `held_drafts` | integer or `null` | Pending drafts of deactivated people, held for the 24-hour grace (then closed). `null` from an older tool server. |
| `approved` | integer or `null` | Approved writes that ran since `since` (audit outcome `approved`). |
| `rejected` | integer or `null` | Drafts rejected since `since`. |
| `denied` | integer or `null` | Actions refused since `since` (audit outcome `denied`, admin reads refused included). |
| `retention_days` | integer or `null` | How many days audit events are kept before the daily purge deletes them (`PMOS_AUDIT_RETENTION_DAYS`: 90, or more if set). `null` from a tool server older than retention. Show as "Audit events are kept 90 days". |
| `oldest_event_at` | string, RFC 3339 UTC, or `null` | The oldest event still in the audit log. `null` when the log is empty or can't be read back. Right after a purge it is at most `retention_days` old (plus up to a day between runs). |
| `last_purge` | object or `null` | The newest purge batch: `{"at": RFC 3339 UTC, "deleted": integer ≥ 0}`, when it ran and how many events it deleted (`0` is a normal day). `null` before the first purge, or when the log can't be read back. A run of several batches shows its last batch; each batch is also in the audit list as `audit.purge`. |
| `users` | object or `null` | The user database's flags (below). `null` from an older tool server. Handles and counts only: never an address, a Slack id or an object id. |

`users`, what needs an admin (spec §2, §4, §6, §7):

| Field | Type | Show |
| --- | --- | --- |
| `active`, `deactivated` | integer | How many users of each status. |
| `sign_in_unconfirmed` | string[] | "Joined from Slack, Microsoft sign-in not confirmed": active people whose sign-in name an admin must confirm before their first Microsoft sign-in. |
| `dual_role` | string[] | "Admin and Auditor (pilot exception, ends <date>)": active people holding both. |
| `pilot_exception` | object or `null` | `{ends: "YYYY-MM-DD", ending_soon, lapsed}`. `ending_soon` (14 days or less): "The pilot exception ends on <date>". `lapsed`: past its end with nobody switching it off. `null` when off. |
| `role_conflicts` | string[] | When the exception has lapsed: a **red notice** naming everyone still holding both ("their audit-log reads are refused until one role is removed"). Empty otherwise. |
| `entra_needs_host` | string[] | Admins and Auditors whose Microsoft account the host hasn't confirmed (they can't sign in with Microsoft). |
| `no_slack` | string[] | Active PMs with no Slack id. |
| `identity_conflicts` | integer or `null` | Refused Microsoft sign-ins in the last 7 days that presented someone's sign-in name with a different Microsoft account (`users.identity_conflict`). `null` when the audit log can't be read back. |
| `identity_conflict_users` | string[] or `null` | Whose sign-in names those were: "Someone signed in with Microsoft as <handle>'s sign-in name using a different Microsoft account". An admin may need to unlink the wrong account (`DELETE /api/admin/users/{handle}/identities/entra`). |
| `resolve_miss_warning` | string[] | Services (`web`, `console`) whose lookups found nobody more than `resolve_miss_warning_threshold` times in the last hour: someone may be guessing names with that service's token. Counted in the tool server's memory since it started. |
| `resolve_miss_warning_threshold` | integer | 100. |
| `deactivations_last_hour`, `deactivation_limit`, `deactivation_limit_reached` | integer, integer, boolean | The hourly cap (**Admin: users and roles**). |
| `deactivation_grace_hours` | integer | 24. |
| `bots_without_active_owner` | string[] | Bots whose owner is not an active user who may use PM OS. |
| `first_links` | object[] or `null` | **New (review of `b469d5b`, U1).** Everyone who signed in with Microsoft for the first time in the last 7 days, newest first: `{handle, at (RFC 3339 UTC), provider: "entra"}`. "N people signed in with Microsoft for the first time in the last 7 days"; if one wasn't expected, unlink it on the Team page. Never an object id. `null` when the audit log can't be read back (or from an older tool server). |
| `password_resets` | object[] | Reset codes made in the window for people who had a password: `handle`, `by` (an admin's handle, or `host`), `at`, and `dismissed_at`: when the person's notice of it was dismissed (from their own session), `null` while it shows. Read by time, however many other role changes there were. Each person also sees a notice until they dismiss it. An older server leaves it out. |

The three counts are `null` when the tool server's audit log can't be read back.
**Purge runs in the audit list:** action `audit.purge`, system `audit`, actor
`pmos-retention`, client `system`, task `retention`, outcome `ok`, target `null`, detail
`purged 1200 events older than 2026-08-19T03:07:12Z`. `client` can therefore also be
`system` (the tool server itself) in the list below. See `docs/audit-retention.md`. **Tasks by
status are not here:** the bot runtime gives the `web` client only the signed-in PM's own
tasks, with no team-wide read, so `pmos-web` can't count them.

### Known limits

- **The tool server's own role check believes the named viewer only as far as
  `PMOS_ASSERTION_MODE` does.** Each admin call names the viewer (`user=`) and carries a user
  assertion for them, and the tool server checks that viewer's role in its own user database.
  With `require`, a call without a valid assertion for that viewer is refused. With `off` or
  `verify`, a call with no assertion is believed, so a stolen `PMOS_WEB_TOKEN` could read the
  audit log by naming an admin. Run `require` in production.
- **A read refused by `pmos-web` itself** (a `400`, or its own `403`) never reaches the tool
  server, so it is only in `pmos-web`'s log, not the audit log.
- **No free-text search**; `actor` and `action` match exactly.

## Admin: bots

The Admin's view of the bot marketplace (slice A1; `docs/bot-marketplace-spec.md` §8, "The
admin view"): every bot in the catalog, offered or not, who has it on their team, and the
drafts on hold because their bot was withdrawn. Offering or withdrawing a bot is a content
change by pull request (`offered` in its `PERSONA.md`, through **Admin: content and harness**);
nothing here changes the catalog or anyone's team. The one write is releasing a held draft.

| Method and path | Who | Answer |
| --- | --- | --- |
| `GET /api/admin/bots` | `view-audit-log` or `manage-members` (Admin, Auditor); others `403 forbidden` before the tool server is asked | `AdminBots` (below). Read from the tool server's `GET /internal/admin/personas?user=<viewer>`, signed for the viewer, which checks the role again and records `admin.personas_read`. `502 upstream_unavailable` when it can't be read. |
| `POST /api/admin/bots/drafts/{id}/release` | `edit-content` (an Admin); others `403 forbidden` before the tool server is asked. No body. Origin and CSRF as every write | `{id, released: true}`, never the draft. Passed to `POST /internal/admin/personas/drafts/{id}/release?user=<admin>`, signed for the admin, which checks again (an active person with `edit-content`), needs assertion `require`, and records `approvals.release_hold`. Errors: `404 not_found` (a malformed or practice id is never sent), `409 not_held`, `403 forbidden`, `503 assertion_required`, `503 unavailable`. |

```json
// AdminBots
{"bots": [
   {"key": "chief", "display_name": "Chief", "mascot": "chief",
    "state": "preview", "offered": true, "always_on": false, "preview": true,
    "users": 4, "handles": ["ana", "ben", "dana", "ravi"],
    "needs": [{"source": "analytics", "label": "an analytics source", "met": false}],
    "drafts_30d": 23, "name_clashes": [],
    "held": [],
    "path": "content/personas/chief/PERSONA.md",
    "lint": {"status": "ok", "errors": 0, "warnings": 0}},
   {"key": "scout", "display_name": "Scout", "mascot": "nudge", "state": "withdrawn", …,
    "held": [{"id": "apr_…", "user": "ana", "kind": "confluence_page",
              "tool": "confluence.create_draft", "created_at": "2026-11-21T08:00:00Z"}]}],
 "unknown": [{"key": "atlas", "users": 1, "handles": ["ravi"], "drafts_30d": 0, "held": []}],
 "held_total": 1, "can_release": true, "can_edit_content": true}
```

- `bots` is the catalog in its order, Nudge first. `state` is `withdrawn` when `offered` is
  `false`, else `preview` when `preview` is `true` ("not yet benchmarked"), else `offered`.
  Nudge's `users` and `handles` are `null` (it is on every team).
- `handles` lists every person with a team row for the bot, a deactivated person's included;
  `name_clashes` the handles of people whose handle or display name reads as the bot's key or
  name (a Slack prefix could be ambiguous).
- `held` lists the bot's drafts on hold, newest first: id, owner, kind (`null` for a tool that
  isn't a draft kind) and when it was drafted. **Never a title, text or arguments.** At most 200
  are listed in all; `held_total` counts every one.
- `path` is the bot's file for the content admin; `lint` is that file's lint status in
  `pmos-web`'s content checkout (counts only, never a finding's message), `null` when the
  checkout can't be read.
- `unknown` lists keys that team rows or held drafts still name but the catalog no longer has
  (a deleted `PERSONA.md`): the rows grant nothing.
- `display_name` and `needs[].label` are catalog content: shown as plain text.
- `can_release` and `can_edit_content` say whether the viewer holds `edit-content`.

**What releasing means.** The draft's owner may then approve or edit it like any other draft,
under their own personal bot's rules (the decision checks the item's bot and the personal role,
never the withdrawn bot). Nothing is sent until they approve. It is one draft, once: a release
can't be undone, and the bot stays withdrawn.

## Admin: users and roles

The Team screen's routes (slice U5a of `docs/user-database-spec.md`; the screen itself is
U5b). The user database belongs to the tool server; `pmos-web` mirrors its Admin API
(`docs/toolserver.md`, "The Admin API") under `/api/admin/users…` and `/api/admin/reviewers/…`,
naming the signed-in person as the actor and signing a user assertion for them bound to the
exact request. **The tool server decides every write** in its own tables (never from roles the
app or `pmos-web` believe); what `pmos-web` checks first is a convenience that saves a round
trip.

| Method and path | Body | Who (`pmos-web`'s first check) | Answer |
| --- | --- | --- | --- |
| `GET /api/admin/users` | | `manage-members` or `view-audit-log` (admin, auditor) | `200` `UserList` |
| `POST /api/admin/users` | `NewUser` | `manage-members` (admin) | `201` `Written` |
| `PUT /api/admin/users/{handle}/roles` | `{roles, expected_version}` | admin | `200` `Written` |
| `PUT /api/admin/users/{handle}/fields` | `{sign_in_name?, atlassian_email?, github_login?, display_name?, expected_version}` | admin | `200` `Written` |
| `POST /api/admin/users/{handle}/sign-in-name/confirm` | `{sign_in_name, expected_version?}` | admin | `200` `Written` |
| `POST /api/admin/users/{handle}/deactivate` | none, `{}` or `{expected_version}` | admin | `200` `Written`, with `note` |
| `POST /api/admin/users/{handle}/reactivate` | none, `{}` or `{expected_version}` | admin | `200` `Written` |
| `DELETE /api/admin/users/{handle}/identities/{kind}` | none | admin | `200` `Written` |
| `PUT /api/admin/reviewers/{reviewer}` | `{pms, expected_version?}` | admin | `200` `Written` (the reviewer) |
| `POST /api/admin/users/{handle}/password-setup-code` | none or `{}` | admin | `201` `SetupCode`, shown once (**Password sign-in**) |

Every write is a `POST`, `PUT` or `DELETE` and so needs the session's CSRF token and the app's
own `Origin`, as every write does (**Sign-in and safety**). Any other method on these paths is
`405 method_not_allowed`. A `{handle}` or `{reviewer}` that can't be a handle (lowercase
letters, digits, `.`, `_`, `-`, at most 64) is `404 not_found`, and so is a `{kind}` other than
`entra`, `atlassian` or `github`, all without an upstream call. A write body that isn't a JSON
object is `400 bad_request`; a field a route doesn't take is `400 bad_request` from the tool
server (every body is exact).

### Who may do what

The tool server's rules (spec §6), each a refusal code below:

- **Only an active admin writes** (`403 forbidden`). An auditor reads the list and nothing
  else; a PM or reviewer gets `403 forbidden` for the list too.
- **Nobody changes their own roles, status or addresses**, in any direction (`403
  self_change`): an admin can't change their own roles, deactivate or reactivate themselves,
  change their own sign-in name, Atlassian email or GitHub login, unlink their own accounts,
  or assign themselves PMs to review. An admin *may* change their own display name.
- **`admin` is never granted from the web** (`403 host_only`), in an invite or a role change.
  Revoking it from someone else is fine. Granting it is `pmos-admin users grant-admin` on the
  host.
- **An Admin's or Auditor's addresses and Microsoft link change on the host only** (`403
  host_only`): their sign-in name, Atlassian email, GitHub login, the sign-in name
  confirmation, and unlinking their `entra` identity. `host_only` in each user says so in
  advance. Their display name and roles (but `admin` granted) are the web's to change.
- **The last admin:** PM OS always keeps an active admin (`409 last_admin`). Through the web
  this can't happen, since nobody changes their own roles: two admins demoting each other
  leave the second refused `403 forbidden` (they are no longer an admin).
- **Admin and Auditor together** only while the host's pilot exception is in force (`409
  dual_role`). Once it has lapsed, any change that keeps someone holding both is refused, but
  revoking either role or deactivating them always works, and their admin powers stay
  meanwhile, so the team keeps a working admin.
- **Addresses** (spec §6 "When addresses may change"): a sign-in name may be set any time
  (it is informational once their Microsoft account is linked); setting it also confirms it.
  An Atlassian email or GitHub login changes only while no account of that kind is linked
  (`409 linked`): after the first link the account id decides; unlink first. Addresses are
  unique across everyone, deactivated people included, and never reused (`409 taken`).
- **Deactivation** is capped and reversible (below), refused while the person owns a team bot
  (`409 owns_bots`), and never restores `admin` when reactivated.

### `expected_version`

`GET /api/admin/users` returns the directory's `version` (an integer that goes up with every
change to anyone's user, roles, keys or reviewer assignments). Send it back as
`expected_version`: **required** on `PUT …/roles` and `PUT …/fields` (`400 bad_request`
without it), optional on the others. When the directory moved since, a write that would
otherwise go through is refused `409 stale` and nothing changes: reload the list, show the
person what is there now, and let them try again. A refusal that doesn't depend on what moved
(`forbidden`, `self_change`, `host_only`, …) is answered as itself even with an old version.
Every successful write answers the new `version`; use it for the next write.

### `UserList`

```json
{"version": 42, "view": "admin",
 "users": [
   {"handle": "ana", "display_name": "Ana Silva", "status": "active", "roles": ["pm"],
    "created_at": "2026-11-17T10:00:00Z", "deactivated_at": null, "grace_ends_at": null,
    "dual_role": false, "role_conflict": false,
    "has_sign_in_name": true, "sign_in_name_confirmed": true, "sign_in_unconfirmed": false,
    "entra": "linked", "entra_needs_host": false,
    "slack_linked": true, "entra_linked": true, "atlassian_linked": true, "github_linked": false,
    "accounts": {"atlassian": true, "slack": true, "github": false},
    "host_only": false, "bot": "pm-ana", "team_bots": ["scout"],
    "reviews": [], "reviewed_by": ["rex"],
    "password_set": true, "second_step_enrolled": true, "setup_code_pending": false,
    "sign_in_name": "ana@contoso.com", "atlassian_email": "ana@contoso.com",
    "github_login": null}
 ],
 "deactivations": {"last_hour": 1, "limit": 3, "remaining": 2, "grace_hours": 24},
 "pilot_exception": {"ends": "2026-12-31", "ends_at": "2026-12-31T00:00:00Z",
                     "ending_soon": false, "lapsed": false},
 "linking": true}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `version` | integer | The directory's version; send it back as `expected_version`. |
| `view` | `"admin"` or `"auditor"` | `admin` for a viewer with `manage-members`: the addresses are included. `auditor` otherwise: they are left out (the keys are absent, not `null`). |
| `users` | `User[]` | Every user, active or deactivated, by handle. |
| `deactivations.last_hour` | integer | Deactivations through the web and Slack in the last rolling hour, counted from the role history: refused attempts never count, and a reactivation doesn't take one back. |
| `deactivations.limit` | integer | The cap (`PMOS_USERS_DEACTIVATIONS_PER_HOUR`, 1–10, default 3). |
| `deactivations.remaining` | integer | `limit - last_hour`, never below 0. At 0 a deactivation is `429 deactivation_limit` until the oldest leaves the hour; say "Someone with host access can continue with `pmos-admin users deactivate`". |
| `deactivations.grace_hours` | integer | 24: how long a deactivation stays reversible (below). |
| `pilot_exception` | object or `null` | The host's Admin + Auditor exception, when one is on (lapsed or not). `ends` (`YYYY-MM-DD`, UTC) and `ends_at` (RFC 3339): when it ends. `ending_soon`: in force and ends within 14 days ("The pilot exception ends on <date>"). `lapsed`: past `ends_at` and nobody switched it off: every person holding both has `role_conflict: true`. `null` when off. |
| `linking` | boolean | Whether account linking is on (live connectors); with fake connectors no account can be linked. |

`User`:

| Field | Type | Meaning |
| --- | --- | --- |
| `handle` | string | Their handle. Never changes. |
| `display_name` | string | **Untrusted text** (often from Slack): render as plain text only. `""` when none. |
| `status` | `"active"` or `"deactivated"` | |
| `roles` | string[] | Of `admin`, `pm`, `reviewer`, `auditor`, sorted in that order. A deactivated person's kept roles grant nothing. |
| `created_at` | string, RFC 3339 | When the user was created. |
| `deactivated_at` | string or `null` | When they were deactivated; `null` while active. |
| `grace_ends_at` | string or `null` | `deactivated_at` + 24 hours: until then a reactivation restores their linked accounts and held drafts; after it, their accounts are deleted and their held drafts closed, and a reactivation brings back only the person and their kept roles. `null` while active. |
| `dual_role` | boolean | Holds both Admin and Auditor (the pilot exception). Show the badge. |
| `role_conflict` | boolean | Active, holds both, and the pilot exception has lapsed: their audit-log reads are refused until one role goes. |
| `has_sign_in_name` | boolean | A Microsoft sign-in name is on file. |
| `sign_in_name_confirmed` | boolean | An admin (or the host, or the import) confirmed it. |
| `sign_in_unconfirmed` | boolean | Active, with a sign-in name that came through Slack and nobody confirmed: they can't sign in with Microsoft until an admin confirms it ("Confirm Microsoft sign-in name"). |
| `entra` | `"none"`, `"linked"` or `"needs_host"` | Their Microsoft account link. `needs_host`: an Admin or Auditor whose link the host hasn't confirmed (they can't sign in until it does). |
| `entra_needs_host` | boolean | `entra == "needs_host"`. |
| `slack_linked`, `entra_linked`, `atlassian_linked`, `github_linked` | boolean | Whether that key is on file (a Slack member id; a Microsoft account; the Atlassian or GitHub account id of their first link). Never the key itself. |
| `accounts` | object or `null` | Whether each account's token is linked now: `atlassian`, `slack`, `github`, booleans. `null` when it couldn't be read (writes' answers only). |
| `host_only` | boolean | Holds Admin or Auditor: their addresses and Microsoft link change on the host only. |
| `bot` | string | Their personal bot's name. It may not exist (an invited person gets none until one is added). |
| `team_bots` | string[] | Team bots they own: they can't be deactivated until each has another owner. |
| `reviews` | string[] | For a reviewer: the PMs assigned to them, sorted. |
| `reviewed_by` | string[] | The reviewers this person is assigned to. |
| `password_set` | boolean | They have a password for password sign-in (**Password sign-in**). An older server leaves it out: treat as `false`. |
| `second_step_enrolled` | boolean | They have an authenticator for its second step (with a password, always). |
| `setup_code_pending` | boolean | A setup or reset code was made for them and is unused and unexpired. |
| `password_needs_host_setup` | boolean | They hold Admin or Auditor and their password came from a code made in the web app: it doesn't work until someone with host access makes them a setup code. An older server leaves it out. |
| `sign_in_name` | string or `null` | **Admin view only.** Lower-case. |
| `atlassian_email` | string or `null` | **Admin view only.** Lower-case. |
| `github_login` | string or `null` | **Admin view only.** |

**An auditor never gets an address, a Microsoft object id or a Slack id**: the three address
fields are absent from every user. Nobody gets an object id, a tenant id, a Slack id or another
account id from these routes. The tool server records every list read as `admin.users`.

### `NewUser` (invite)

`{"handle": "cy", "display_name": "Cy Young", "sign_in_name": "cy@contoso.com",
"atlassian_email": "cy@contoso.com", "github_login": "cy-gh", "slack_id": "U0CY0001",
"roles": ["pm", "reviewer"]}`

| Field | Type | Rules |
| --- | --- | --- |
| `handle` | string, required | Lowercase letters, digits, `.`, `_`, `-`, at most 64, starting with a letter or digit; not `bots.…`, `host`, `import` or `system` (`422 invalid`). Never reused (`409 exists`). |
| `display_name` | string, optional | Cleaned: control characters, `<` and `>` removed, spaces collapsed, at most 100 characters. Default `""`. |
| `sign_in_name` | string, required | An email address (the Microsoft UPN), stored lower-case; confirmed by this invite. |
| `atlassian_email` | string, required | An email address, stored lower-case. |
| `github_login` | string, optional | `A–Z a–z 0–9 -`, 1–39. `null` or `""` means none. |
| `slack_id` | string, optional | A Slack member id (`U…` or `W…`). `null` or `""` means none. |
| `roles` | string[], required | At least one of `pm`, `reviewer`, `auditor` (`422 invalid` for none or an unknown name); `admin` is `403 host_only`. |

The person is `active` at once and can sign in with Microsoft (their sign-in name is
confirmed). No personal bot is created (`bot` names the one they would have).

### `Written`

Every write answers the person as they are now and the directory's new version:

```json
{"version": 43, "user": { …a User, admin view… },
 "note": "`ana` is deactivated: …"}
```

`user` is the target (the reviewer, for `PUT /api/admin/reviewers/{reviewer}`), in the admin
view. `note` is only on a deactivation: a sentence for the admin saying what happened and that
the personal bot's sandbox keeps idling until `pmos-admin pause pm-<handle>` (show it as plain
text).

### Each write

- **`PUT …/roles`** `{"roles": ["pm", "reviewer"], "expected_version": 42}`: the whole set
  (`[]` removes every role). Unknown names `422 invalid`; adding `admin` `403 host_only`; the
  same set `409 unchanged`. Granting `auditor` to an admin needs the pilot exception
  (`409 dual_role`). Granting `auditor` to someone whose Microsoft account linked itself
  leaves it `needs_host` until the host confirms it.
- **`PUT …/fields`** `{"display_name": "Ana S.", "expected_version": 42}`: any of
  `sign_in_name`, `atlassian_email`, `github_login`, `display_name`, at least one (`422
  invalid` for none); a field left out is unchanged; none can be cleared here. All or
  nothing. Values that are all already so are `409 unchanged`. A sign-in name set here is
  confirmed too.
- **`POST …/sign-in-name/confirm`** `{"sign_in_name": "ana@contoso.com"}`: confirms the
  stored name, or replaces it and confirms (spec §4: "editable before confirming"). The same
  name, already confirmed, is `409 unchanged`. Only then can a person who joined through Slack
  sign in with Microsoft for the first time.
- **`POST …/deactivate`**: at once, their sessions end (PM OS's tool sessions now, the web and
  console within 10 seconds, Slack and bots within 30), their linked accounts are suspended
  (unusable, not deleted), their pending drafts are held (not decided, not rejected), and
  `admin` is revoked if they held it. Capped: at most `deactivations.limit` in any rolling
  hour from the web and Slack together (`429 deactivation_limit`); the host is not capped.
  `409 owns_bots` while they own a team bot; `409 unchanged` if already deactivated.
- **`POST …/reactivate`**: `active` again with their kept roles, **never `admin`** (only the
  host grants it). Within `grace_ends_at` their linked accounts and held drafts come back;
  after it they must link their accounts again and their held drafts are gone. `409
  unchanged` if active.
- **`DELETE …/identities/{kind}`**: removes their `entra` link (a reassigned mailbox: the next
  Microsoft sign-in with their sign-in name links afresh), or their `atlassian` or `github`
  account id **and that linked account**, so another account can be linked (and the email or
  login then edited). Their web sessions keyed on that Microsoft account end within 10
  seconds. `409 unchanged` when nothing of that kind is linked; an Admin's or Auditor's
  `entra` is `403 host_only`.
- **`PUT /api/admin/reviewers/{reviewer}`** `{"pms": ["ana", "ben"]}`: the whole set of PMs
  assigned to the reviewer (spec §8; at most 200). The reviewer must be active and hold
  `reviewer` (`409 not_active`, `409 not_reviewer`); each PM added must be active and hold
  `pm` (`409 not_active`, `409 not_pm`); nobody reviews themselves, and a PM appears once
  (`422 invalid`); an unknown handle is `404 not_found`. `[]` clears every assignment and is
  always allowed. Assignments are kept, and grant nothing, when either person loses the role
  or is deactivated. (What a reviewer then does: **Reviews**.)

### Errors

`{"code", "error"}` as everywhere; `error` is `pmos-web`'s own sentence, never the tool
server's. Every refusal below changed nothing, and the tool server recorded it in the audit
log (a `denied` event with the code as its detail) when it reached it.

| Status | `code` | When |
| --- | --- | --- |
| `400` | `bad_request` | The body isn't a JSON object, has a field the route doesn't take, or lacks a required one (`expected_version` on roles and fields). |
| `401` | `signed_out` | No live session. |
| `403` | `bad_origin`, `bad_csrf` | A write without the app's origin or CSRF token. |
| `403` | `forbidden` | Not an admin (for the list: not an admin or auditor). `pmos-web` answers it itself when the person's roles don't allow it; the tool server also answers it from its own tables. |
| `403` | `host_only` | Granting `admin`; an Admin's or Auditor's addresses or Microsoft link. |
| `403` | `self_change` | The actor's own roles, status, addresses, links or reviewer assignments. |
| `404` | `not_found` | No such person (or PM, in an assignment), or a path that can't name one. |
| `409` | `exists` | Invite: the handle is taken. |
| `409` | `taken` | An address or account that belongs to someone else. |
| `409` | `unchanged` | Nothing to change. |
| `409` | `stale` | `expected_version` isn't the directory's version any more. |
| `409` | `last_admin` | The change would leave no active admin. |
| `409` | `dual_role` | The change would leave someone holding Admin and Auditor without a pilot exception in force. |
| `409` | `evals_account` | The `evals` account holds `pm` only and is never linked. |
| `409` | `not_active` | A reviewer assignment names someone deactivated. |
| `409` | `linked` | An Atlassian email or GitHub login while that account is linked. |
| `409` | `owns_bots` | Deactivating someone who owns a team bot; `error` names none of them, `team_bots` does. |
| `409` | `not_reviewer`, `not_pm` | A reviewer assignment: the reviewer lacks `reviewer`, or a PM lacks `pm`. |
| `422` | `invalid` | A malformed value: a handle, an address, a GitHub login, a Slack id, an unknown role, no role on an invite, no field to change, a PM twice or the reviewer themselves. |
| `429` | `deactivation_limit` | The hourly deactivation cap is reached. |
| `502` | `upstream_unavailable` | The tool server couldn't be reached, or answered something `pmos-web` doesn't know. Nothing changed. |
| `502` | `unknown_outcome` | A write was sent and its answer was lost: it may have happened. Reload the list. |
| `503` | `assertion_required` | The tool server isn't running with signed user assertions (`PMOS_ASSERTION_MODE=require`), so it makes no user or role change (development stacks with `PMOS_ASSERTION_INSECURE_OFF=1`). Reads still work. |
| `503` | `unavailable` | The user database is down. |
| `503` | `users_unavailable` | The tool server can't be asked who the signed-in person is. |

### What gets recorded

Every write, allowed or refused, is an audit event (`client` `web`, `task_id` `users`, the
admin as `actor`, the target handle as `target`), and every allowed change is also a row of
the user database's own role history:

| `action` | `detail` |
| --- | --- |
| `users.create` | `roles pm,reviewer` |
| `users.roles` | `+reviewer -pm`; `-admin on deactivation` |
| `users.fields` | the field names changed (`sign_in_name; display_name`), `sign_in_name confirmed`; never a value |
| `users.deactivate` | `credentials suspended` |
| `users.reactivate` | `credentials restored`, or `credentials deleted` after the grace |
| `users.identity_unlink` | `entra`, `atlassian` or `github` |
| `reviewers.assign` | `<n> pms` (how many the reviewer has now) |
| any of these, `denied` | the refusal's code (`self_change`, `stale`, `deactivation_limit`, …), or `bad_request`, `bad_assertion`, `assertion_required`, `owns_bots` |

`GET /api/admin/users` is recorded as `admin.users` (allowed or refused).

## Reviews

A **Reviewer recommends; the draft's owner confirms** (`CLAUDE.md`; slice U6a of
`docs/user-database-spec.md` §8; the screens are U6b, built from this section). A person
holding the `reviewer` role reads the **pending** drafts of the PMs an admin assigned to them
(**Admin: users and roles**, `PUT /api/admin/reviewers/{reviewer}`) and records a
recommendation on one: **approve** or **changes**, with an optional note. A recommendation
**never decides anything**: the draft stays pending, nothing is sent anywhere, and only its
owner approves or rejects it (`POST /api/drafts/{id}/approve`, the Slack card). The owner sees
the advice on their draft (`recommendations`, **Drafts**), marked when it was given on an
earlier version. Recommending happens in this app only: Slack shows one line and has no button
for it.

`pmos-web` mirrors the tool server's reviewer routes (`docs/toolserver.md`, "Reviews"), naming
the signed-in person as `user=` and signing a user assertion for them bound to the exact request.
**The tool server decides** who the reviewer is, whom they review and what they may see, from
its own tables, on every call.

| Method and path | Body | Answer |
| --- | --- | --- |
| `GET /api/reviews` | | `200` `{drafts: [ReviewSummary], total, truncated, pms}` |
| `GET /api/reviews/{id}` | | `200` `ReviewDraft` |
| `POST /api/reviews/{id}/recommendation` | `{verdict, note?, revision, args_hash}` | `200` `{recommendation: MyRecommendation, draft: ReviewDraft}` |

**Who.** Show the Reviews entry when `GET /api/me` says `reviewer: true`. Anyone else gets
`403 forbidden` from `pmos-web` itself, with no upstream call (an admin too: holding `admin` is
not reviewing). The tool server checks again, on every call: an **active** user holding
`reviewer`. It reads no one's draft for them but those of the PMs assigned to them who are
active (a deactivated PM's held drafts are nobody's to review).

**Which drafts.** `GET /api/reviews` lists every pending draft of the assigned PMs, newest
first (up to the tool server's safety cap of 1,000; `truncated` then says some were left out,
and `total` counts them all). Another person's draft, a missing one, and a draft whose owner
isn't assigned to this reviewer all answer `404 not_found`, the same way, so the app can't
learn which ids exist. A draft of **their own** answers `403 own_draft`: they decide it as its
owner, under **Drafts**. A draft decided meanwhile is `409 not_pending`; a Slack draft past its
24 hours, `409 expired`. An `{id}` that can't be one is `404 not_found` with no upstream call.

**A restricted draft is metadata only** (`restricted: true`): built from restricted sources, or
from unrecorded ones (older drafts), which count the same; that includes a follow-up whose
conversation read restricted content, even if the follow-up itself read nothing. The reviewer
sees who, what kind, when, and that it is restricted: no title, body, target, sources or
version, and it can't be recommended on (`403 restricted_draft`). Show "This draft uses
restricted sources; its owner can share it with you." `pmos-web` treats any draft the tool
server doesn't mark `restricted: false` as restricted.

```json
// ReviewSummary (restricted: title and my_recommendation are null, no args_hash, no edited)
{"id": "apr_…", "owner": "ana", "owner_name": "Ana Silva", "kind": "jira_issue",
 "status": "pending", "created_at": "2026-11-16T09:00:00Z", "expires_at": null,
 "restricted": false, "revision": 2, "persona": "chief", "persona_offered": true,
 "title": "PAY-212: TCB grace period",
 "edited": false, "args_hash": "3f9a…(64 hex)", "my_recommendation": MyRecommendation | null}

// ReviewDraft = ReviewSummary plus, when restricted is false:
{"body": "markdown with [[s1]] citations", "target": Target, "sources": [Source]}

// A restricted ReviewDraft: exactly these fields
{"id": "apr_…", "owner": "ana", "owner_name": "Ana Silva", "kind": "slack_reply",
 "status": "pending", "created_at": "…", "expires_at": "…", "restricted": true, "revision": 1,
 "persona": null, "persona_offered": true}

// MyRecommendation: the reviewer's own live one on this draft
{"verdict": "approve", "note": "Looks right to me." | null, "note_removed": false,
 "revision": 2, "created_at": "2026-11-16T10:00:00Z", "stale": false}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | string | The draft's id (`apr_…`), as under **Drafts**. |
| `owner` | string | The draft owner's handle. |
| `owner_name` | string | Their display name, or their handle when they have none. **Untrusted text**: render as plain text. |
| `kind` | string | `confluence_page`, `jira_issue`, `slack_reply` or `slack_message`. Other writes (such as a bet) are not listed. |
| `status` | string | `pending` (a listed draft always is). |
| `created_at`, `expires_at` | string, string or null | RFC 3339. `expires_at` is set for Slack drafts. |
| `restricted` | bool | Metadata only: see above. |
| `revision` | integer or null | The version of the draft's text: 1 as the bot drafted it, up by one each time its owner saves different text (an edit, an accepted suggestion, an edit on the Slack card). `null` from an older tool server. |
| `persona`, `persona_offered` | string or null, bool | Which bot drafted it, as under **Drafts** and **Bots** ("Attribution"): show "Drafted by Chief". Metadata, so also on a restricted draft. |
| `title` | string or null | As under **Drafts**; `null` when restricted. |
| `body`, `target`, `sources` | | As under **Drafts** (`Draft.body`, `Draft.target`, `[Source]`, citations resolved the same way). `ReviewDraft` only; absent when restricted. A Slack target names a public channel only: for a private channel or a direct message the reviewer gets the label without a name ("A Slack thread", "Direct message"), which the owner sees in full. |
| `edited` | bool | Its owner changed the bot's text. Absent when restricted. |
| `args_hash` | string | 64 lowercase hex: the SHA-256 of the draft's arguments as read. Send it back with `revision`. Absent when restricted. |
| `my_recommendation` | object or null | The reviewer's own live recommendation; `null` for none. Never another reviewer's. |
| `pms` (inbox only) | [string] | The handles whose drafts the reviewer reviews now (assigned and active). |

**Recommending.** `POST /api/reviews/{id}/recommendation` with the `revision` and `args_hash`
the reviewer read (from `ReviewDraft`):

```json
{"verdict": "approve", "note": "Looks right to me.", "revision": 2, "args_hash": "3f9a…"}
```

- `verdict` is `approve` ("recommends approving") or `changes` ("suggests changes"). Anything
  else is `400 bad_request`.
- `note` is optional, **plain text**, at most 1,000 characters (`422 invalid` past it). The
  server removes control characters (a tab becomes a space, line breaks are kept) and trims it;
  an empty note is no note. It is never a link, Markdown or HTML anywhere: render it as text.
- `revision` (an integer ≥ 1) and `args_hash` (64 hex characters) are required; any other field
  is `400 bad_request`.
- **Bound to the version read.** If the owner saved different text since, or the draft holds
  other arguments, nothing is recorded: `409 draft_changed`. Read the draft again, show what
  changed, and let the reviewer recommend on the new version.
- **One live recommendation per reviewer per draft**: a new one replaces the reviewer's earlier
  one (and its note). The answer is the new `MyRecommendation` and the draft as now read.
- Refused on a restricted draft (`403 restricted_draft`), on their own (`403 own_draft`), once
  decided (`409 not_pending`) or expired (`409 expired`), and while the tool server runs
  without signed user assertions (`503 assertion_required`, development stacks only).
- A write: it needs the session's CSRF token and the app's own `Origin` (**Sign-in and
  safety**). If the answer is lost, `502 unknown_outcome`: read the draft again to see whether
  `my_recommendation` is the new one.

**Stale advice.** `MyRecommendation.stale` (and `Recommendation.stale` on the owner's draft) is
`true` when the draft changed since the recommendation was given: its revision moved on, or
its arguments are others. Show it as given "on an earlier version" (the owner's draft: "Ana
recommended approving an earlier version", the note greyed). The reviewer can recommend again
on the new version.

**What the note and names are.** A reviewer's note reaches the reviewer and the draft's owner
in this app only: never a Slack message, the console, the audit log or any log. It is removed
when its Slack draft's text is (24 hours after the draft was made or decided): `note: null`,
`note_removed: true`. `owner_name` and `reviewer_name` are untrusted display names. A test
feeds both a `<!channel>` and an `<img onerror>` string: React text nodes only.

**Errors** (each changed nothing, and the tool server recorded it in the audit log with its
code when it reached it): `400 bad_request`, `401 signed_out`, `403 bad_origin` / `bad_csrf`
(a recommendation), `403 forbidden` (not an active reviewer), `403 own_draft`,
`403 restricted_draft`, `404 not_found`, `409 not_pending`, `409 expired`,
`409 draft_changed`, `422 invalid`, `502 upstream_unavailable`, `503 assertion_required`,
`503 unavailable`, `503 users_unavailable`; and `502 unknown_outcome` (a recommendation whose
answer was lost: it may have been recorded). See **Error codes**.

**What gets recorded** (the tool server's audit log, `client` `web`, `task_id` `reviews`, the
reviewer as `actor`, the draft as `target`, its args hash; never its text or the note):
`approvals.review_inbox` for each listing, `approvals.review_read` for each read,
`approvals.recommend` for each recommendation with the detail `verdict approve revision 2`,
and each of them `denied` with the refusal's code. The owner's later decision of the draft is
recorded with `after a recommendation to approve, on this version` (or `to make changes`, or
`on an earlier version`), one part per kind of advice, after any detail of its own (`edited
before approval; …`, `reason: tone; …`). Like every audit event they are deleted after 90 days.

## Admin: policy

The Policy Admin for the NVIDIA OpenShell sandbox every bot runs in (`docs/policy-admin.md`
says what it does and how it's set up). **Read-and-propose:** nothing here writes a file. A
change is opened as a GitHub pull request **by the tool server, with the admin's own linked
GitHub account** (so the admin is its author and GitHub refuses their own approval); another
person reviews and merges it in GitHub. `pmos-web` holds no GitHub token.

| Route | `admin` | `auditor` | anyone else |
| --- | --- | --- | --- |
| `GET /api/admin/policy` | yes | yes | `403 forbidden` |
| `POST /api/admin/policy/validate` | yes | `403 forbidden` | `403 forbidden` |
| `GET /api/admin/policy/proposals` | yes | yes | `403 forbidden` |
| `POST /api/admin/policy/proposals` | yes | `403 forbidden` | `403 forbidden` |

Reading needs `view-policies`; validating and proposing need `edit-policies` (a validation's
diffs show the current text of the files it changes). Every `403 forbidden` on these routes is
recorded in the audit log. Signed out is `401 signed_out`; every `POST` needs the CSRF token
(`403 bad_csrf`) and JSON (`415 bad_content_type`), as everywhere. The role check comes before
anything else, so an auditor proposing gets `403`, never `501`.

**Changed on 2026-11-17** (security gate findings A1 to A4, A6), for the front end:

- `POST /api/admin/policy/validate` and `POST /api/admin/content/validate` are **admins only**
  now: an auditor gets `403 forbidden`. Hide the validate controls for viewers without
  `edit-policies` / `edit-content` in `GET /api/me`'s `permissions`.
- `GET /api/admin/policy` and `GET /api/admin/content` have a new field `protection`
  (below); warn when it isn't `verified` or `required_reviews` is `0`.
- `proposals_configured` now means "the tool server has proposals on" (it was "pmos-web has a
  GitHub token").
- The proposal routes' `201` has a new field `author` (the admin's GitHub login). New error
  codes: `409 github_not_linked`, `422 policy_mismatch`, `502 unknown_outcome`, and
  `502 upstream_unavailable` when the tool server can't be reached; `501 not_configured` and
  `429 rate_limited` now come from the tool server (same shapes).
- The proposals lists have new fields `github` (`"linked"` or `"not_linked"`) and `message`;
  without a linked account they answer `200` with an empty list, not an error. `author` is
  now the admin's own login.
- `GET /api/admin/content/file` and the content validate and proposal routes can answer
  `502 audit_unavailable` for an admin's read of a PM's personal master file or restricted
  context: such a read is recorded before the text is served.
- In `GET /api/admin/content` (and `generate_errors` in `GET /api/admin/policy`), an
  auditor sees findings on files they can't open with the message replaced by "A finding on a
  file your role can't open." (`code`, `path` and `line` stay).
- Removed server settings: `PMOS_WEB_GITHUB_*` and `PMOS_WEB_PROPOSALS_PER_HOUR`
  (`docs/policy-admin.md`, **Configuration**).

**What the server shows is its deployed revision.** The spec, the files and every validation
are computed from the content tree `pmos-web` is running with (`PMOS_CONTENT_ROOT`) and the
policy files beside it (`PMOS_WEB_POLICIES_ROOT`). That can be behind the repository's default
branch. A proposal is made on the default branch's head and is refused `409 stale_base` when a
file it writes differs there from the server's copy; `revision.commit` says which commit the
server has, when it can tell.

**What a policy is made from.** Policies are generated, never hand-written: `pmos policy
generate` derives each bot's policy from three inputs in the content tree, and those inputs are
the `spec` the admin edits:

- `bots.<bot>`: `role`, `scope` (`team` or `personal`) and `owner` (a handle), from
  `content/bots/<bot>/BOT.md`. A personal bot also reads its owner's
  `master/people/<owner>/AGENTS.md`, when there is one.
- `roles.<role>.skills`: from `content/roles/<role>/ROLE.md`. Each skill is uploaded read-only.
- `skills.<skill>.tools`: registry names (`docs.search`) from `content/skills/<skill>/SKILL.md`
  `metadata.tools`. They become the only tools `tools/call` may name (as MCP names,
  `docs_search`).

Everything else is `fixed`: the three destinations (tool server, gateway, runtime) and their
ports, REST routes, MCP methods and revisions, the worker binary, the system paths, the
workspace, the non-root identity and Landlock. They change only with code
(`crates/policy/src/lib.rs`), never here. New bots, roles and skills are content proposals
(**Admin: content and harness**), which regenerate the policies the same way.

### `GET /api/admin/policy`

```json
{"revision": {"commit": "5bb6de4c…", "source": "git"},
 "spec": {
   "bots": {"docs": {"role": "docs-helper", "scope": "team", "owner": "TODO"}},
   "roles": {"docs-helper": {"skills": ["docs"]},
             "pm-assistant": {"skills": ["ask", "meeting-notes", "…"]}},
   "skills": {"docs": {"tools": ["docs.search", "docs.read"]}, "…": {"tools": []}}},
 "spec_sources": {"bots": "content/bots/<bot>/BOT.md", "roles": "content/roles/<role>/ROLE.md",
                  "skills": "content/skills/<skill>/SKILL.md"},
 "fixed": {
   "services": [
     {"service": "toolserver", "rule": "pmos_toolserver", "host": "toolserver.pmos.internal",
      "port": 8710, "protocol": "mcp", "routes": null, "credential_profile": null},
     {"service": "gateway", "rule": "pmos_gateway", "host": "gateway.pmos.internal",
      "port": 8740, "protocol": "rest", "routes": ["POST /v1/chat/completions"],
      "credential_profile": "pmos-gateway"},
     {"service": "runtime", "rule": "pmos_runtime", "host": "bots.pmos.internal", "port": 8750,
      "protocol": "rest", "routes": ["POST /v1/worker/claim", "POST /v1/worker/tasks/*/complete",
      "POST /v1/worker/tasks/*/events", "POST /v1/worker/tasks/*/fail",
      "POST /v1/worker/tasks/*/heartbeat", "POST /v1/worker/tasks/*/trace"],
      "credential_profile": "pmos-worker"}],
   "mcp_path": "/mcp",
   "mcp_methods": ["initialize", "notifications/initialized", "tools/list", "tools/call"],
   "mcp_versions": ["2025-03-26", "2025-06-18", "2025-11-25"],
   "worker_binary": "/usr/local/bin/node", "workspace": "/sandbox/workspace",
   "content_mount": "/sandbox", "shared_content": ["templates", "reviewers", "playbooks"],
   "system_read_only": ["/app", "/dev/urandom", "/etc", "/lib", "/proc", "/usr", "/var/log"],
   "system_read_write": ["/dev/null", "/tmp"], "run_as": "sandbox",
   "landlock": "hard_requirement", "source": "crates/policy/src/lib.rs"},
 "bots": [
   {"bot": "docs", "role": "docs-helper", "scope": "team", "sandbox": "pmos-docs",
    "skills": ["docs"], "tools": ["docs_read", "docs_search"], "master": ["master/AGENTS.md"],
    "policy_file": "policies/bots/docs.yaml",
    "launch_spec_file": "policies/bots/docs.sandbox.yaml",
    "allowances_from": "checked_in",
    "allowances": {
      "network": [
        {"rule": "pmos_gateway", "service": "gateway", "host": "gateway.pmos.internal",
         "port": 8740, "ports": [], "allowed_ips": [], "protocol": "rest",
         "enforcement": "enforce", "tls": null, "access": null,
         "binaries": ["/usr/local/bin/node"], "credential": "pmos-docs-gateway",
         "routes": ["POST /v1/chat/completions"], "mcp_methods": [], "mcp_tools": [],
         "mcp_versions": []},
        {"rule": "pmos_toolserver", "service": "toolserver", "host": "toolserver.pmos.internal",
         "port": 8710, "ports": [], "allowed_ips": [], "protocol": "mcp",
         "enforcement": "enforce", "tls": null, "access": null,
         "binaries": ["/usr/local/bin/node"], "credential": null, "routes": [],
         "mcp_methods": ["initialize", "notifications/initialized", "tools/list", "tools/call"],
         "mcp_tools": ["docs_read", "docs_search"],
         "mcp_versions": ["2025-03-26", "2025-06-18", "2025-11-25"]}],
      "network_middlewares": [],
      "filesystem": {"include_workdir": false,
        "read_only": ["/app", "/dev/urandom", "/etc", "/lib", "/proc", "/sandbox/master/AGENTS.md",
                      "/sandbox/playbooks", "/sandbox/reviewers", "/sandbox/skills/docs",
                      "/sandbox/templates", "/sandbox/tools.yaml", "/usr", "/var/log"],
        "read_write": ["/dev/null", "/sandbox/workspace", "/tmp"]},
      "landlock": "hard_requirement",
      "process": {"run_as_user": "sandbox", "run_as_group": "sandbox"},
      "providers": ["pmos-docs-gateway", "pmos-docs-worker"],
      "uploads": ["master/AGENTS.md -> /sandbox/master/AGENTS.md",
                  "skills/docs -> /sandbox/skills/docs", "tools.yaml -> /sandbox/tools.yaml"],
      "summary": [
        "May connect to the model gateway at gateway.pmos.internal:8740 (rest, enforce) from /usr/local/bin/node: POST /v1/chat/completions.",
        "Uses the credential pmos-docs-gateway on gateway.pmos.internal:8740.",
        "May call these tools: docs_read, docs_search.",
        "May read this content: /sandbox/master/AGENTS.md, /sandbox/skills/docs, /sandbox/tools.yaml.",
        "May write only: /dev/null, /sandbox/workspace, /tmp.",
        "Runs as sandbox:sandbox; Landlock hard_requirement."]}}],
 "files": [
   {"path": "policies/bots/docs.sandbox.yaml", "state": "same",
    "generated": "# Generated by `pmos policy generate` …", "checked_in": "# Generated by …"},
   {"path": "policies/bots/docs.yaml", "state": "same", "generated": "…", "checked_in": "…"},
   {"path": "policies/providers/pmos-gateway.yaml", "state": "same", "generated": "…",
    "checked_in": "…"},
   {"path": "policies/providers/pmos-worker.yaml", "state": "same", "generated": "…",
    "checked_in": "…"}],
 "drift": {"checked_in_available": true, "in_sync": true, "violations": []},
 "generate_errors": [],
 "proposals_configured": true,
 "protection": {"branch": "main", "required_reviews": 1, "verified": true, "github": "linked"}}
```

(Shortened where `…` appears; the real answer lists every item, every runtime route and every
summary line.)

| Field | Type | Meaning |
| --- | --- | --- |
| `revision.commit` | string or `null` | The commit the server's content is from: `PMOS_WEB_CONTENT_REVISION`, else read from the git files beside the content root. `null` when neither says. |
| `revision.source` | `"config"`, `"git"` or `null` | Where `commit` came from. |
| `spec` | object | The policy inputs, as above: every bot, role and skill the content defines (a file that doesn't lint is left out; see `generate_errors`). `owner` is a handle without `@`, `TODO` while unassigned. Lists are in file order. |
| `spec_sources` | object | Where each part of the spec lives in the repository. |
| `fixed` | object | What no spec can change (above). `routes` is `null` for the MCP endpoint; `credential_profile` is `null` for the tool server, which binds no provider (its session token is per task). |
| `bots[]` | array | One per bot the content generates a policy for, by name. Empty when `generate_errors` isn't. |
| `bots[].tools` | string[] | MCP tool names `tools/call` may name, sorted. |
| `bots[].master` | string[] | Master context files the bot reads, content-relative. |
| `bots[].allowances` | object or `null` | What the policy file allows, read field by field from it (below). `null` when the file is missing or doesn't parse. |
| `bots[].allowances_from` | `"checked_in"` or `"generated"` | `checked_in`: the file in the policies root (what OpenShell is given). `generated`: the server has no policies root, so this is what the content generates. |
| `files[]` | array | Every generated file, and every checked-in `.yaml` in `bots/` and `providers/`, by path. |
| `files[].state` | string | `same`, `differs` (drift: regenerate, or a hand edit), `missing` (generated, not checked in), `orphan` (checked in, not generated any more; a hand-written file there is also `orphan`) or `unknown` (no policies root). |
| `files[].generated`, `files[].checked_in` | string or `null` | The file's text: what generation writes, and what is checked in. `null` where there is none. |
| `drift.checked_in_available` | boolean | Whether the server has a policies root to compare with. |
| `drift.in_sync` | boolean or `null` | Every file `same` and no rule broken. `null` without a policies root. |
| `drift.violations[]` | `Issue[]` | Every PM OS rule a checked-in file breaks (`pmos policy check`'s rules: `net-egress`, `net-tools`, `fs-writable`, `cred-binding`, `proc-identity`, …), at its path. |
| `generate_errors[]` | `Issue[]` | Content lint errors that stop generation (then `bots` is empty). For an auditor, a finding on a PM's personal master file or restricted context has the message "A finding on a file your role can't open." |
| `proposals_configured` | boolean | Whether the tool server has proposals on (the GitHub App and the content repository). When `false`, hide the propose button: proposals answer `501`. `false` also when the tool server didn't answer. |
| `protection` | object | Whether the default branch makes GitHub require someone else's review, as the tool server could read it with **this viewer's** GitHub account. Best effort: never an error, never blocks a proposal. |
| `protection.branch` | string or `null` | The branch proposals target (`null` when proposals are off). |
| `protection.required_reviews` | integer or `null` | Approving reviews the branch requires; `0`: none (warn: the author could merge without anyone's review). `null`: unknown. |
| `protection.verified` | boolean | **Changed (review of `b469d5b`, UM4).** `true` only when GitHub's answer showed that the branch requires a review **and** that admins aren't exempt **and** that nobody may bypass it (`admins_enforced: true`, `bypass_allowed: false`), or that no review is required at all (`required_reviews: 0`, which the screen warns about). Anything else is `false`: warn that every change can't be confirmed to need someone else's review (the existing `unverified` sentence, reworded to say admins may be exempt, someone may be allowed to bypass, or the account can't read it). |
| `protection.admins_enforced` | boolean or `null` | **New.** Whether the rule binds admins too (classic `enforce_admins`, or a ruleset with no bypass actor). `null`: GitHub's answer didn't say (never treated as `true`). |
| `protection.bypass_allowed` | boolean or `null` | **New.** Whether anyone may merge without the review (classic `bypass_pull_request_allowances`, or a ruleset's `bypass_actors`). `null`: GitHub's answer didn't say (a ruleset's bypass list is shown only to someone who may edit it). |
| `protection.github` | `"linked"`, `"not_linked"`, `"unavailable"` or `null` | Whether this viewer has linked GitHub (`null` when proposals are off or the tool server didn't answer). |

**Allowances** (one object per bot):

| Field | Type | Meaning |
| --- | --- | --- |
| `network[]` | array | One per endpoint, rule by rule. Anything not listed is denied. |
| `network[].rule` | string | The `network_policies` key, as OpenShell logs it. |
| `network[].service` | `"toolserver"`, `"gateway"`, `"runtime"` or `null` | Which PM OS service `host:port` is. **`null` is a destination PM OS doesn't know**: show it as a red flag (a hand edit). |
| `network[].host`, `network[].port` | string or `null`, integer or `null` | The destination. |
| `network[].ports`, `network[].allowed_ips` | arrays | Extra ports and IP ranges. Always empty in a generated policy; anything here is a hand edit. |
| `network[].protocol` | string or `null` | `rest` or `mcp` when inspected; `null` is not inspected. |
| `network[].enforcement` | string or `null` | `enforce`; anything else (`audit`, `null`) only logs. |
| `network[].tls`, `network[].access` | string or `null` | Always `null` in a generated policy (`skip` would relay TLS uninspected; an access preset is broad). |
| `network[].binaries` | string[] | The executables that may open these connections. |
| `network[].credential` | string or `null` | The provider instance bound to this endpoint. |
| `network[].routes` | string[] | REST: `METHOD path` allowed (in OpenShell's globs, `*` is one path segment). |
| `network[].mcp_methods`, `network[].mcp_tools`, `network[].mcp_versions` | string[] | MCP: the methods, the tools `tools/call` may name, and the protocol revisions. |
| `network_middlewares` | string[] | Always empty in a generated policy. |
| `filesystem.read_only`, `filesystem.read_write` | string[] | Paths inside the sandbox. Content is under `/sandbox/`. |
| `filesystem.include_workdir` | boolean | `true` would make the working directory writable too. |
| `landlock` | string or `null` | `hard_requirement`: the sandbox doesn't start without its filesystem rules. |
| `process.run_as_user`, `process.run_as_group` | string or `null` | The workload identity. |
| `providers` | string[] or `null` | The launch spec's `--provider` attachments; `null` without a launch spec. |
| `uploads` | string[] or `null` | The launch spec's read-only uploads, `from -> to` (`from` content-relative). |
| `summary` | string[] | The same, in sentences. |

**`Issue`** (here and in every validation below):

```json
{"severity": "error", "code": "role-skill-unknown", "path": "content/roles/docs-helper/ROLE.md",
 "line": null, "message": "skill `no-such-skill` is not in skills/"}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `severity` | `"error"` or `"warning"` | An error stops a proposal; a warning doesn't. |
| `code` | string | Stable: a content lint code (`docs/contract.md`; such as `skill-parse`, `role-skill-unknown`, `owner-unassigned`), a policy rule (`net-egress`, …), or a spec code (below). |
| `path` | string | A repository path (`content/…`, `policies/…`), or for a spec error a spec path (`spec.bots.docs.scope`, `spec.skills.docs.tools[2]`). |
| `line` | integer or `null` | 1-based line in the file, when the parser gave a position (front matter that doesn't parse). Usually `null`: most rules are about the whole file. |
| `message` | string | One sentence for the admin. |

### `POST /api/admin/policy/validate`

Runs the change on a scratch copy of the deployed content: applies the spec to the bot, role
and skill files, runs the content lint, regenerates every policy, checks each against the PM
OS rules, and compares with what's checked in. Nothing is saved.

Request: `{"spec": SpecPatch}`. A `SpecPatch` is any part of `spec`: leave out what doesn't
change. A field equal to the current value changes nothing.

```json
{"spec": {"skills": {"docs": {"tools": ["docs.search", "docs.read", "jira.search"]}},
          "bots": {"docs": {"owner": "ana"}},
          "roles": {"docs-helper": {"skills": ["docs"]}}}}
```

Unknown fields anywhere (`{"bots": {"docs": {"hosts": []}}}`) are `400 bad_request`: only
`role`, `scope`, `owner`, `skills` and `tools` exist.

Response, always `200` once the body parses (check `valid`):

```json
{"valid": true,
 "errors": [],
 "warnings": [
   {"severity": "warning", "code": "bot-reads-empty", "path": "content/bots/docs/BOT.md",
    "line": null, "message": "this team bot can read Jira, Confluence or Slack but lists nothing under `reads`, so it will read nothing there"}],
 "content_changes": [
   {"path": "content/skills/docs/SKILL.md", "change": "modified",
    "diff": "--- a/content/skills/docs/SKILL.md\n+++ b/content/skills/docs/SKILL.md\n@@ -6,7 +6,7 @@\n   version: \"0.1.0\"\n   model: \"extraction\"\n   data-class: \"public\"\n-  tools: \"docs.search docs.read\"\n+  tools: \"docs.search docs.read jira.search\"\n   surfaces: \"slack cli\"\n   ships-in: \"phase-0\"\n   track: \"Everyday\"\n"}],
 "policy_changes": [
   {"path": "policies/bots/docs.yaml", "change": "modified",
    "diff": "--- a/policies/bots/docs.yaml\n+++ b/policies/bots/docs.yaml\n@@ -100,6 +100,7 @@\n                 any:\n                   - docs_read\n                   - docs_search\n+                  - jira_search\n     name: pmos_toolserver\n process:\n   run_as_group: sandbox\n"}],
 "allowance_changes": [
   {"bot": "docs", "kind": "network.mcp_tool", "item": "jira_search", "before": null,
    "after": "jira_search", "direction": "wider", "widening": true,
    "description": "docs may now call the tool jira_search"}],
 "widening": true,
 "summary": {"wider": 1, "narrower": 0, "changed": 0}}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `valid` | boolean | No errors: it can be proposed. |
| `errors`, `warnings` | `Issue[]` | Every error in the changed tree, and the warnings that are new or on a changed file (the content's existing warnings elsewhere, such as unassigned owners, are left out). |
| `content_changes[]` | `FileDiff[]` | The content files the spec changes. |
| `policy_changes[]` | `FileDiff[]` | The generated files that change, compared with the checked-in ones (or, without a policies root, with what the deployed content generates). Empty when the changed tree doesn't lint. |
| `allowance_changes[]` | array | Each difference in what a bot may do (below). Empty: no sandbox changes. |
| `widening` | boolean | Any allowance change is `widening`. Show it loudly. |
| `summary` | object | Counts of `allowance_changes` by `direction`. |

`FileDiff`: `{path, change: "added" | "modified" | "deleted", diff}`, where `diff` is a unified
diff with three lines of context (`--- a/<path>`, `+++ b/<path>`, `/dev/null` for an added or
deleted side). Show it monospaced.

**Allowance changes:**

| Field | Type | Meaning |
| --- | --- | --- |
| `bot` | string | The bot (empty for `provider_profile`). |
| `kind` | string | `network.destination` (`host:port`, and extra ports or IP ranges), `network.binary`, `network.route`, `network.mcp_method`, `network.mcp_tool`, `network.mcp_version`, `network.middleware`, `network.protocol`, `network.enforcement`, `network.tls`, `network.access`, `network.credential`, `filesystem.read_only`, `filesystem.read_write`, `filesystem.include_workdir`, `landlock`, `process`, `credential.provider`, `upload`, or `provider_profile` (a shared provider profile file changes). |
| `item` | string | What it is about: a destination, a route, a tool, a path. Empty for bot-wide settings. |
| `before`, `after` | string or `null` | The old and new value (`null` on the side where it doesn't exist). |
| `direction` | `"wider"`, `"narrower"` or `"changed"` | Something added, something removed, or a setting changed. A new bot is all `wider`; a removed bot all `narrower`. |
| `widening` | boolean | `true` for `wider` and `changed`: **every widening is marked**, and a changed setting is treated as one for review. |
| `description` | string | One sentence (`docs may now call the tool jira_search`). |

**Spec error codes** (`path` is the spec path): `spec-unknown-bot`, `spec-unknown-role`,
`spec-unknown-skill` (it isn't in the content: add it as a content proposal), `spec-bad-name`
(a role, skill or tool name that isn't valid), `spec-bad-value` (a `scope` other than `team` or
`personal`, an `owner` that isn't a handle), `spec-duplicate`, `spec-too-long` (over 100 in a
list), `spec-file-missing`, `spec-unsupported-format` (the file's front matter has a layout the
editor can't change line by line, such as a folded multi-line value: change it as a content
proposal). With a spec error, the other arrays are empty.

### `POST /api/admin/policy/proposals`

Admins only. Request: `{"spec": SpecPatch, "title": "…", "reason": "…"}`.

- `title`: one line; control characters are dropped and whitespace collapsed; at most 120
  characters kept. Required. The pull request's title is `PM OS policy: <title>`.
- `reason`: free text for the reviewers; control characters other than newlines and tabs
  dropped; at most 4,000 characters kept. Required. It goes into the pull request body in a
  fenced block, so it can't format the page or mention anyone.

It validates again and sends the change to the tool server, which checks it again and opens the
pull request with the admin's own GitHub account (`docs/policy-admin.md`, How a proposal is
opened). `201`:

```json
{"number": 42, "url": "https://github.com/acme/pmos/pull/42", "author": "adam-gh",
 "branch": "pmos/proposal/policy/adam-20261117-101509-d2aabc50",
 "base": {"branch": "main", "commit": "1111111111111111111111111111111111111111"},
 "files": ["content/skills/docs/SKILL.md", "policies/bots/docs.yaml"],
 "widening": true,
 "allowance_changes": [{"bot": "docs", "kind": "network.mcp_tool", "item": "jira_search",
   "before": null, "after": "jira_search", "direction": "wider", "widening": true,
   "description": "docs may now call the tool jira_search"}],
 "stale_check": "passed",
 "labels_added": true}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `number`, `url` | integer, string | The pull request. Link to it: review and merge happen there. |
| `author` | string or `null` | The GitHub login that opened it: the admin's own. Someone else must approve it. |
| `branch` | string | Made by the server: `pmos/proposal/<kind>/<handle>-<yyyymmdd-hhmmss>-<random>`. |
| `base.branch`, `base.commit` | strings | What it was made on. |
| `files` | string[] | Every file the commit writes: the content files, then the regenerated policy files. |
| `widening`, `allowance_changes` | | As in validate. |
| `stale_check` | `"passed"` or `"skipped"` | `skipped` when GitHub cut the base's file list short (a very large repository): the base wasn't compared. |
| `labels_added` | boolean | `false` if GitHub refused the labels. The pull request is open regardless, and still listed below (by its branch). |

Errors, besides those above:

| Status and code | When |
| --- | --- |
| `400 bad_request` | The body doesn't parse, or the title or reason is empty after cleaning. |
| `400 no_change` | The spec changes nothing. |
| `422 invalid` | Validation found errors (here, or in the tool server's own re-check). The body also has `errors` and `warnings` (`Issue[]`). Nothing was written; the refusal is recorded. |
| `409 github_not_linked` | The admin hasn't linked GitHub in PM OS. `error` says how (in Slack: the PM OS app's Home tab, **Accounts**). Show it as a call to action, not a failure. Nothing was written. |
| `409 stale_base` | A file the pull request would write differs on the base branch (or in the tool server's copy) from the copy this change was made from. The body also has `paths` (string[]) and, when it was the base branch, `base` (`{branch, commit}`). Nothing was written; the refusal is recorded. Wait for the deployment to catch up, or make the change in GitHub. |
| `422 policy_mismatch` | The tool server regenerates different policy files from the change (`paths`): the web app and the tool server run different content or code. Nothing was written. |
| `429 rate_limited` | Over this admin's proposals per hour (the tool server's `PMOS_PROPOSALS_PER_HOUR`, default 10). `Retry-After` gives the seconds. |
| `501 not_configured` | Proposals are off at the tool server. |
| `502 audit_unavailable` | The tool server couldn't record the proposal, so nothing was written to GitHub. |
| `502 github_refused` | GitHub refused the admin's account for the repository (it needs write access: contents and pull requests). |
| `502 github_unavailable` | GitHub didn't answer, couldn't find the repository or base branch, or refused the request. |
| `502 unknown_outcome` | Sent, but the answer was lost: reload the proposals list. |
| `502 upstream_unavailable` | The tool server couldn't be reached or answered something else. Nothing was written if it wasn't reached. |
| `503 github_rate_limited` | GitHub's own rate limit. |

Every error's `error` is `pmos-web`'s own sentence, never the tool server's or GitHub's.

### `GET /api/admin/policy/proposals`

Open pull requests this feature made for policy: a branch under `pmos/proposal/policy/` or the
label `pmos-proposal:policy`. Read from GitHub by the tool server with **the viewer's own linked
GitHub account** on each request (the newest 300 open pull requests). `501 not_configured` when
proposals are off. A viewer who hasn't linked GitHub gets `200` with an empty list,
`github: "not_linked"` and a `message` to show as the empty state (not an error page).

```json
{"repo": "acme/pmos",
 "github": "linked",
 "message": null,
 "proposals": [
   {"number": 42, "url": "https://github.com/acme/pmos/pull/42",
    "title": "PM OS policy: Let the Docs bot search Jira",
    "branch": "pmos/proposal/policy/adam-20261117-101509-d2aabc50", "base": "main",
    "status": "open", "proposed_by": "adam",
    "base_commit": "1111111111111111111111111111111111111111", "author": "adam-gh",
    "labels": ["pmos-proposal", "pmos-proposal:policy"],
    "created_at": "2026-11-17T10:00:00Z", "updated_at": "2026-11-17T10:00:00Z"}]}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `github` | `"linked"` or `"not_linked"` | Whether the viewer's GitHub account was used. `not_linked`: `proposals` is empty and `message` says how to link. |
| `message` | string or `null` | The empty state's sentence when `not_linked`. |
| `status` | `"open"` or `"draft"` | Only open pull requests are listed; merged and closed ones are in GitHub. Review state and checks are not read: link to the pull request. |
| `proposed_by` | string or `null` | The PM OS handle, read from the marker on the pull request body's first line; `null` if the body was edited. It is what the body says, not a verified fact: the audit log has the verified record. |
| `base_commit` | string or `null` | The base it was made on, from the same marker. |
| `author` | string or `null` | The GitHub login that opened it: the proposing admin's own account (pull requests opened before 2026-11-17 show the old shared token's identity). |
| `created_at`, `updated_at` | string or `null` | As GitHub gives them. |

## Admin: content and harness

The harness content (roles, bots, skills, playbooks, the context library, templates, reviewers,
master context) and what the running harness uses (`docs/content-admin.md`). The same
read-and-propose rules and deployed-revision caveat as **Admin: policy**.

| Route | `admin` | `auditor` | anyone else |
| --- | --- | --- | --- |
| `GET /api/admin/content` | yes | yes | `403 forbidden` |
| `GET /api/admin/content/file?path=` | yes (a PM's own master file or `restricted` context is recorded first) | yes, but not a PM's own master file or `restricted` context (`403 forbidden`) | `403 forbidden` |
| `POST /api/admin/content/validate` | yes | `403 forbidden` | `403 forbidden` |
| `GET /api/admin/content/proposals` | yes | yes | `403 forbidden` |
| `POST /api/admin/content/proposals` | yes | `403 forbidden` | `403 forbidden` |
| `GET /api/admin/harness` | yes | yes | `403 forbidden` |

Reading needs `view-content`; validating and proposing need `edit-content`. Every
`403 forbidden` is recorded in the audit log. **Sensitive files:** every path named in a file
read, a validation or a proposal goes through one read rule: a PM's own master file
(`master/people/<handle>/AGENTS.md`) and context with `sensitivity: restricted` are for admins
only, and an admin's read of one is recorded in the audit log **before** its text is served;
if it can't be recorded, the answer is `502 audit_unavailable` and nothing is shown.

**Paths** are repository paths inside the content tree, such as `content/skills/docs/SKILL.md`.
A path is relative, `/`-separated, made of letters, digits, `.`, `_` and `-`, with no empty,
`.`, `..` or hidden (`.`-prefixed) component, at most 200 bytes and 8 deep. Anything else is
`400 bad_path`; anything outside `content/` (`policies/`, `deploy/`, `.github/`, code) is
`400 path_not_allowed`. Symbolic links are never followed (`400 bad_path`), and the tree never
lists one.

### `GET /api/admin/content`

```json
{"revision": {"commit": null, "source": null},
 "root": "content",
 "lint": {"errors": 0, "warnings": 93, "files_checked": 111,
          "checked_at": "2026-11-17T10:15:09.841Z", "other": []},
 "files": [
   {"path": "content/skills/docs/SKILL.md", "kind": "skill", "name": "docs", "title": null,
    "version": "0.1.0", "owner": "TODO", "sensitivity": null, "last_reviewed": null,
    "size": 1170, "editable": true, "readable": true,
    "lint": {"status": "warning", "errors": 0, "warnings": 1,
             "issues": [{"severity": "warning", "code": "owner-unassigned",
                         "path": "content/skills/docs/SKILL.md", "line": null,
                         "message": "owner is not assigned yet"}]}},
   {"path": "content/context/products/roadmap.md", "kind": "context", "name": null,
    "title": null, "version": null, "owner": null, "sensitivity": "restricted",
    "last_reviewed": null, "size": 112, "editable": true, "readable": false,
    "lint": {"status": "ok", "errors": 0, "warnings": 0, "issues": []}}],
 "roles": [
   {"name": "docs-helper", "version": "0.1.0", "owner": "TODO", "model": "default",
    "data_class": "public", "skills": ["docs"], "bots": ["docs"]}],
 "skills": [
   {"name": "docs", "version": "0.1.0", "owner": "TODO", "model": "extraction",
    "data_class": "public", "tools": ["docs.search", "docs.read"], "used_by": ["docs-helper"]}],
 "bots": [{"name": "docs", "role": "docs-helper", "scope": "team", "owner": "TODO"}],
 "proposals_configured": true,
 "protection": {"branch": "main", "required_reviews": 1, "verified": true, "github": "linked"}}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `revision` | object | As in **Admin: policy**. |
| `root` | string | The content tree's folder in the repository (`content`; empty if the content is the repository root). |
| `lint` | object | The content lint, run now on the deployed tree: counts, when, and `other` (`Issue[]` about a folder rather than a file, such as a skill folder with no `SKILL.md`). For a viewer without `edit-content` (an auditor), every `other` message is "A finding on a file your role can't open." (review of `b469d5b`, UM3: such a finding can name anything in the tree). |
| `files[]` | array | Every regular file in the tree, sorted by path (no symlinks, nothing hidden). |
| `files[].kind` | string | `role`, `bot`, `skill`, `skill_file` (another file in a skill's folder), `playbook`, `template`, `reviewer`, `context`, `readme`, `master` (`master/AGENTS.md`), `personal_master` (`master/people/<handle>/AGENTS.md`), `team`, `tools` or `other`. |
| `files[].name`, `title`, `version`, `owner`, `sensitivity`, `last_reviewed` | string or `null` | From the file's front matter, where it has them (a skill's `version` and `owner` come from `metadata`). `null` when absent or unparsable. **For a file this viewer may not open** (`readable: false`), `name`, `title`, `version`, `owner` and `last_reviewed` are always `null` (UM3); `sensitivity` stays, to say why. |
| `files[].size` | integer | Bytes. |
| `files[].editable` | boolean | Whether a proposal may change it: text (`.md`, `.yaml`, `.yml`, `.txt`) and not `team.yaml` (retired: people and roles are in the user database, and `pmos lint` refuses the file). |
| `files[].readable` | boolean | Whether this viewer may open it (`GET …/file`). An auditor sees personal master files and `restricted` context listed, not opened. The rule fails closed (UM3): `sensitivity` is read in any letter case (`Restricted` is restricted), and a Markdown file whose front matter doesn't parse, never closes, has a `sensitivity` that isn't text, or wasn't read (too large) is closed to an auditor too. |
| `files[].lint` | object | `status` (`ok`, `warning` or `error`), counts, and that file's `Issue[]`. When `readable` is `false`, each issue's `message` is "A finding on a file your role can't open." (a parser's message can quote the file). |
| `roles[]` | array | Each role: its skills, model role, data class, and the bots made from it. |
| `skills[]` | array | Each skill: its tools and `used_by` (the roles that list it). A skill that doesn't lint isn't here (its file shows the error). |
| `bots[]` | array | Each bot: role, scope, owner. |
| `proposals_configured`, `protection` | | As in **Admin: policy**. |

### `GET /api/admin/content/file?path=content/…`

```json
{"path": "content/skills/docs/SKILL.md", "kind": "skill", "size": 1170,
 "sha256": "7937723eb58e838aa8dbafc2e758c1163a2151b27d894414c04f47a34c45efa9",
 "text": "---\nname: docs\n…",
 "front_matter": {"name": "docs", "description": "…",
                  "metadata": {"owner": "TODO", "version": "0.1.0", "tools": "docs.search docs.read"}},
 "front_matter_error": null,
 "editable": true}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `sha256` | string | Of `text`, hex. Send it back as `base_sha256` with a change to this file. |
| `text` | string | The whole file, UTF-8. Files over 256 KiB, not UTF-8, or with NUL bytes are refused (`413 too_large`, `400 not_text`). |
| `front_matter` | object or `null` | A `.md` file's YAML front matter, parsed. `null` for other files, or when it's missing or doesn't parse. |
| `front_matter_error` | string or `null` | Why it's `null` for a `.md` file. |

Errors: `400 bad_path`, `400 path_not_allowed`, `400 not_a_file` (a folder), `404 not_found`,
`403 forbidden` (an auditor and a personal or restricted file), `502 audit_unavailable` (an
admin's read of a personal or restricted file couldn't be recorded, so it isn't shown).

### `POST /api/admin/content/validate`

Request: `{"changes": [Change]}`, 1 to 50 changes, each path once:

```json
{"changes": [
  {"path": "content/playbooks/bar.md", "text": "---\ntitle: The bar\n…", "base_sha256": "7937…"},
  {"path": "content/playbooks/new.md", "text": "---\ntitle: New\nowner: \"adam\"\nversion: \"0.1.0\"\n---\n\nBody\n"},
  {"path": "content/templates/old.md", "delete": true}]}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `path` | string | Required. An editable path (above). |
| `text` | string | The whole new file. At most 256 KiB, no NUL. Give either `text` or `delete: true`. |
| `delete` | boolean | Remove the file (it must exist). Folders left empty go too. |
| `base_sha256` | string, optional | The `sha256` from `GET …/file` when the admin opened it. If the server's copy has changed since, `409 stale_base`. Leave it out for a new file. |

A change identical to the deployed file is dropped. The response has the same shape as
`POST /api/admin/policy/validate` (`valid`, `errors`, `warnings`, `content_changes`,
`policy_changes`, `allowance_changes`, `widening`, `summary`): a content change that alters a
sandbox (a skill's tools, a role's skills, a bot's role, a new or removed bot) regenerates the
policies, and its widenings are marked the same way.

```json
{"valid": false,
 "errors": [
   {"severity": "error", "code": "role-skill-unknown", "path": "content/roles/docs-helper/ROLE.md",
    "line": null, "message": "skill `docs` is not in skills/"},
   {"severity": "error", "code": "skill-parse", "path": "content/skills/docs/SKILL.md", "line": 6,
    "message": "invalid front matter: did not find expected ',' or ']' at line 5 column 1, while parsing a flow sequence at line 4 column 10"}],
 "warnings": [],
 "content_changes": [{"path": "content/skills/docs/SKILL.md", "change": "modified", "diff": "…"}],
 "policy_changes": [], "allowance_changes": [], "widening": false,
 "summary": {"wider": 0, "narrower": 0, "changed": 0}}
```

Refusals, before anything is linted:

| Status and code | When |
| --- | --- |
| `400 bad_request` | The body doesn't parse (unknown fields included), a path is listed twice, or a change has both or neither of `text` and `delete`. |
| `400 bad_path` | A path that isn't plain (above), or one that goes through a symlink. |
| `400 path_not_allowed` | Outside the content tree, or `content/team.yaml` (retired; never written by a proposal). |
| `400 not_text` | Not `.md`, `.yaml`, `.yml` or `.txt`, or text with a NUL byte. |
| `400 not_found` | Deleting a file that doesn't exist. |
| `400 no_change` | No changes, or every change equals the deployed file. |
| `409 stale_base` | A `base_sha256` doesn't match the server's copy now (recorded). |
| `413 too_large` | A file over 256 KiB. |
| `413 too_many_files` | More than 50 changes. |
| `502 audit_unavailable` | A change names a PM's personal master file or restricted context, and the admin's read of it couldn't be recorded. |

The whole request may be up to 16 MiB.

### `POST /api/admin/content/proposals`

Admins only. Request: `{"changes": [Change], "title": "…", "reason": "…"}` (title and reason as
in **Admin: policy**). Validates again and sends the changed content files and any regenerated
policy files to the tool server, which checks them again and opens the pull request with the
admin's own GitHub account. The `201` answer and the errors are those of
`POST /api/admin/policy/proposals`, for kind `content`: the branch is
`pmos/proposal/content/…`, the labels `pmos-proposal` and `pmos-proposal:content`, the title
`PM OS content: <title>`. A `409 stale_base` comes from any of three checks: the admin's
`base_sha256` against this server's copy (message only), this server's copy against the tool
server's (`paths`), or against the base branch (`paths` and `base`).

### `GET /api/admin/content/proposals`

As `GET /api/admin/policy/proposals` (with `github` and `message`), for branches under
`pmos/proposal/content/` or the label `pmos-proposal:content`.

### `GET /api/admin/harness`

What the running harness uses, read-only.

```json
{"bots": [
   {"name": "docs", "role": "docs-helper", "scope": "team", "owner": "TODO",
    "model_role": "default", "gateway_alias": "pmos/default", "gateway_model": "qwen3.6-35b-a3b",
    "skills": ["docs"], "tools": ["docs.read", "docs.search"], "loaded_by_runtime": true}],
 "model_roles": [
   {"role": "default", "alias": "pmos/default", "model": "qwen3.6-35b-a3b", "upstream": "qwen",
    "workarounds": ["strip-think"]},
   {"role": "extraction", "alias": "pmos/extraction", "model": "nemotron-3-nano-30b-a3b",
    "upstream": "nemotron", "workarounds": []},
   {"role": "reasoning", "alias": "pmos/reasoning", "model": "qwen3.6-35b-a3b", "upstream": "qwen",
    "workarounds": ["strip-think"]}],
 "gateway_config": "ok",
 "runtime": {"reachable": true, "bots": ["docs"]},
 "content_revision": {"commit": null, "source": null}}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `bots[]` | array | Every bot in the deployed content. |
| `bots[].model_role` | string or `null` | Its role's model role (`default`, `extraction`, `reasoning`); `null` if its role doesn't lint. |
| `bots[].gateway_alias` | string or `null` | What the harness asks the gateway for (`pmos/<model_role>`). |
| `bots[].gateway_model` | string or `null` | The served model name that alias maps to in the gateway config; `null` without the config, or when it has no such role. |
| `bots[].tools` | string[] | Registry names the bot may call (its role's skills' tools), sorted. |
| `bots[].loaded_by_runtime` | boolean or `null` | Whether the bot runtime lists it (`GET /v1/bots`); `null` when the runtime didn't answer. `false` means the runtime is running other content. |
| `model_roles` | array or `null` | Each gateway model role: the alias, the served model name, the upstream's **name** (never its address or a key) and its workarounds. `null` when `gateway_config` isn't `ok`. |
| `gateway_config` | `"ok"`, `"not_configured"` or `"unreadable"` | Whether `PMOS_WEB_GATEWAY_CONFIG` is set and parses. This is the server's copy of the gateway config, not what the running gateway loaded. |
| `runtime.reachable` | boolean | Whether the bot runtime answered. |
| `runtime.bots` | string[] or `null` | The bot names the runtime has loaded, sorted. |
| `content_revision` | object | As `revision` above: the server's, not the workers'. |

**Not here, because nothing exposes it to `pmos-web`:** worker health (the runtime has no
worker-status read for the `web` client) and the content revision each worker loaded (workers
don't report one).
