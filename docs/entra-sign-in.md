# Entra ID sign-in for the web app

`pmos-web` can sign people in with Microsoft Entra ID (Azure AD): the OpenID Connect
authorization code flow with PKCE, as a confidential client of **one** tenant. It is off unless
configured, and it sits beside development sign-in behind the same provider interface
(`crates/web/src/entra.rs`). The API is in `docs/web-api.md` ("Sign-in and safety"); the
backend's checks are in `crates/web/README.md` ("How a request is checked").

Entra proves who the browser is. **Who may use PM OS, and with which roles, still comes only
from `content/team.yaml`.**

## What to register in Entra

In the Microsoft Entra admin center, **App registrations → New registration**:

| Setting | Value |
| --- | --- |
| Supported account types | **Accounts in this organizational directory only** (single tenant). |
| Redirect URI | Platform **Web**, `<PMOS_WEB_PUBLIC_URL>/api/auth/callback`, for example `https://pmos.example.com/api/auth/callback`. Exactly this; `https` (Entra allows `http` only for `localhost`). |
| Certificates & secrets | One **client secret**. Put it in the secret store, never in a file in git. Note its expiry: sign-in stops working when it lapses. |
| Authentication | Implicit grant and hybrid flows: leave **both unticked** (ID tokens come from the code exchange, not the front channel). Front-channel logout URL: none. |
| API permissions | **No extra permissions.** The default delegated `User.Read` may stay or go; `pmos-web` asks only for `openid profile email` and never calls Microsoft Graph. |
| Token configuration | Nothing required. If `preferred_username` isn't the person's email in your tenant, add the optional `email` claim to the ID token. No group or role claims are used. |

Then note the **Directory (tenant) ID** and the **Application (client) ID** from the overview.

## Configuration

| Variable | What it is |
| --- | --- |
| `PMOS_WEB_ENTRA_TENANT_ID` | The directory (tenant) id, a GUID. `common`, `organizations` and `consumers` are refused: they would let any tenant's accounts in. |
| `PMOS_WEB_ENTRA_CLIENT_ID` | The application (client) id, a GUID. |
| `PMOS_WEB_ENTRA_CLIENT_SECRET` | The client secret. Never logged, never in an error message. |
| `PMOS_WEB_PUBLIC_URL` | The origin browsers use, `https` (or loopback `http`). Required with Entra; the redirect URI is derived from it. |
| `PMOS_WEB_ENTRA_AUTHORITY` | Optional, default `https://login.microsoftonline.com`. For tests (a mock identity provider) or a sovereign cloud. An origin; `https` unless loopback. |

Start-up rules:

- Entra is on when the three `PMOS_WEB_ENTRA_*` variables above and `PMOS_WEB_PUBLIC_URL` are
  all set. Setting only some of them (or only `PMOS_WEB_ENTRA_AUTHORITY`) refuses to start and
  names what is missing: a half-configured provider never runs.
- The server starts if Entra is fully configured **or** `PMOS_WEB_DEV_LOGIN=1`, and refuses with
  neither. **With both it refuses to start**, unless `PMOS_WEB_DEV_LOGIN_WITH_ENTRA=1` is also
  set: development sign-in lets anyone who reaches the port sign in as anyone, bypassing
  Entra, so running both has to be meant (a developer's own machine). With the override it
  starts and logs a loud warning. Development sign-in also refuses to start unless
  `PMOS_WEB_BIND` is a loopback address.
- The start-up log names the tenant, client id and redirect URI (never the secret).

## People in `team.yaml`

A member is found by the ID token's `preferred_username`, or else its `email` claim,
lower-cased, against members' `email` field (case-insensitive). Never by handle.

```yaml
members:
  - handle: ana
    slack-id: U012AB3CD
    email: ana@contoso.com   # Atlassian account email, and the Entra sign-in name
    roles: [pm]
```

- The same `email` field also links Atlassian, so a PM's Entra sign-in name and Atlassian
  email must be the same address. If they differ in your organization, this needs a separate
  field (not built).
- `pmos lint` refuses two members with the same email (case-insensitive) as `team-duplicate`;
  if it happens anyway, Entra sign-in matches neither.
- Someone Entra signs in who is not on the list lands on `/#/signed-out?reason=not_a_member`
  with no session. A platform owner adds them the usual way (onboarding or a pull request).
- Removing someone from `team.yaml` ends their sessions on their next request after the file
  changes (at most 5 seconds later), as with any provider.

### Matching by object id

An email address can be reassigned: someone leaves, and their address is given to someone
else in the tenant. Matching by email alone would then sign the new person in as the member.
To prevent that for a member, give them their Entra **object id** (`oid`, a GUID: Entra admin
center → Users → the person → Object ID):

```yaml
members:
  - handle: ana
    slack-id: U012AB3CD
    email: ana@contoso.com
    entra-oid: 3f2504e0-4f89-41d3-9a0c-0305e82c3301
    roles: [pm]
```

- Every Entra sign-in records the ID token's `oid` and `tid` on the session (never shown to
  the browser or logged).
