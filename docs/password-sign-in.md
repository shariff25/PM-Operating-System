# Password sign-in with a required authenticator code

A second way to sign in to the web app, beside Entra ID: a **sign-in name and password, then
always a code from an authenticator app** (or one of ten single-use recovery codes). It is off
unless `PMOS_WEB_PASSWORD_LOGIN=1` is set on `pmos-web`, and it can run alone, or with Entra;
beside the loopback development sign-in only with `PMOS_WEB_DEV_LOGIN_WITH_PASSWORD=1` (a
developer's own machine). There is no way to sign in with a password alone and no setting that
turns the second step off.

The independent review of 2026-11-20 found two High and four Medium issues here, and some Lows;
what changed is in **What changed after review** at the end. Its re-check of `eb24d28`
(2026-11-21) found one High (a setup code could spend the shared hashing allowance), four
Mediums and nine Lows here and in the Entra first link; what changed is in **Re-check of
eb24d28**, at the end.

There is no email. A person gets a password by a **one-time setup code** an Admin makes on the
Team page (or, for an Admin or Auditor, someone with host access makes with
`pmos-admin users password-setup <handle>`), passed on by hand. A forgotten password, a lost
phone or a suspected compromise is the same: a new setup code, which replaces everything.

The contract the front end builds from is `docs/web-api.md` ("Password sign-in"); the tool
server's routes are in `docs/toolserver.md` ("Password sign-in"); the code is
`crates/toolserver/src/password/`, `crates/toolserver/src/app/password.rs`,
`crates/toolserver/src/users/password_pg.rs` (migrations `0020_password_sign_in.sql`,
`0022_password_review_fixes.sql` and `0023_gate_fixes_2.sql`) and
`crates/web/src/app/password.rs`.

## Where the secrets live

**The tool server owns users, so it owns their password hash, their authenticator secret and
their recovery codes**, in its own tables (migrations 0020, 0022, 0023):

| Table | Holds | Never holds |
| --- | --- | --- |
| `password_credentials` | the Argon2id PHC string, the authenticator secret sealed by the vault, the last accepted authenticator step, the credential's **epoch**, the two account-wide failure counts, **who issued** the code it was set with (`issued_via`: `host` or `web`), and a **reset notice** (who, when) until the person dismisses it | a password, a secret in the clear |
| `password_source_counters` | per person **and per address**: the failure counters behind the waits, and when the person last fully signed in from there; the address only as a SHA-256 of `pmos-web`'s normalized text | an address |
| `password_recovery_codes` | SHA-256 of each code (bound to the handle), used or not | a code |
| `password_setup_codes` | SHA-256 of each setup or reset code, who issued it (web or host), its expiry, use and replacement; how many setups it started, passwords it set and wrong guesses it drew, and when it was made void | a code |
| `password_pending` | SHA-256 of each token between steps, its owner, purpose, expiry and tries; during a setup the chosen password's hash, the sealed secret being enrolled, and how many passwords and secrets this token made | a token |

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
   **A reset is never silent.** A reset code also puts a notice on the person's credential (who
   issued it, an admin's handle or `host`, and when). From their next sign-in it is shown at the
   top of every page and on "Sign-in and security": "Your sign-in was reset by <name> on
   <date>. If this was not you, tell an admin." It stays, through the reset itself, until they
   dismiss it (`password.reset_notice`, `dismissed`). The dismissal is also a `role_changes` row
   (`dismiss_reset_notice`: who, when), so it can't go unrecorded even if whoever holds the
   account dismisses it. The admin overview lists the week's resets, read from `role_changes`
   by time (`password_resets`: whose, by whom, when, and when the notice was dismissed).
2. **The person opens the setup page** and enters their sign-in name and the code
   (`…/setup/begin`). Every refusal before the code matched is one answer, `401 setup_failed`:
   no such name, no code, a wrong, expired, used or replaced code, or an account waiting out
   its delay. Once the code matched, a refusal can say why: deactivated (`403 not_a_member`),
   an unconfirmed name (`409 sign_in_unconfirmed`), an Admin's or Auditor's code that the host
   didn't issue (`403 host_only`; the person's roles are checked again at every step), or a code
   that gave all the work it allows (`409 setup_code_spent`, below). A setup token (15 minutes,
   never past the code's expiry) goes into the `__Host-pmos-setup` cookie. **A wrong code counts
   against the person's live code itself**: at 20 wrong guesses (from any number of addresses)
   the code is void (`password.setup_code`, denied, `voided; failures 20`), the admin overview
   lists it (`password_codes_voided`), and the admin makes a new one, which starts from zero.
3. **A password** (`…/setup/password`): checked against the rules below, hashed, kept with the
   setup (not yet in force). **Bounded** (the re-check of eb24d28, H1): at most 3 per setup
   token (then `401 setup_expired`: start again with the code) and 6 per code across all its
   tokens; a code starts at most 5 setups. Past either, the code is spent (`409
   setup_code_spent`) and an admin issues a new one. Each call is counted before its hash,
   whatever comes of it, against the token, the code, the account's work (below) and, at
   `pmos-web`, the browser's address, **successes included**; its hash comes from the global
   limit's open allowance only, never the reserve.
4. **The authenticator** (`…/setup/authenticator`): the tool server makes a 160-bit secret and
   answers it once as base32 text and an `otpauth://` URI; the front end draws the QR code (the
   server has no image library). Calling it again makes a new secret, at most 5 times per setup
   token; each call counts against the address at `pmos-web`, successes included. The secret is
   shown only: never put in web storage, a readable cookie, the address or history (tested).
5. **A code from the app** (`…/setup/confirm`). Then, in one transaction: the password, the
   secret and ten new recovery codes **replace** whatever the person had; the setup code is
   spent; every pending token of theirs goes; the credential's **epoch** moves, so every
   password session of theirs ends; recorded `role_changes` `set_password` (`setup` or `reset`)
   and `password.setup_complete`. `pmos-web` also ends every session the person has (Entra and
   development ones included) and starts a new one in this browser. The recovery codes are
   shown once.

**Until a reset code is used, the old password keeps working**, by design: a reset is often for
someone who still signs in fine and only lost their recovery codes, and a code that is never
used changes nothing. To shut a person out at once, deactivate them (their sessions end within
10 seconds). An "ends at once" option on the reset was considered and not built: it isn't a
small change (it would end the credential before the person has a new one, which is what
deactivation is for).

**Considered and not built: requiring the person's second factor, or a second admin, for a
reset.** The reviewer suggested it so that a compromised admin (or `pmos-web`) can't take over
someone silently. It isn't built because a reset is exactly what a person who lost their phone
needs: if it took their authenticator, they could never be reset; and a second admin isn't
always there in a pilot. Instead a reset is never silent (above), every reset is recorded in
`role_changes` and the audit log with the actor and client, and **a reset for an Admin or
Auditor is host-only** (`403 host_only` from the web, checked at issue and again at every setup
step), so a compromised admin session or `pmos-web` can reset only people who hold neither.

### Signing in

1. **The password step** (`…/sign-in`): sign-in name and password. Every refusal is the same
   answer, `401 sign_in_failed`, with the same status, body and work: no such user, no password
   set, a wrong password, a deactivated user, an address waiting out its delay for this account,
   an Admin's or Auditor's web-issued credential (below). When there is no real hash to check, a
   hash of a random password is checked instead, so the time spent is the same class. What
   happened is in the audit log, by reason (`unknown_name`, `no_password`, `wrong_password`,
   `deactivated`, `locked`, `host_only`), never by value.
   **A web-issued credential doesn't survive a promotion.** The credential records who issued
   the code it was set with. When its person holds Admin or Auditor and an admin issued that code
   in the web app, the password step and the second step refuse it, uniformly, until someone with
   host access issues a setup code and the person sets up again; their status says
   `needs_host_setup`, `resolve` gives `pmos-web` no epoch for them, so their password sessions
   end at their next request (at once in the `pmos-web` that made the role change), and the Team
   page shows "Password needs host" (`password_needs_host_setup`, also in the role change's
   answer). A credential set before migration 0022 recorded nothing counts as web-issued unless
   the code it was set with is still on record as host-issued.
2. A correct password gives **only a pending token** (`__Host-pmos-pending` cookie): single
   purpose (the second step of this one person), five minutes, three tries, stored hashed,
   worthless after a reset or a password change (it carries the epoch). **It grants nothing
   else.** Correct password steps count against the account's work (10 in 15 minutes, then
   `429` for that account until the window moves), so whoever holds a password can't loop it
   to spend the shared allowance; a wrong guess never counts there.
