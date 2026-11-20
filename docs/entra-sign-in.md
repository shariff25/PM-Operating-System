# Entra ID sign-in for the web app

`pmos-web` can sign people in with Microsoft Entra ID (Azure AD): the OpenID Connect
authorization code flow with PKCE, as a confidential client of **one** tenant. It is off unless
configured, and it sits beside development sign-in behind the same provider interface
(`crates/web/src/entra.rs`). The API is in `docs/web-api.md` ("Sign-in and safety"); the
backend's checks are in `crates/web/README.md` ("How a request is checked").

Entra proves who the browser is. **Who may use PM OS, and with which roles, comes only from
the user database** (`docs/user-database-spec.md`), which the tool server owns. `pmos-web`
asks it through `GET /internal/users/resolve` and `POST /internal/users/{handle}/identities/entra`
(`docs/toolserver.md`, "The user database's read API"), with the client in
`crates/directory/src/resolve.rs`. There is no team file: `team.yaml` was retired in slice U7,
and none decided anything here since U4c.

## What to register in Entra

In the Microsoft Entra admin center, **App registrations → New registration**:

| Setting | Value |
| --- | --- |
| Supported account types | **Accounts in this organizational directory only** (single tenant). |
| Redirect URI | Platform **Web**, `<PMOS_WEB_PUBLIC_URL>/api/auth/callback`, for example `https://pmos.example.com/api/auth/callback`. Exactly this; `https` (Entra allows `http` only for `localhost`). |
| Certificates & secrets | One **client secret**. Put it in the secret store, never in a file in git. Note its expiry: sign-in stops working when it lapses. |
| Authentication | Implicit grant and hybrid flows: leave **both unticked** (ID tokens come from the code exchange, not the front channel). Front-channel logout URL: none. |
| API permissions | **No extra permissions.** The default delegated `User.Read` may stay or go; `pmos-web` asks only for `openid profile` and never calls Microsoft Graph. |
| **Token configuration** | **Required: add the optional claim `acct` to the ID token** (Token configuration → Add optional claim → ID → `acct`). Without it every sign-in is refused (`reason=entra_misconfigured`): PM OS can't tell a member of the tenant from a guest, so it lets no one in. Nothing else: no `email`, no group or role claims. |

Then note the **Directory (tenant) ID** and the **Application (client) ID** from the overview.
Admins and Auditors also need their **Object ID** (Entra admin center → Users → the person →
Object ID) for the host command below.

## Configuration

| Variable | What it is |
| --- | --- |
| `PMOS_WEB_ENTRA_TENANT_ID` | The directory (tenant) id, a GUID. `common`, `organizations` and `consumers` are refused: they would let any tenant's accounts in. |
| `PMOS_WEB_ENTRA_CLIENT_ID` | The application (client) id, a GUID. |
| `PMOS_WEB_ENTRA_CLIENT_SECRET` | The client secret. Never logged, never in an error message. |
| `PMOS_WEB_PUBLIC_URL` | The origin browsers use, `https` (or loopback `http`). Required with Entra; the redirect URI is derived from it. |
| `PMOS_WEB_ENTRA_AUTHORITY` | Optional, default `https://login.microsoftonline.com`. For tests (a mock identity provider) or a sovereign cloud. An origin; `https` unless loopback. |
| `PMOS_TOOLSERVER_URL`, `PMOS_WEB_TOOLSERVER_TOKEN` | The tool server and `pmos-web`'s own token there (`PMOS_WEB_TOKEN`): the user database is asked with it. |

The tool server's host commands use `PMOS_ENTRA_TENANT_ID` (the same tenant) for the links the
host sets.

Start-up rules:

- Entra is on when the three `PMOS_WEB_ENTRA_*` variables above and `PMOS_WEB_PUBLIC_URL` are
  all set. Setting only some of them (or only `PMOS_WEB_ENTRA_AUTHORITY`) refuses to start and
  names what is missing: a half-configured provider never runs.