- A member with `entra-oid` signs in only with a token whose `oid` is that one (any case).
  Another account with the same email, or a token without `oid`, lands on
  `/#/signed-out?reason=wrong_account` with no session, and the refusal is logged with the
  handle (not the ids).
- If `entra-oid` is added or changed while the member has a session from another account,
  that session ends (`401 signed_out`) on its next request.
- A member without `entra-oid` is matched by email alone, as before. Development sign-in
  doesn't check it.
- `pmos lint` refuses an `entra-oid` that isn't a GUID (`team-entra-oid`) and the same one on
  two members (`team-duplicate`).

## The flow

1. The app links "Sign in with Microsoft" to `GET /api/auth/login` (optionally
   `?return_to=/#/somewhere`; only a same-origin path is kept).
2. `pmos-web` fetches the tenant's discovery document
   (`<authority>/<tenant>/v2.0/.well-known/openid-configuration`, cached a day; its `issuer`
   must be exactly `<authority>/<tenant>/v2.0` and every endpoint on the authority's origin),
   makes a random `state`, `nonce` and PKCE verifier, stores them with the return path in a
   signed `__Host-pmos-entra` cookie (`Secure; HttpOnly; SameSite=Lax`, ten minutes), and
   redirects to the authorize endpoint with the S256 challenge.
3. Microsoft redirects back to `GET /api/auth/callback?code&state`. `pmos-web` checks the
   cookie's signature and age, compares `state` in constant time and accepts it once, then
   exchanges the code with the client secret and the verifier.
4. The ID token must be RS256 (anything else, `none` and `HS256` included, is refused) and
   signed by a key in the tenant's JWKS (from the discovery document's `jwks_uri`, cached a
   day, refetched on an unknown `kid` at most once a minute). `iss` must be the exact v2.0
   issuer, `aud` the client id (a list must be exactly that one), `tid` the tenant, `nonce` the
   flow's, and `exp`, `nbf` and `iat` within 60 seconds of the server's clock.
5. On success the browser gets the same session as development sign-in creates and is
   redirected to the return path (default `/`). On any failure it is redirected to
   `/#/signed-out?reason=<not_a_member|wrong_account|cancelled|expired|unavailable|failed>`
   with no session.
   The flow cookie is always cleared.

**Logout** (`POST /api/auth/logout`) ends the PM OS session only. Microsoft's own session is
untouched, so signing in again usually goes straight through without a prompt; to sign out of
Microsoft too, the person signs out at Microsoft.

## Not built

- **Roles from Entra.** Group claims and app roles are ignored; roles come only from
  `team.yaml`. Group-to-role mapping is not decided (`docs/web-client.md`).
- **Matching by `oid` alone.** People are matched by email, which an admin can rename;
  `oid` is checked only for members whose `team.yaml` entry has `entra-oid`, which a platform
  owner adds by hand. Filling it in automatically at a member's first sign-in, or matching by
  `oid` alone, waits for the user database.
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
the tests only: the happy path, wrong or reused `state`, an expired flow, a tampered flow
cookie, a code for another PKCE challenge, every ID token check (nonce, audience, issuer,
tenant, expiry, `alg: none`, HS256, an unknown `kid`, a tampered payload), key rotation,
non-members, matching by object id (`entra-oid`), the start-up rules (development sign-in
beside Entra only with the override, and only on loopback), and that no secret, code, token or
verifier is ever logged.