3. **The second step** (`…/second-step`): an authenticator code, accepted for the step before,
   the current step or the step after, and only for a step later than the last one accepted (a
   code works once), or one of the recovery codes (each works once). A wrong code is
   `401 code_failed` with `attempts_left`. Then the session, exactly as the other sign-ins make
   it (`__Host-pmos`, rotated, with its CSRF token), holding the credential's epoch; the sign-in
   event `web.sign_in` with `provider: password`.

### Signed in

- **Change the password**: the current password and a fresh authenticator code. The epoch
  moves: every other session of the person ends; this browser gets a new session. Its two
  hashes come from the open allowance only, and each call counts against the account's work
  and the browser's address, whatever its outcome (LA).
- **New recovery codes**: an authenticator code; the ten old ones stop working. No Argon2 hash;
  each call counts against the account's work and the address all the same.
- **Every request** of a password session resolves the person in the user database (at most 10
  seconds old) like an Entra session: not active, every session of theirs ends
  (`403 not_a_member`); the epoch moved (a reset, a change) or is missing (a held credential),
  the session ends (`401 signed_out`). The epoch is in `resolve`'s answer to `pmos-web` only.
- **When the tool server can't be reached, a password session fails closed** (`503
  users_unavailable`), even for the two reads other sessions may serve for up to 5 minutes on
  their last-known-good answer: a session's epoch can't be checked then, and a reset or a
  password change must end it within the 10 seconds above, not 5 minutes. So the bound is
  10 seconds whether the tool server answers or not. (Chosen over stating a 5-minute bound: it is
  one condition in `pmos-web`'s session check, and the cost, a password user seeing `503` on
  their home page during an outage, is what every write already does.)
- **A password session always carries an epoch**: a sign-in, setup or change whose answer has
  none starts no session (`502`), so no password session can go unchecked.

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
| Request body on the password routes | 4 KiB (`413` at `pmos-web`) | tool server and `pmos-web` |
| Failures per account **per address** before that address waits (passwords and second-step codes **counted together**) | 5 | tool server, per person and address (an IPv4 address, an IPv6 /64) |
| The wait | 30 s after the 5th failure, doubling with each further one (1, 2, 4, 8 min), **at most 15 min** from the 10th; **for that address only** | tool server |
| An attempt from that address during its wait | refused with the same answer, **not checked and not counted** | tool server |
| Failures on one account from **every** address together | **50**; past it (until 24 hours with no failure) an address the person hasn't fully signed in from in the last 30 days is **rationed**: after each failure it waits 15 minutes at once (one attempt per 15 minutes per address). It is never refused for the ceiling alone, so the right password and code from a new address always get in. Reaching it is audited (`password.lockout`, `ceiling`) | tool server, per person |
| Failures forgotten | 24 hours after the last one | tool server |
| Back to zero | a full sign-in (that address's count), a finished setup or reset (every address's), a password change (the person's re-authentication count) | tool server |
| Setup-code guesses | the same schedule per address (5, then 30 s doubling to 15 min), a counter of their own; **no** account-wide ceiling; each wrong guess also counts against the person's live code, which is **void at 20** (listed on the admin overview); a new code clears every count and starts at zero | tool server, per person and per code |
| Re-authentication while signed in (change the password, new recovery codes) | the same schedule, in one bucket per person apart from every address | tool server, per person |
| Hashing work after an authentication (a correct password step, a setup's password, a password change, new recovery codes) | **10** per account in any rolling 15 minutes, then `429` for that account; a correct password step is checked before its hash and counted once it passed (a wrong guess by anyone never uses the room); the others are counted before their work, whatever its outcome | tool server, per person, per process |
| A setup token | **3** password sets and **5** new authenticator secrets; past either, the token is spent (`401 setup_expired`) | tool server, per token |
| A setup code, across its tokens | **5** setups started and **6** password sets (two tokens' worth: room for a typo or a setup abandoned and started again); past either, the code is spent (`409 setup_code_spent`) until an admin issues a new one | tool server, per code |
| Password hashes, all callers together | **300** in any rolling minute, plus a **reserve of 120** that only a **password step from an address someone fully signed in from in the last 30 days** may use, decided from the address before the name is looked up (`429 too_many_attempts`); setup and the signed-in routes take from the 300 only; routes that compute no hash take none, and an invalid token is checked before anything | tool server, whole process |
| Hashing | 2 at a time; a 10-second wait at most (`503 sign_in_busy`) | tool server |
| Refused attempts per source address | 20 in any rolling 15 minutes, **every** refusal counted (a wrong password or code, an unknown or expired token, a step out of order, a malformed body; not PM OS's own `5xx`), and **every** `setup/password`, `setup/authenticator`, `change` and `recovery-codes` call, successes included; then those routes answer `429 too_many_attempts` with `Retry-After`; the check and the count are one step | `pmos-web`, per process, per IPv4 address or IPv6 /64; at most 10,000 addresses tracked |
| Pending token (after the password) | 5 minutes, 3 tries, single use | tool server |
| Setup token | 15 minutes (never past the code), 5 tries at the confirmation | tool server |
| Setup or reset code | 100 bits, 24 hours, single use; a new one replaces the old | tool server |
| Recovery codes | 10, 80 bits each, single use | tool server |
| Authenticator | TOTP (RFC 6238): HMAC-SHA-1, 6 digits, 30-second steps, the step before and after accepted, each step once; a 160-bit secret | tool server |
| Pending sign-ins per person | 5 at once (the oldest go) | tool server |

**No lockout an attacker can hold on someone.** The wait is per person **and per address**: ten
wrong guesses from one address make that address wait for that account, and the owner, from
their own address, signs in at once with the right password and a right code (tested). Nobody
else's guessing can make the owner wait from an address they have signed in from, and since the
re-check of eb24d28 (M2) not from a **new** address either: past the account's ceiling a new
address is rationed, never refused, so the owner's first attempt from it is checked and the
right password and code get in (tested, past the ceiling, from an address never used). A setup
code has no account-wide ceiling at all: guesses are counted against the code, a void code says
so on the overview, and a new code starts clean.

**Why guessing stays bounded.** Per account, from addresses the person has never fully signed
in from: **50** wrong guesses from anywhere until the count is forgotten (24 hours with no
failure at all); past that, **one guess per 15 minutes per address** (96 a day). So with A
addresses (or IPv6 /64s) an attacker can check at most **50 + 96 × A** passwords a day against
one account; from the person's own recent addresses, each has its own schedule (about **104** a
day: 5, then waits of 30 s, 1, 2, 4 and 8 minutes, then one every 15 minutes), usable only from
that network. `pmos-web` adds 20 refused attempts per address per 15 minutes on top. Password
guesses are behind Argon2id, against a 12-character minimum with the common passwords refused;
and a right password still needs the authenticator code (3 tries per pending token, counted the
same way). This is a weaker count bound than the 50 a day before, by design: the owner can
always get in from anywhere, and an attacker with many addresses still faces Argon2id and the
second factor.

**The global limit can't shut everyone out.** It bounds hashing only (the per-account and
per-address limits bound guessing, and two hashes at a time bound the CPU). The reserve of 120 is
for password steps **from an address someone fully signed in from** in the last 30 days, decided
from the address alone before the sign-in name is looked up (M3), so an unknown name and a real
account get the same answer under load (tested with the open allowance full: both `429` from a
new address, both the uniform `401` from a known one). A flood of junk can't stop a sign-in from
a known address (tested), and setup and the signed-in routes never use the reserve (H1, LA).
What it costs: under a flood, someone signing in from an address nobody has signed in from (a
new person on a new network) waits until the flood ends; a new person's first sign-in is
usually from the address they finished setup from, which is known. Routes that compute no hash
(the second step, a setup's first step and confirmation) don't touch it, and a made-up or
expired token is checked, with one lookup, before any work.

**No credential holder can spend what everyone shares** (H1). Each kind of work that follows an
authentication is bounded per token, per account and per address:

| Route | Per token | Per account | Per address (`pmos-web`) | Global |
| --- | --- | --- | --- | --- |
| `sign-in` (a correct password) | (n/a) | 10 correct steps / 15 min | refusals only (a NAT'd office's sign-ins don't share a cap) | open, or the reserve from a known address |
| `second-step` (no hash) | 3 tries | the failure counts above | every refusal | none |
| `setup/password` | 3 | 10 / 15 min; 6 per code | every call | open only |
| `setup/authenticator` (a seal) | 5 | 5 setups per code | every call | none |
| `setup/confirm` (no hash) | 5 tries | 5 setups per code | every refusal | none |
| `change` (two hashes) | (session) | 10 / 15 min; the re-authentication count | every call | open only, both |
| `recovery-codes` (no hash) | (session) | 10 / 15 min; the re-authentication count | every call | none |

**The per-address limit counts real addresses.** `pmos-web` refuses to start with password
sign-in on unless it knows where browsers' addresses come from: either
`PMOS_WEB_TRUSTED_PROXIES` names the reverse proxy, or `PMOS_WEB_DIRECT=1` says browsers reach
it directly with no proxy, which is allowed on a loopback `PMOS_WEB_BIND` only (the test stack).
With no trusted proxy, a password request that carries `X-Forwarded-For`, `Forwarded` or
`X-Real-IP` is refused (`403 proxy_not_trusted`): a proxy nobody named is in front, and every
browser would share its address. Behind a trusted proxy, `X-Forwarded-For` (every header, in
order) is read strictly from the right, **entry by entry on the raw bytes** (LI: an entry that
isn't text, or isn't an address, spoils only itself, so nginx's appending
`$proxy_add_x_forwarded_for`, which puts whatever the client sent to the left of the address
nginx saw, still names the client): each entry that is a trusted proxy is passed, the first
that isn't is the browser; `ip:port`, `[ipv6]` and `[ipv6]:port` are read; an entry that can't
be read ends the walk and is never skipped. **A wrong or stale setting is loud, never one shared
count** (M1): with a list set, a forwarded header from a peer that isn't on it (the proxy's
address moved), and a trusted proxy that names no client (no `X-Forwarded-For`, only proxies in
it, or an unreadable entry right after the trusted hops), are refused (`403
proxy_not_trusted`), counted on `/readyz` (`untrusted_proxy_refusals`, since start-up) and
logged at error level at most once a minute with the count and what to fix. A peer that isn't a
trusted proxy and forwards nothing is counted by its own address. An IPv6 address counts by its
/64.

**How the tool server knows the address** (M1). Only from `pmos-web`: the `source` field of a
password call, which `pmos-web` makes with its own token (`PMOS_WEB_TOKEN`; no other client
reaches these routes). The tool server never uses the peer address of the connection (that is
`pmos-web`'s own), and `pmos-web` never sends a proxy's address as anyone's: a request whose
client it can't name is refused before the tool server is asked. A call with no `source` (only
a `pmos-web` served without peer addresses: tests) shares one key, and that key **never** becomes
anyone's known address (tested), so it can't open the reserve or the ceiling's exemption. The
address is hashed before it is stored and never logged.

**Behind the deploy kit's reverse proxy.** The web app listens on `0.0.0.0:8770` in its
container and is published on the host's `127.0.0.1:8770`; the reverse proxy on the host reaches
it there, so its requests arrive from the `pmos` network's **gateway**. `deploy/compose.yaml`
**pins** that network's subnet and gateway (`PMOS_NETWORK_SUBNET`, default `10.201.77.0/24`;
`PMOS_NETWORK_GATEWAY`, default `10.201.77.1`), so the gateway doesn't move when the network is
made again; the default is outside Docker's own pools (`172.17`–`172.31.0.0/16`,
`192.168.0.0/16`), so Docker never gives it to another network, and a site that already routes
it sets both in `deploy/.env`. `deploy/env.example` sets `PMOS_WEB_TRUSTED_PROXIES=10.201.77.1`
to match. `deploy/pmos-admin up --web` refuses, before anything starts and with the exact fix:
while `PMOS_WEB_TRUSTED_PROXIES` is blank (it prints the gateway); while the network's real
gateway (`docker network inspect pmos_pmos`) isn't `PMOS_NETWORK_GATEWAY` (a network made before
it was pinned: `deploy/pmos-admin down && deploy/pmos-admin up --web` re-creates it); while an
address in `PMOS_WEB_TRUSTED_PROXIES` is neither the gateway nor inside the pinned subnet (a
proxy in a container on the `pmos` network has an address inside it); and while
`PMOS_WEB_PUBLIC_URL` isn't `https://`. The proxy must set `X-Forwarded-For` (Caddy does; nginx
with `proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for`). Anything else on the host
that connects to the published port also arrives from the gateway and is believed about its
`X-Forwarded-For`; only the host's own processes can do that. **Not verified on the Spark:**
that the published port's traffic arrives from the gateway (it does with Docker's default
userland proxy and with its iptables rules). If it doesn't, nothing is shared silently any more:
every password request is refused `403 proxy_not_trusted` and `/readyz` counts them, so the
first sign-in after setting it up shows it.