- The server starts if Entra is fully configured, `PMOS_WEB_DEV_LOGIN=1` or
  `PMOS_WEB_PASSWORD_LOGIN=1` (password sign-in with an authenticator code,
  `docs/password-sign-in.md`, which may run beside Entra), and refuses with none of them. **With both it refuses to start**, unless `PMOS_WEB_DEV_LOGIN_WITH_ENTRA=1` is also
  set: development sign-in lets anyone who reaches the port sign in as anyone, bypassing
  Entra, so running both has to be meant (a developer's own machine). With the override it
  starts and logs a loud warning. Development sign-in also refuses to start unless
  `PMOS_WEB_BIND` is a loopback address. It resolves people in the user database **by
  handle** (closed test networks only).
- The start-up log names the tenant, client id and redirect URI (never the secret).

## What Microsoft says about the claims

Checked on 2026-11-17 against Microsoft's own documentation, before the claim rules were
built (reviewer build note 3). `learn.microsoft.com` couldn't be reached from the build
session, so the pages were read from their published source in Microsoft's documentation
repository (`MicrosoftDocs/entra-docs`, `docs/identity-platform/`), which is what Learn
renders; the dates are each page's own `ms.date`.

- [ID token claims reference](https://learn.microsoft.com/en-us/entra/identity-platform/id-token-claims-reference)
  (`ms.date` 2023-05-30):
  - **`oid`**: "The immutable identifier for an object, in this case, a user account. … If a
    single user exists in multiple tenants, the user contains a different object ID in each
    tenant … The `oid` claim is a GUID and can't be reused." And: "To correctly store
    information per-user, use `sub` or `oid` alone … If you need to share data across
    services, `oid` and `tid` is best". **PM OS keys on `(tid, oid)`.**
  - **`tid`**: the immutable tenant id. Checked against the configured tenant.
  - **`preferred_username`**: "Its value is mutable and might change over time. Since it's
    mutable, this value can't be used to make authorization decisions. It can be used for
    username hints and in human-readable UI".
  - **`email`**: "Present by default for guest accounts … This value isn't guaranteed to be
    correct and is mutable over time. Never use it for authorization or to save data for a
    user." **PM OS never reads it** (and doesn't ask for the `email` scope).
  - **`idp`**: "Records the identity provider that authenticated the subject of the token.
    This value is identical to the value of the issuer claim unless the user account isn't in
    the same tenant as the issuer - guests, for instance. If the claim isn't present, it means
    that the value of `iss` can be used instead. For personal accounts being used in an
    organizational context (for instance, a personal account invited to a tenant), the `idp`
    claim may be 'live.com' or an STS URI containing the Microsoft account tenant
    `9188040d-6c67-4c5b-b112-36a304b66dad`."
  - Its general rule: "Your application mustn't use human-readable data to identify a user."
- [Optional claims reference](https://learn.microsoft.com/en-us/entra/identity-platform/optional-claims-reference)
  (`ms.date` 2026-07-22):
  - **`acct`**: "Users account status in tenant … If the user is a member of the tenant, the
    value is `0`. If they're a guest, the value is `1`." An optional claim, so it is there
    only when the app registration adds it.
  - **`email`** again: "never use it for authorization or to save data for a user"; and
    "Never use `email` or `upn` claim values to store or determine whether the user … should
    have access to data."
  - **`xms_edov`** says whether an email's domain owner was verified; PM OS doesn't use
    `email`, so it doesn't need it.
- [Secure applications and APIs by validating claims](https://learn.microsoft.com/en-us/entra/identity-platform/claims-validation)
  (`ms.date` 2025-03-21): "Always check that the `tid` in a token matches the tenant ID used
  to store data"; "use the immutable claim values `tid` and `oid` as a combined key for
  application data and determining whether a user should be granted access"; and "Never use
  claims like `email`, `preferred_username` or `unique_name` to store or determine whether the
  user … should have access to data. These claims aren't unique and can be controllable by
  tenant administrators or sometimes users".

**What an ordinary member's token carries for `idp`.** Absent, or equal to the issuer: the
ID token reference says the value "is identical to the value of the issuer claim unless the
user account isn't in the same tenant as the issuer", and that when it is absent "the value of
`iss` can be used instead". So the rule can't refuse an ordinary member: PM OS accepts a
token with no `idp`, with `idp` equal to the token's own `iss` (the v2.0 issuer
`https://login.microsoftonline.com/<tid>/v2.0`), or with `idp` equal to this tenant's v1
issuer `https://sts.windows.net/<tid>/` (the same tenant, in the form Entra uses in v1 tokens;
accepted so a member is never refused for the format). Anything else (`live.com`, the
Microsoft account tenant, another tenant's issuer) is someone who signed in through another
identity provider: `not_a_member`. **Not verified against a real tenant**: no Entra tenant is
available to the build; a pilot sign-in should confirm what the tenant's tokens carry.

**Where PM OS still uses `preferred_username`, and why that is acceptable.** Only to find the
person for their **first** link, as the spec decided (§7), with the limits that make it safe
enough: the name must be one an admin entered or confirmed (web invite, web confirmation, or
the reviewed import), never one the Slack bot client alone supplied; members only (`acct`);
never for an Admin or Auditor (the host sets their key); never when the person already has a
link (a conflict is refused, audited and flagged); and from then on only `(tid, oid)` counts.
The residual risk is a sign-in name reassigned in the tenant before its owner's first sign-in;
the answer is the conflict flag on the admin overview and an admin's unlink.

## How a sign-in is decided

The ID token checks come first (below, "The flow"). Then, **before any lookup**:

| Token | Answer |
| --- | --- |
| No `oid`, or one that isn't a GUID | `reason=failed` |
| No `acct` claim | `reason=entra_misconfigured` (fails closed: add the optional claim) |
| `acct` anything but `0` (a guest is `1`) | `reason=not_a_member` |
| `idp` present and not this tenant (see above) | `reason=not_a_member` |

Then the user database (`docs/user-database-spec.md` §7):

1. **Known identity:** `resolve?by=entra&tid=&oid=`. Found: signed in if the user is active
   (`reason=deactivated` otherwise) and the link is usable: an Admin's or Auditor's link must
   have been set or confirmed on the host (`reason=entra_needs_host` otherwise). The names in
   the token are ignored from here on. If the token's `preferred_username` is **another
   user's** sign-in name, the sign-in still goes ahead (the `oid` says who this is) and
   `pmos-web` records `users.identity_conflict` naming that other user, which the admin
   overview counts.
2. **First link:** nobody has that `(tid, oid)`. `resolve?by=sign_in_name` with the token's
   `preferred_username` only, **never `email`**; no such user is `reason=not_a_member`. Found:
   `POST /internal/users/{handle}/identities/entra {tid, oid, sign_in_name}`, where the tool
   server re-checks everything in one transaction and audits the outcome. Its refusals:
   `not_active` → `reason=deactivated`; `sign_in_unconfirmed` (a Slack joiner whose name no
   admin has confirmed) → `reason=sign_in_unconfirmed`; `host_only` (an Admin or Auditor) →
   `reason=entra_needs_host`; `linked` or `taken` (the name's user already has a different
   Microsoft account, or the account is someone else's) → `reason=not_a_member`, audited by
   the tool server as `users.identity_conflict` and flagged on the overview; anything else →
   `reason=not_a_member`.
3. **The tool server unreachable** (or over its miss limit): `reason=users_unavailable`. There
   is no fallback to a file or to an old answer at sign-in.

Every refusal is recorded at the tool server (`web.sign_in_refused` with the reason; for
`not_a_member`, a keyed hash of the name tried, never the name). No `oid`, sign-in name or
address is logged or recorded anywhere by `pmos-web`.

**Admins and Auditors** are linked on the host: `pmos-admin users set-entra <handle> --oid
<object id>` (or `--oid` on `bootstrap-admin` and `grant-admin`; `docs/runbooks/users.md`).
Granting Auditor (in web) to someone whose link was automatic marks it `needs_host`: their
session ends (`403 entra_needs_host`) within 10 seconds, until the host confirms the same
`oid`.

### Sessions

- An Entra session records the token's `oid` and `tid` (never shown to the browser or
  logged). Every request looks the person up by that key (cached 10 seconds): unlinked, or
  now linked to someone else, the session ends (`401 signed_out`); deactivated, every session
  of theirs ends (`403 not_a_member`); an Admin's or Auditor's link no longer host-confirmed,
  it ends (`403 entra_needs_host`). Roles are read per request, never stored in the session.
- **The tool server unreachable:** only `GET /api/me` and `GET /api/brief` keep answering, on
  the last-known-good answer, for at most 5 minutes, with Slack items withheld; everything
  else is `503 users_unavailable` (`docs/web-api.md`, "Sign-in and safety").
- A session from before slice U4c (an Entra session with no `oid`) ends on its next request:
  sign in again.

## The flow

1. The app links "Sign in with Microsoft" to `GET /api/auth/login` (optionally
   `?return_to=/#/somewhere`; only a same-origin path is kept).
2. `pmos-web` fetches the tenant's discovery document
   (`<authority>/<tenant>/v2.0/.well-known/openid-configuration`, cached a day; its `issuer`
   must be exactly `<authority>/<tenant>/v2.0` and every endpoint on the authority's origin),
   makes a random `state`, `nonce` and PKCE verifier, stores them with the return path in a
   signed `__Host-pmos-entra` cookie (`Secure; HttpOnly; SameSite=Lax`, ten minutes), and
   redirects to the authorize endpoint with the S256 challenge and `scope=openid profile`.
3. Microsoft redirects back to `GET /api/auth/callback?code&state`. `pmos-web` checks the
   cookie's signature and age, compares `state` in constant time and accepts it once, then
   exchanges the code with the client secret and the verifier.
4. The ID token must be RS256 (anything else, `none` and `HS256` included, is refused) and
   signed by a key in the tenant's JWKS (from the discovery document's `jwks_uri`, cached a
   day, refetched on an unknown `kid` at most once a minute). `iss` must be the exact v2.0
   issuer, `aud` the client id (a list must be exactly that one), `tid` the tenant, `nonce` the
   flow's, and `exp`, `nbf` and `iat` within 60 seconds of the server's clock. Then the claim
   rules above.
5. On success the browser gets the same session as development sign-in creates and is
   redirected to the return path (default `/`). On any failure it is redirected to
   `/#/signed-out?reason=<not_a_member|deactivated|sign_in_unconfirmed|entra_needs_host|entra_misconfigured|users_unavailable|cancelled|expired|unavailable|failed>`
   with no session. The flow cookie is always cleared.

**Logout** (`POST /api/auth/logout`) ends the PM OS session only. Microsoft's own session is
untouched, so signing in again usually goes straight through without a prompt; to sign out of
Microsoft too, the person signs out at Microsoft.

## Not built

- **Roles from Entra.** Group claims and app roles are ignored; roles come only from the user
  database (spec §7: a sync would let tenant admins grant PM OS admin outside its audit log).
- **"You're signed in with Microsoft for the first time"** on the first page after a first
  link (spec §7 step 2): the link is logged and audited, but the app doesn't say it yet.
- **Conditional access awareness.** Whatever Entra's policies require happens at Microsoft;
  `pmos-web` doesn't read `amr`, `acr` or device claims, and a session lasts its own idle and
  absolute limits regardless of Entra's sign-in frequency.
- **Single logout.** No front-channel or back-channel logout: signing out of Microsoft does not
  end a PM OS session, and logging out of PM OS does not sign out of Microsoft.
- **Token refresh.** Only the ID token is used, once, at sign-in. No access or refresh token is
  kept, and nothing calls Microsoft after sign-in.
- **A certificate credential.** Only a client secret; a certificate or federated credential is
  not supported.
- **Shared single-use `state` across servers.** The used-`state` list is in each process. Two
  `pmos-web` processes with the same configuration accept each other's flow cookies (its key is
  derived from the client secret), so a callback replayed to the other process would pass the
  `state` check; Entra still refuses the second use of the code.
- **The fallback when Entra is down.** Sign-in answers `reason=unavailable`; existing sessions
  keep working. Not decided beyond that (`docs/web-client.md`).

## In the deploy kit

`deploy/compose.yaml` passes the four `PMOS_WEB_ENTRA_*` variables from `deploy/.env` (blank by
default, so off) to the web app, with the Spark's outbound proxy. `deploy/pmos-admin up --web`
starts the web app when the tenant, client id, secret and `PMOS_WEB_PUBLIC_URL` are all set, or
with the development opt-in (`docs/web-app-running.md`). `deploy/setup.sh` doesn't make the
secret: it comes from the app registration, and goes in `deploy/.env` and the team password
manager.

## Tests

`crates/web/tests/entra.rs` runs the whole flow against a mock identity provider (discovery,
JWKS and a token endpoint that checks the client secret and PKCE) with RSA keys generated for
the tests only, and a fake of the tool server's user database routes with the real first-link
rules (`crates/web/tests/support/mod.rs`): the happy path and the first link; wrong or reused
`state`, an expired flow, a tampered flow cookie, a code for another PKCE challenge; every ID
token check (nonce, audience, issuer, tenant, expiry, `alg: none`, HS256, an unknown `kid`, a
tampered payload, no `oid`); guests, other identity providers and a missing `acct` (refused
before any lookup), and a member's `idp` accepted in both forms; no fallback to `email`; an
unconfirmed sign-in name; Admin and Auditor without a host link; a conflict refused, audited
and flagged, and a known account presenting another user's name flagged; a reassigned name
and an admin's unlink; a deactivated user at sign-in and on a live session; the tool server
unreachable (`users_unavailable`, then the 5-minute last-known-good limit); someone only in a
leftover `team.yaml`; the start-up rules; and that no secret, code, token, verifier, `oid` or sign-in
name is ever logged. `crates/stack-tests/tests/stack.rs` signs in against the real tool
server's user database and shows a deactivation there ending the web session within 10
seconds, and (slice U7) that a hostile `team.yaml` planted in the content changes no sign-in.
**Not tested against a real Entra tenant** (none is available yet).