## Threats

| Threat | How the design answers it |
| --- | --- |
| Guessing passwords online | Argon2id per guess; 5 failures per account per address then a growing wait to 15 min; 50 per account from anywhere new until 24 quiet hours, then one guess per 15 minutes per new address (at most 50 + 96 × A a day with A addresses); 20 refused attempts per address per 15 min at `pmos-web`; 2 hashes at a time; and a right password still needs the second step. Guesses in parallel can't pass the threshold: a failure is counted before the hash is checked and taken back when the password was right (tested with 12 at once); `pmos-web`'s check and count are one step. |
| Telling which accounts exist | One answer, status and work at the password step and the setup's first step for every reason; a dummy hash where there is no real one; a waiting address answers the same; **under load too**: whether an attempt may use the reserve is decided from the address alone, before the name is looked up (M3, tested with the open allowance full). An Auditor reading the audit log sees one reason (`sign_in_failed`, `setup_failed`) and no target for those refusals; an Admin sees each reason. The one other answer: an account whose correct password was given 10 times in 15 minutes answers `429`, which only the password's holder can cause. |
| Shutting everyone out with junk | The global limit has a reserve only password steps from known addresses may use; an invalid token costs nothing; every refused attempt counts against its address at `pmos-web`, which counts real browser addresses (a trusted proxy is required, or a loopback-only direct setting). |
| A credential holder spending what everyone shares (H1) | A new invitee, someone with an intercepted setup code, a signed-in user or someone with a password: every route that hashes or makes a secret after an authentication is bounded per token, per account (10 pieces of work in 15 minutes) and per address (every call counted at `pmos-web`), a setup code gives at most 5 setups and 6 password sets, and setup and the signed-in routes hash from the open allowance only, never the reserve. The loop from the re-check is a test: one code sets 6 passwords in all, and a sign-in from another address goes through meanwhile. |
| A wrong or stale trusted-proxy setting (M1) | The `pmos` network's gateway is pinned; `pmos-admin up --web` refuses a network whose real gateway differs and a trusted address that is neither the gateway nor in the subnet; `pmos-web` refuses a forwarded header from a peer not on the list and a trusted proxy that names no client, and counts both on `/readyz` with an error in its log. No setting makes every browser share one count silently. |
| Plain HTTP between the services inside the stack (LJ) | **Accepted, as a property of the single-host deploy kit.** `pmos-web` reaches the tool server at `http://toolserver.pmos.internal:8710` on the `pmos` Docker bridge network (`deploy/compose.yaml`), so its token, a forwarded ID token on a first link, and what a person typed into a password step cross that network unencrypted. Every container on it runs from the kit's own images on one host, the network isn't published, and the browser's connection to the reverse proxy is `https://` (required with password sign-in). Reading it takes root on the host or a container on the network, which already reaches the database and the vault key. **What would change it:** running the services on more than one host, or on a network shared with anything that isn't PM OS (another project's containers, an orchestrator's overlay), or a policy that needs encryption in transit everywhere: then each service needs TLS (or mutual TLS) on its internal listener, with the tool server's and the runtime's URLs moved to `https://`. Not built. |
| A stolen password | Useless alone: the second step is required, always, with no switch to turn it off. |
| A replayed authenticator code | Each step is accepted once (the last accepted step is stored); a code older than the last accepted one is refused too. |
| Guessing authenticator codes | Only after a correct password, 3 tries per pending token, counted with password failures toward the same wait. |
| Stolen recovery codes | Single use, 80 bits, stored as SHA-256 bound to the person; regenerating replaces all ten. |
| A copy of the database | Argon2id hashes; sealed secrets that need the vault key and don't unseal for another person; SHA-256 of high-entropy codes and tokens only. |
| A stolen setup code | 24 hours, single use, needs the person's sign-in name; an Admin's or Auditor's must come from the host; a new code replaces it. The per-address waits apply to setup-code guesses too; guesses also count against the code, void at 20 (on the overview), and a fresh code is not held up by them. Whoever holds the code can do at most 5 setups and 6 password sets with it. **Residual:** someone who knows a person's sign-in name and keeps guessing from enough addresses can void each new code (four addresses, five guesses each, reach 20 at once); it shows on the overview each time, and the remedy is to issue the code and have the person use it at once. |
| An admin taking over another admin | Web-issued codes never work for an Admin or Auditor (checked at issue and again at every setup step), so a compromised `pmos-web` or admin session can't reset an Admin's or Auditor's password. A credential set with a web-issued code stops working when its person becomes an Admin or Auditor (refused at the password step and the second step, password sessions ended) until a host-issued setup replaces it. Nobody issues a code for themselves. |
| A compromised `pmos-web`, or an admin, resetting someone | Only people who hold neither Admin nor Auditor; each reset recorded in `role_changes` with the actor and `client = web`, listed on the overview, and shown to the person until they dismiss it ("reset by <name> on <date>"). It never sees a hash, a secret or a stored code. Requiring the person's second factor or a second admin was considered and not built (a person who lost their phone could never be reset). |
| Another service's token | The password routes answer only `pmos-web`'s token. |
| Login CSRF and cross-site posts | Every password write needs the app's origin and a JSON body; the signed-in ones the session's CSRF token too; the step cookies are `SameSite=Strict`, `HttpOnly`, `__Host-`. |
| Session theft after a reset or change | The session carries the credential's epoch and ends when it moves (≤ 10 s; at once in the `pmos-web` that made the change, which also ends the person's other sessions). The bound holds while the tool server is unreachable too: a password session never runs on a last-known-good answer. A session without an epoch is never started. |
| A deactivated person | Can't sign in (the uniform refusal), can't finish a second step or a setup, and their sessions end within 10 seconds. At the end of the 24-hour grace their password credential is deleted with their linked accounts. |
| A lockout used to shut someone out | The wait is per address: someone else's guesses never make the owner wait from their own address. Past the account's ceiling a new address is rationed (one attempt per 15 minutes), never refused, so the owner gets in from anywhere with the right password and code (M2). A setup code has no account-wide ceiling (see "A stolen setup code" for its residual). |
| Secrets in logs or the audit log | No password, code, token, secret or sign-in name is logged or audited; details are fixed words and counts (`password::is_safe_detail`), tested by capturing the log and reading every audit event. |
| Exhausting the CPU through sign-in | Two hashes at a time, a 10-second wait, the global and per-address limits. |
| The `evals` account | Can never have a password: refused at issue, and by the tables' checks. |

## What is recorded

Audit events (`client` `web`, or `host` for a host-issued code; `task_id` `password`), metadata
only:

| `action` | `actor` | `outcome` | `target` | `detail` |
| --- | --- | --- | --- | --- |
| `password.sign_in` | `anonymous` | `ok` / `denied` | the person, or none for an unknown name | `password` (`; rehashed`) / `unknown_name`, `no_password`, `wrong_password`, `deactivated`, `locked`, `host_only`, `stale` |
| `password.second_step` | the person / `anonymous` | `ok` / `denied` | the person | `authenticator`, `recovery_code; recovery_left 9` / `wrong_code`, `locked`, `deactivated`, `expired`, `host_only`, `stale` |
| `password.reset_notice` | the person | `ok` | the person | `dismissed` |
| `password.lockout` | `anonymous` | `denied` | the person | `failures 5` (`; setup` for setup-code guesses); `ceiling` when the account reaches 50 failures |
| `password.setup_code` | the admin / `host` / `anonymous` | `ok` / `denied` | the person | `setup` or `reset` (`host_user <name>; …` from the host) / the refusal's code / `voided; failures 20` (a code void after wrong guesses) |
| `password.setup_begin` | `anonymous` | `ok` / `denied` | the person, or none | `begin; setup` / `unknown_name`, `wrong_code`, `expired`, `locked`, `deactivated`, `spent`, … |
| `password.setup_complete` | the person / `anonymous` | `ok` / `denied` | the person | `setup` or `reset` / `wrong_code`, `locked`, `host_only`, … |
| `password.change` | the person | `ok` / `denied` | the person | `change` / `reauth_failed`, `locked` |
| `password.recovery_codes` | the person | `ok` / `denied` | the person | `regenerated` / `reauth_failed`, `locked` |
| `web.sign_in`, `web.sign_out` | the person | `ok` | none | `provider: password` (from `pmos-web`) |

**What an Auditor sees** (L15): a refused `password.sign_in`, and a refused
`password.setup_begin` before its code matched, show the one reason `sign_in_failed` (or
`setup_failed`) and no target, so the log can't tell an Auditor whether a sign-in name they
tried exists. An Admin (and a person holding both roles) sees each reason and target.

**How reliable the records are** (L14). Every `password.*` event is written by the tool server
in the same transaction as the decision it records: if the audit log can't take it, nothing
changes and the request fails (`503`). `web.sign_in` and `web.sign_out` come from `pmos-web`
afterwards and are **best effort**: a sign-in is not refused because its event couldn't be
recorded (a session alone reaches nothing; everything it does is audited where it is done).
A failure is never silent: `pmos-web` logs `AUDIT: a sign-in event wasn't recorded` at error
level and counts it in `/readyz` (`unrecorded_sign_in_events`, since start-up); the tool
server's own `password.second_step` event records the same sign-in in any case.

`role_changes`: `issue_password_code` and `set_password`, each with detail `setup` or `reset`,
the actor, the client and, from the host, `host_user`; and `dismiss_reset_notice` (detail
`reset`, the person's own session, when) since migration 0023. A void setup code is kept for the
overview's week after it expires. The usual retention applies: audit
events go at 90 days, `role_changes` rows 90 days after the person's deactivation. Pending
tokens and setup codes are deleted once past their expiry, an address's counters once they hold
no failure from the last 24 hours and no sign-in from the last 30 days; a deactivated person's
credential (and their counters) at the end of the 24-hour grace.

## Configuration

| Variable | Service | What it does |
| --- | --- | --- |
| `PMOS_WEB_PASSWORD_LOGIN=1` | `pmos-web` | Turns password sign-in on (off by default); `GET /api/auth/providers` then lists `password`. Refuses to start without a `web:toolserver` signing key or with `PMOS_ASSERTION_MODE` other than `require`; without `PMOS_WEB_TRUSTED_PROXIES` or `PMOS_WEB_DIRECT=1`; and on a bind that isn't loopback without an `https://` `PMOS_WEB_PUBLIC_URL`. |
| `PMOS_WEB_TRUSTED_PROXIES` | `pmos-web` | **Required with password sign-in** unless `PMOS_WEB_DIRECT=1`: the address(es) the reverse proxy reaches `pmos-web` from, comma-separated. Their `X-Forwarded-For` names the browser; a forwarded header from any other peer, or a trusted proxy that names no client, is refused and counted on `/readyz` (`untrusted_proxy_refusals`). In the deploy kit: the `pmos` network's pinned gateway, `10.201.77.1` by default (above). |
| `PMOS_NETWORK_SUBNET`, `PMOS_NETWORK_GATEWAY` | deploy kit (`deploy/compose.yaml`) | The `pmos` network's subnet and gateway, pinned (default `10.201.77.0/24`, `10.201.77.1`). Change both only if the site already routes that range; then re-create the network (`deploy/pmos-admin down`, then `up`) and set `PMOS_WEB_TRUSTED_PROXIES` to the new gateway. `pmos-admin up --web` checks all three. |
| `PMOS_WEB_DIRECT=1` | `pmos-web` | Browsers reach `pmos-web` with no proxy: each is counted by its own address, and a forwarded header is refused. Only on a loopback `PMOS_WEB_BIND`, and never with `PMOS_WEB_TRUSTED_PROXIES` (both refuse to start). The test stack uses it. |
| `PMOS_WEB_DEV_LOGIN_WITH_PASSWORD=1` | `pmos-web` | Lets development sign-in run beside password sign-in (a loud warning at start-up). Without it, both on refuses to start: development sign-in lets anyone in without a password. A developer's own machine only. |
| `PMOS_VAULT_KEY` | tool server | The vault's keys; password sign-in needs them to seal authenticator secrets. Required with live connectors; optional with fake ones. |
| `PMOS_DEV_PASSWORD_SETUP_FOR`, `PMOS_DEV_PASSWORD_SETUP_FILE` | tool server | **Development only**: at start-up, issue a setup code for that seeded user, as the host would, into a new 0600 file. Refused with a database, live connectors, no seed or no vault key. `scripts/claude-test-stack.sh` uses it for `PMOS_TEST_PASSWORD_LOGIN=1`. |

In the deploy kit, `deploy/compose.yaml` passes `PMOS_WEB_PASSWORD_LOGIN`,
`PMOS_WEB_TRUSTED_PROXIES`, `PMOS_WEB_PUBLIC_URL` and `PMOS_ASSERTION_MODE` to the web app (never
`PMOS_WEB_DIRECT`), and `deploy/pmos-admin up --web` starts the web app with password sign-in as
its provider too, once `PMOS_WEB_TRUSTED_PROXIES` and an `https://` `PMOS_WEB_PUBLIC_URL` are set
(it says what to set otherwise). `pmos-admin users password-setup <handle>` issues a code on the
host.

## The screens

Built in `web/` from `docs/web-api.md`: the sign-in card with the code step, the setup page
(`#/setup`, the QR code drawn in the browser), "Sign-in and security" (`#/security`: change
password, new recovery codes), the reset notice at the top of every page until dismissed, the
Team page's setup or reset code action and its "Password needs host" tag, and the overview's
list of the week's resets.

## What changed after review

The independent review of `b469d5b` (2026-11-20). Each change has a test that failed with the
fix taken out.

| Finding | What changed |
| --- | --- |
| P1 (High): anyone could exhaust the shared limit | The token is checked before any work; routes that compute no hash take nothing from the global limit; the global limit has a reserve for real accounts below their threshold; `pmos-web` counts **every** refused attempt against the address, not three codes. |
| P2 (High): the per-address limit was one limit for everyone behind the kit's proxy | `pmos-web` refuses to start with password sign-in on unless `PMOS_WEB_TRUSTED_PROXIES` is set or `PMOS_WEB_DIRECT=1` (loopback only); a forwarded header with no trusted proxy is refused; `pmos-admin up --web` says what to set (the `pmos` network's gateway). |
| PM1: a lockout an attacker could hold | The wait is per account and address; an account-wide ceiling of 50 past which only known addresses may try. The "known residual" paragraph is gone. |
| PM2: a silent reset | A reset leaves the person a notice until they dismiss it, and the overview lists resets. A second factor or second admin was considered and not built (above). |
| PM3: a web-issued credential survived promotion | The credential records `issued_via`; a web-issued one is refused for an Admin or Auditor until a host-issued setup, their password sessions end, the Team page says so. |
| PM4: a session could outlive a reset by 5 minutes | A password session never uses the last-known-good answer: 10 seconds is the bound either way. |
| L1 | `X-Forwarded-For` read strictly from the right; `ip:port` and `[ipv6]:port` read; an unreadable entry ends the walk. |
| L2 | The per-address check and count are one step. |
| L3 | IPv6 counts by /64, at `pmos-web` and at the tool server. |
| L4 | Development sign-in beside password sign-in needs `PMOS_WEB_DEV_LOGIN_WITH_PASSWORD=1`. |
| L5 | Password sign-in on a bind that isn't loopback needs an `https://` public URL. |
| L6 | 4 KiB body limit on `pmos-web`'s password routes too. |
| L7 | A password sign-in whose answer has no epoch starts no session. |
| L12 | The types that hold a hash, a sealed secret or a token's hash write their own `Debug`, redacting them (tested). |
| L14 | No code change: the tool server's events were already in the decision's transaction, and `pmos-web`'s best-effort sign-in events already logged and counted their failures; this document now says so. |
| L15 | An Auditor sees one reason, and no target, for refused password steps. |

## Re-check of eb24d28

The independent re-check of `eb24d28` (2026-11-21). Each change has a test that reproduces the
finding; the ones that use behaviour or tables that didn't exist before fail by construction
against `eb24d28`, and the purge-trigger one (LG) was also run against the old migrations and
failed there. Migration `0023_gate_fixes_2.sql`. Findings about the Entra first link (M4, LB to
LE) are in `docs/entra-sign-in.md`, "Re-check of eb24d28".

| Finding | What changed | The test |
| --- | --- | --- |
| H1 (High): one setup token or code could fill the shared hashing allowance and its reserve | Per setup token: 3 password sets, 5 authenticator secrets, then the token is spent. Per code across renewals: 5 setups started, 6 password sets, then `409 setup_code_spent` until an admin issues a new one (6 = two tokens' worth: a typo or an abandoned setup still finishes). Setup hashes come from the open allowance only. `pmos-web` counts every `setup/password` and `setup/authenticator` call against the address, successes included. Every route that hashes after any authentication is bounded per account: 10 pieces of work in 15 minutes (a correct password step, a setup's password, a change, new recovery codes). | `one_setup_code_cannot_spend_the_shared_allowance` (the loop: 6 sets in all, then spent; with the open allowance full of setup work a sign-in from another address goes through); `correct_password_steps_are_bounded_per_account`; `setup_passwords_count_against_the_address_even_when_they_succeed` (`pmos-web`) |
| LA: `change` and `recovery-codes` hashed outside the global limit | `change` takes both hashes from the open allowance before either; both routes count against the account's work and, at `pmos-web`, the address on every call. `recovery-codes` computes no Argon2 hash (SHA-256 only), so it takes nothing from the global limit. | `change_and_recovery_codes_are_bounded`; the `pmos-web` test above |
| M1: a wrong or stale `PMOS_WEB_TRUSTED_PROXIES` silently restored one shared limit | The `pmos` network's subnet and gateway are pinned (`10.201.77.0/24`, `10.201.77.1`, overridable). `pmos-admin up --web` refuses a network whose real gateway differs (re-create it) and a trusted address neither the gateway nor in the subnet, with the exact line to set. `pmos-web` refuses a forwarded header from a peer not on the list and a trusted proxy that names no client (`403 proxy_not_trusted`), counts them on `/readyz` (`untrusted_proxy_refusals`) and logs an error at most once a minute. The tool server takes the address only from `pmos-web`'s token-authenticated `source`, `pmos-web` never sends a proxy's address, and the shared no-address key never becomes a known address. The test stack (loopback, `PMOS_WEB_DIRECT=1`) is unchanged. | `a_wrong_or_stale_proxy_setting_is_refused_and_counted` (`pmos-web`); `a_wrong_or_stale_proxy_setting_is_refused_not_shared` (unit); `no_address_never_becomes_a_known_address`; `scripts/test-pmos-admin.sh` (stale network, wrong address, container proxy, override, the rendered subnet) |
| M2: the account ceiling was a lockout an attacker could hold, for sign-in and for setup codes | Past the ceiling an unknown address is rationed (one attempt per 15 minutes), never refused: the right password and code always get in. Bound: 50 + 96 × A guesses a day with A addresses, then the second step. Setup codes have no account-wide ceiling: each wrong guess counts against the live code, void at 20 (audited, listed on the admin overview as `password_codes_voided`); a new code starts clean and isn't held up. The residual (a determined guesser can void each new code) is in the threat table. | `guesses_from_one_address_never_hold_the_owner_out` (the owner from a new address past the ceiling); `setup_code_guesses_void_the_code_not_the_person`; `an_address_waits_alone_and_past_the_ceiling_new_addresses_are_rationed` (unit) |
| M3: under load an unknown name got `429` and a real account `401` | Whether a password step may use the reserve is decided from the address alone, before the name is looked up: an address someone fully signed in from in the last 30 days. Every refusal on that path draws from the same allowance. | `under_load_an_unknown_name_and_a_real_account_answer_alike` (open allowance full: both `429` from a new address, both `401` from a known one) |
| LF: the reset notice could be dismissed unrecorded by whoever holds the account; the overview read only the last 500 role changes | A dismissal is a `role_changes` row (`dismiss_reset_notice`: who, when). The overview reads resets and dismissals from `role_changes` by time and shows each reset's `dismissed_at`. | `a_dismissal_is_recorded_and_resets_are_read_by_time` (520 later role changes; the reset is still listed) |
| LG: the purge triggers matched a function name in PG_CONTEXT, which a quoted name containing a newline could spoof | Confirmed: identifiers are cut at 63 bytes, so the spoof fits beside a short schema name (role history's frame with a schema of up to 10 characters, `public` included; the audit log's with one), and with the old triggers the crafted function deleted old rows. Now the purge functions mark their own transaction in `purge_marks` (owner-only; EXECUTE on the purges revoked from PUBLIC) and the triggers look for the mark; no name is read. | `postgres_a_crafted_function_name_cannot_pass_as_the_purge` (a one-letter schema; failed against migration 0022, passes with 0023) |
| LH: `errorText` showed the server's message for an unknown code | It shows the caller's fixed sentence for any code the app doesn't know (and the app's own sentence for "no answer"); `messageOf` does the same for unknown codes. | `web/src/errors.test.ts` |
| LI: with nginx's appending header, a non-text client value failed the whole header and the client counted as the proxy | `X-Forwarded-For` is split and parsed entry by entry on the raw bytes, from the right; the first unreadable or untrusted entry is the boundary, and a walk that reads only trusted hops is refused (M1), never the proxy's address. | `a_malformed_client_entry_with_an_appending_proxy_is_its_own_entry` (non-UTF-8 bytes, `unknown`, bad ports, empty entries) |
| LJ: plain HTTP between the services inside the stack | Not changed: documented in the threat table as a property of the single-host deploy kit, with what would change it. | (none) |
| Missing tests | The authenticator secret is absent from web storage, readable cookies, the address and history on the setup page, before and after it finishes. | `keeps the authenticator secret out of web storage, cookies and history` (`Setup.test.tsx`) |

## Not built

- **Self-service reset by email**, by design: there is no email sending.
- **WebAuthn or passkeys**; only TOTP and recovery codes.
- **Breached-password lookups** (for example Have I Been Pwned): only the built-in list, so no
  password, or part of its hash, leaves PM OS.
- **Shared limits across several `pmos-web` or tool server processes**: `pmos-web`'s
  per-address limit and the tool server's global limit and per-account work are per process (one
  of each in the deploy kit; a restart empties them). The per-account, per-address waits, the
  account-wide ceiling and the per-token and per-code counts are in the database and hold across
  processes and restarts.
- **"End the old credential at once" on a reset** (above).
- **Not verified:** a real authenticator app scanning the URI (the codes are tested against
  RFC 6238's vectors and the URI against the Key Uri Format); a deployment behind a real reverse
  proxy, and the address it appears from on the Spark (the `pmos` network's gateway, by Docker's
  documented behaviour).
