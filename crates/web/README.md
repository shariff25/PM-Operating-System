# `pmos-web`: the web app's backend

`pmos-web` holds the browser session and serves `/api` for the web app (`web/`), exactly as
`docs/web-api.md` describes. It calls the bot runtime and the tool server for the signed-in
PM. The browser never sees a service token, a connector token or another PM's data.

```
browser ──HTTPS──► pmos-web :8770 ──► pmos-bots :8750        (client "web", X-Pmos-User + X-Pmos-Assertion)
                               └──► pmos-toolserver :8710  (approvals and admin reads: web token + X-Pmos-Assertion;
                               │                            users: GET /internal/users/resolve, cached 10 s)
                               └──► content/               (the admin content and policy screens)
pmos-slack ───────► pmos-web :8770, or :8771 with PMOS_WEB_INTERNAL_BIND  (PUT /internal/brief/{handle})
```

## Running it

```sh
PMOS_WEB_DEV_LOGIN=1 \
PMOS_WEB_BOTS_TOKEN=<the runtime's "web" client token> \
PMOS_WEB_TOOLSERVER_TOKEN=<the tool server's PMOS_WEB_TOKEN> \
PMOS_ASSERTION_KEYS=web:bots:<kid>:<base64>,web:toolserver:<kid>:<base64> \
PMOS_WEB_STATIC_DIR=web/dist \
cargo run -p pmos-web
```

For Entra ID sign-in instead of `PMOS_WEB_DEV_LOGIN=1`, set `PMOS_WEB_ENTRA_TENANT_ID`,
`PMOS_WEB_ENTRA_CLIENT_ID`, `PMOS_WEB_ENTRA_CLIENT_SECRET` and `PMOS_WEB_PUBLIC_URL`
(`docs/entra-sign-in.md` says what to register in Entra).

| Variable | Default | Notes |
| --- | --- | --- |
| `PMOS_WEB_BIND` | `127.0.0.1:8770` | Must be a loopback address (`127.0.0.1`, `::1`) while development sign-in is on |
| `PMOS_WEB_INTERNAL_BIND` | none | A second listener for the server-to-server route `PUT /internal/brief/{handle}` (and `/healthz`, `/readyz`); the browser-facing `PMOS_WEB_BIND` then doesn't serve that route. Without it the route is on `PMOS_WEB_BIND`, refusing any call that carries a session cookie or an `Origin`. Must differ from `PMOS_WEB_BIND` |
| `PMOS_WEB_DEV_LOGIN` | off | `1` turns on development sign-in, only on a loopback `PMOS_WEB_BIND`. The server starts with Entra ID or development sign-in; with neither it refuses to start |
| `PMOS_WEB_DEV_LOGIN_WITH_ENTRA` | off | `1` lets development sign-in run beside Entra ID (a loud warning). Without it, both on refuses to start: development sign-in bypasses Entra. For a developer's own machine only |
| `PMOS_WEB_ENTRA_TENANT_ID` | none | Entra ID sign-in (`docs/entra-sign-in.md`): the directory (tenant) id, a GUID. `common`, `organizations` and `consumers` are refused |
| `PMOS_WEB_ENTRA_CLIENT_ID` | none | The application (client) id, a GUID |
| `PMOS_WEB_ENTRA_CLIENT_SECRET` | none | The client secret, from the secret store. Never logged or shown in errors. Entra is on only with all three and `PMOS_WEB_PUBLIC_URL`; any of them alone refuses to start |
| `PMOS_WEB_ENTRA_AUTHORITY` | `https://login.microsoftonline.com` | For tests (a mock identity provider) and sovereign clouds: an origin, `https` unless loopback |
| `PMOS_BOTS_URL` | `http://127.0.0.1:8750` | |
| `PMOS_WEB_BOTS_TOKEN` | required | The `web` entry of the runtime's `PMOS_BOTS_TOKENS`. At least 32 characters |
| `PMOS_TOOLSERVER_URL` | `http://127.0.0.1:8710` | |
| `PMOS_WEB_TOOLSERVER_TOKEN` | required | The tool server's `PMOS_WEB_TOKEN`, which reaches only the approvals routes and the read-only admin routes (older deployments used `PMOS_SERVICE_TOKEN`). At least 32 characters, different from the runtime token |
| `PMOS_ASSERTION_KEYS` | none | The keys pmos-web signs user assertions with: `web:bots:<kid>:<base64>` and `web:toolserver:<kid>:<base64>`, each optionally followed by `:<kid>:<base64>` for the previous key (`docs/user-assertion.md`, Keys). Without a key for an upstream, its calls carry no assertion. A malformed entry, a key under 32 bytes, or another service's pair refuses to start |
| `PMOS_CONTENT_ROOT` | `content` | The deployed content tree the admin content and policy screens read. Who may sign in, their roles and the Team page's list come from the user database at the tool server; there is no team file (slice U7) |
| `PMOS_WEB_STATIC_DIR` | none | The built web app. Must contain `index.html`, served for any unknown non-`/api` path that isn't an asset; the icon is `favicon.svg` (or `favicon.ico`) here |
| `PMOS_WEB_PUBLIC_URL` | none | The origin browsers use. Writes must come from it; without it, `Origin` must match `Host`. Required with Entra ID (`https` unless loopback): the redirect URI is `<PMOS_WEB_PUBLIC_URL>/api/auth/callback`. When it is `https`, every response carries `Strict-Transport-Security: max-age=31536000` |
| `PMOS_WEB_SESSION_IDLE_MINUTES` | `480` | Idle expiry |
| `PMOS_WEB_SESSION_MAX_MINUTES` | `1440` | Absolute expiry from sign-in |
| `PMOS_WEB_DATABASE_URL` | none | Postgres for sessions, settings and follow-ups (`src/store_config.rs`). Without it everything is in memory and a start-up warning says a restart signs everyone out |
| `PMOS_WEB_BRIEF` | off | `1` turns on `GET /api/brief` and the follow-up routes; off, they answer `501 not_built` and the app hides the section |
| `PMOS_WEB_BRIEF_TOKEN` | none | The bearer token `PUT /internal/brief/{handle}` needs, at least 32 characters. Without it that route answers `501` |
| `PMOS_WEB_CONTENT_DIR`, `PMOS_WEB_POLICIES_DIR` | `content`, `policies` | Where the content tree and the generated policies are in the repository, for the paths the admin screens show and send. Neither may be the repository root (set but empty refuses to start) nor inside the other; they match the tool server's `PMOS_CONTENT_DIR` and `PMOS_POLICIES_DIR` |
| `PMOS_WEB_POLICIES_ROOT` | `policies` beside the content root, if there | The checked-in policy files the Policy Admin compares with (drift) |
| `PMOS_WEB_GATEWAY_CONFIG` | none | The gateway's model config (`deploy/gateway.yaml`), read for model and upstream names only, for `GET /api/admin/harness` |
| `PMOS_WEB_CONTENT_REVISION` | none | The deployed content's commit, when the content root has no `.git` to read it from |

**Removed:** `PMOS_WEB_GITHUB_TOKEN`, `PMOS_WEB_GITHUB_REPO`, `PMOS_WEB_GITHUB_API`,
`PMOS_WEB_GITHUB_BASE`, `PMOS_WEB_PROPOSALS_PER_HOUR`, `PMOS_WEB_GITHUB_CONTENT_DIR` and
`PMOS_WEB_GITHUB_POLICIES_DIR`. Any of them set refuses to start, naming where it went:
pmos-web holds no GitHub token, and the tool server opens proposals with the admin's own linked
GitHub account (`docs/policy-admin.md`, **Configuration**).

`GET /healthz` answers `ok` with no sign-in while the process runs. `GET /readyz` answers `200`
or `503` with each check `ok` or `down` (its store, and the runtime's and the tool server's own
`/healthz` within two seconds); the reason goes to the log, never the answer.
Every ten minutes (`app::HOUSEKEEPING_EVERY`, and at each sign-in) expired sessions,
follow-ups, `changed` items and old edits and suggestions are deleted, whether or not anyone
signs in. SIGTERM or Ctrl-C stops that task and lets in-flight requests finish on both
listeners.

The `__Host-pmos` cookie is `Secure`, so browsers only keep it over HTTPS (or on
`http://localhost` in most browsers). Put a TLS proxy in front for anything else.

## How a request is checked

1. **Session:** the `__Host-pmos` cookie (`Secure; HttpOnly; SameSite=Lax; Path=/`, no
   `Domain`). The id is 256 random bits from the OS; the store keeps only its SHA-256. A
   session ends after 8 idle hours or 24 hours from sign-in, at logout, or when the person
   leaves the team. Signing in always makes a new id and ends the browser's old one.
2. **Membership:** the session's person must be an **active** user in the user database,
   asked at the tool server (`GET /internal/users/resolve`, through `pmos_directory::resolve`)
   and cached 10 seconds per person: someone deactivated or removed loses access within 10
   seconds (`403 not_a_member` once, and every session they have ends), and a removed role is
   gone within 10 seconds. Roles are looked up per request; a session holds none. An Entra
   session is looked up by its `(tid, oid)`: unlinked or re-linked, it ends (`401
   signed_out`); an Admin's or Auditor's link the host hasn't confirmed ends it with `403
   entra_needs_host`. **The tool server unreachable:** `GET /api/me` and `GET /api/brief`
   (pmos-web's own store) may use the person's last-known-good answer for at most 5 minutes
   (`app::LAST_KNOWN_GOOD`), the brief with every Slack item withheld; every other route and
   every write is `503 users_unavailable` (`app::SignedRead` vs `app::Signed`). There is no
   team file (`team.yaml` was retired in slice U7).
3. **Writes** (`POST`, `PUT`, `DELETE`): `Sec-Fetch-Site` must not be another site, `Origin`
   (or else `Referer`) must be the app, **and one of them must be there** (`403 bad_origin`;
   browsers always send `Origin` on these), and `X-Pmos-Csrf` must equal the session's own
   random token, compared in constant time (`403 bad_csrf`).
4. **Roles,** from the permission matrix (`pmos_contract::permissions`): drafts need
   `decide-own-approval`, asking needs `ask-bot`, reading tasks `view-own-tasks`. An
   auditor-only member gets `403 not_allowed` on every draft and task route.
5. **Ownership:** a draft is the PM's when the approval item's `user` is their handle; a
   task when its `requested_by` is. Anything else is `404 not_found`, checked here before
   any decision is sent, because the tokens this backend holds could act for anyone.

6. **JSON only:** a `POST`, `PUT` or `DELETE` with a body must be `Content-Type:
   application/json` (`415 bad_content_type`), and development sign-in always must, so a
   cross-site form can't reach a route or sign a victim in as someone else (login CSRF). A
   write with no body (logout, `DELETE /api/me/hints`) needs no type; it still needs the CSRF
   token.
7. **Entra ID sign-in** (`src/entra.rs`, `docs/entra-sign-in.md`) is two `GET`s, so rules 3
   and 6 don't apply to it. `/api/auth/login` sets a signed `__Host-pmos-entra` cookie
   (HMAC-SHA256 under a key derived from the client secret; `SameSite=Lax` so Microsoft's
   top-level redirect back carries it; ten minutes) holding `state`, `nonce`, the PKCE
   verifier and the return path. `/api/auth/callback` requires that cookie and its `state`,
   compared in constant time and accepted once (a used `state` is remembered until its flow
   expires), so a forged or replayed callback, and login CSRF with an attacker's own code,
   fail. The code goes to Microsoft with the client secret and the verifier; the ID token must
   be RS256, signed by a key from the tenant's JWKS (fetched from the discovery document,
   cached a day, refetched on an unknown `kid` at most once a minute), with `iss` exactly
   `<authority>/<tenant>/v2.0`, `aud` the client id, `tid` the tenant, the flow's `nonce`, and
   `exp`, `nbf` and `iat` within 60 seconds. Every redirect after it is a same-origin path.
   Tokens, codes, verifiers, `state`, `nonce` and the client secret are never logged; a
   refused sign-in logs a fixed reason and, for a refused code, only Microsoft's error code.

Every response carries `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`,
`Referrer-Policy: no-referrer` and `Cross-Origin-Opener-Policy: same-origin`. Every `/api`
answer, errors included, is `Cache-Control: no-store` with a CSP that loads nothing. The
app's files get `default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self';
font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self';
frame-ancestors 'none'`: no inline script or style, no `data:` or remote images.
`index.html` is `no-cache`; `favicon.svg` and `favicon.ico` (the icon must be a file: the
policy allows no `data:` image) are `public, max-age=604800`; other files five minutes. A
missing file with an asset extension (`.svg`, `.js`, `.css`, …) is a `404`, not
`index.html`. Under `/api`, an unknown path is always a JSON `404 not_found` and a known
path with another method a JSON `405 method_not_allowed`.

## How the contract maps upstream

- **Drafts** are the PM's tool server approval items (`GET /internal/approvals?user=`).
  `kind`, `title`, `body` and `target.label` come from the write tool's arguments (the table
  in `src/drafts.rs`); the label uses only id-shaped values, and anything else reads "an
  unrecognised channel" (or space, page, project, epic), so it is never text the bot wrote.
  Where the run read the target (a Confluence parent or updated page, a Jira epic), the label
  names it with the title the tool server recorded at the read (`drafts::with_sources`). A
  Slack label never shows a channel id: PM OS keeps no channel names, so it reads "A Slack
  thread", "A direct message" and so on, and `target.url` opens the channel in Slack. Slack
  sources' titles lose their channel id the same way ("Slack thread").
- **What the approve dialog shows** (`restricted_sources`, `audience`, `marker`): from the
  item's `sources_sensitivity` (missing counts as restricted, as at the tool server), the
  `members` and `external` of the tool server's last `needs_external_confirm` or
  `audience_blocked` refusal for that draft (kept in memory here, as the tool server keeps
  `external`), and the marker the tool server's connectors append ("Drafted with PM OS", with
  "· run <run id>" for Jira and Confluence).
  `target.url`, `expires_at` and `external` are the item's own `target_url`, `expires_at`
  and `external` from the tool server (`target.url` only if `https`; `expires_at` falls back
  to 24 hours for a tool server that doesn't send it; `external` to the last
  `needs_external_confirm` for this draft). Items of other write tools (`bets.record`) are
  not drafts and are left out. `executing` reads as `approved` with no `result_url` yet;
  `result_url` is the `url` of the item's `result`.
- **Sources** of every single-draft answer come from the tool server's
  `GET /internal/approvals/{id}/sources?user=<handle>`, called only after the ownership
  check. Each becomes `{id: "s1"…, system, title, url, excerpt: null}`: `url` only if
  `https`, else `null` (context files have none; Jira and Confluence have none without a
  linked Atlassian site); a title of at most 200 characters; at most 20. The title is the
  one the tool server recorded at the read (`PAY-212: Grace period`, a page title), or the
  id for Slack and for reads before titles were recorded. `excerpt` is always `null`:
  nothing keeps source text to quote. If the call fails the draft still loads with
  `sources: []`.
- **Body repair, for display only.** A body with no real line break at all that contains
  the two characters `\n` (a model that wrote its line breaks as JSON escapes: "…launch.\n\nSource:
  PM-101") is shown with `\n` as a line break and `\t` as a tab (`drafts::display_body`).
  What an approve sends is never changed by this: the `version` and an approve without an
  edit use the item's own arguments, as the bot drafted them and as the tool server hashed
  them. The trade-off: an unedited approve publishes the escapes the PM didn't see. We
  accept that rather than send text that differs from the stored arguments without the
  PM's edit; the app can offer "Save" (any `PUT`), after which the text the PM saw is the
  text that is sent. A body with any real line break is left alone, since there `\n` is
  more likely meant (a path, a regex, code).
- **Task progress** (`src/progress.rs`): `events` are the runtime's `milestone` events only,
  in order, each text once; `status` events (which name workers and hosts) and `checklist`
  events never pass. `progress` is `{done, total}` from the task's `checklist`, else the
  latest checklist event's "N of M done", else `null`. `retrying` is true while the task is
  queued or working after a retry: `attempts` above 1, or the runtime's "interrupted by a
  restart; retrying" event. The gateway's and the harness's own retries aren't reported to
  the runtime, so they can't be shown.
- **`version`** is the SHA-256 of the tool and the canonical JSON of what an approve would
  send now: the item's arguments, which hold the PM's saved edit (for a practice draft, its
  title and body). **Approve** takes an optional `version`: a different one is
  `409 stale` and nothing is sent.
- **Edit** (`PUT`, and accepting a suggestion) is saved **on the item at the tool server**
  (`PUT /internal/approvals/{id}/draft?user=<handle>`, signed for the PM): the item's
  arguments with the new body (and title), where it goes unchanged. The tool server keeps it
  as the item's arguments, so the Slack card and the console show it, and an approve from
  any of them writes it; it refuses a change of target (`422 target_changed`, here
  `422 bad_edit`), a decided item (`409`, here `not_pending`) and an expired one (`expired`).
  Nothing of a real draft's edit is kept here any more: `web_draft_edits` holds only practice
  drafts' edits (rows saved for real drafts before this change are ignored and purged with
  the rest). Suggestions stay here. **Approve** sends `edit` with the item's arguments, the
  very ones the version was computed from, when the item is edited (so a save from another
  tab a moment later can't change what is written), and `approve` otherwise, always with
  `decided_by` = the signed-in handle. A save from another tab between this read and an
  `approve` of an unedited draft would be what is written: it is the PM's own text, and the
  window is one round trip.
- **Refused decisions** answer fixed codes and sentences; the tool server's own text is never
  passed on (it can quote the draft) and only its status and code are logged:
  `needs_external_confirm`, `audience_blocked`, `expired` (`409`), `audience_unavailable`
  (`503`); a `409` without a code is `not_pending` when the item has since been decided (in
  Slack, say), else `not_approvable`; a `400` or `422` on edited arguments is `422 bad_edit`.
  **A lost answer** (the call timed out, the connection dropped, a `5xx` other than `503`, an
  unreadable reply) reads the item again: if it shows this PM's decision, that is the answer;
  otherwise `504` (timed out) or `502` `unknown_outcome`, never "nothing was sent".
- **Approve is safe to retry.** An approve of an item already `executing` or `approved`
  whose `decided_by` is this PM, with the same version (or none), answers the approved draft
  and sends nothing. So does an approve that loses a race with the PM's own other approve
  of the same text (the tool server's plain `409`, then a re-read). Anything else already
  decided is `not_pending`. The same holds for a practice draft.
- **Ask for a change** creates a task for `pm-<handle>` (`source: "web"`) whose prompt holds
  the instruction and the current text and says to call no write tool, make no draft or
  approval, and answer with the revised text only. The task's `result` (without a code fence)
  becomes the suggestion's `body`. These tasks are hidden from `GET /api/tasks`.
- **Practice drafts** live only here (ids `prc_…`, looked up in this PM's own state and
  never at the tool server): no call of any kind goes upstream for them, their target label
  is "Practice only: nothing is sent", and asking for a change to one answers at once with
  a canned note. An upstream item with a `prc_` id is never decided.
- **Tasks:** `POST /v1/tasks {bot: "pm-<handle>", requested_by, text, source: "web"}`, and
  reads of the PM's own tasks and their events, each with `X-Pmos-User: <handle>`.
- **User assertions** (`src/upstream.rs`): with a key for the upstream, every runtime call and
  every tool-server call that names the PM (the inbox, one item, sources, decisions) also
  carries `X-Pmos-Assertion`: issuer `web`, the upstream as audience, the PM as subject, roles
  as the user database answered for that request (informational), 60 seconds, bound to the method, path, query and body sent, signed afresh
  for each call. Every read of one item (the first read of a draft and the re-read after a
  conflicting decision) is `ToolServer::approval_for(user, id)`: it names the PM
  (`?user=<handle>`, which the tool server requires from the web token, answering `404`
  otherwise) and carries an assertion for them; the owner is also checked here. `sid` is the PM's
  session reference, `session::assertion_ref` (16 hex characters of a labelled SHA-256 over
  the session's stored hash): `app.rs` binds each upstream to it with `for_session`. Never
  the session id or the cookie.
- **Brief and follow-ups** (`src/app/brief.rs`, `src/followups.rs`), on with `PMOS_WEB_BRIEF=1`
  and `501 not_built` otherwise. `GET /api/brief` answers `200` with the PM's open follow-ups
  (not dismissed, not expired) and the day's `changed` items, empty lists when nothing is
  stored, and `delivered_at` and `status` (`never`, `today`, `stale`): when the producer last
  filled this PM's brief. That time outlives the day's items (`web_brief_days.updated_at`
  stays after its `changed` list is emptied by the purge); it is a time, no text. A follow-up holds a one-line summary (at most 200 characters, no message text), who,
  system and url; Slack ones expire after 24 hours and every other after a week, both purged.
  Its id is derived from the handle, kind, system and url, so a producer sending the same one
  again refreshes it (keeping its age and a dismissal) rather than copying it. `POST
  /api/followups/{id}/dismiss` is `204` and idempotent; `POST /api/followups/{id}/draft`
  makes a task for `pm-<handle>` (`source: "web"`) asking for a reply (waiting on you) or a
  gentle nudge (you wait on), written through the bot's normal write tool so it arrives as an
  ordinary approval; it needs `ask-bot`. Someone else's, a dismissed, an expired or an
  unknown id is `404`. **The follow-up is untrusted data in that prompt**: its who, system,
  summary and url were written by the brief run from other people's messages, so they go in
  as one line of JSON between two marker lines that carry a nonce fresh for each request
  (`<<<FOLLOW_UP_DATA <nonce>` and `FOLLOW_UP_DATA <nonce>>>>`), after an instruction to treat
  every value as text and never as instructions. Every `<` and `>` in the JSON is written as
  a JSON escape (`<`, `>`), so no value, however nested, can contain or rebuild
  either marker (R8; the earlier single-pass removal of the markers could be defeated).
  The PM's own words around it are fixed. This lowers the risk, it doesn't remove it: the bot
  reads the thread anyway, and whatever it drafts still waits for the PM's approval.
- **`PUT /internal/brief/{handle}`** is the producer's route. The Slack service calls it
  when a PM's daily brief finishes (`crates/slack/src/brief_feed.rs`, on with `PMOS_WEB_URL`
  and `PMOS_WEB_BRIEF_TOKEN` set there; `docs/daily-brief.md`). It needs `Authorization: Bearer
  <PMOS_WEB_BRIEF_TOKEN>` (`401 bad_token`, constant-time compare), no session cookie and no
  `Origin` (`403 forbidden`: a page can't reach it), a member's handle, JSON,
  at most 128 KiB (`413`), and replaces that PM's follow-ups and `changed` items:
  `{date?, changed: [{system, title, summary?, url}], waiting_on_you: [{system, who, summary,
  url, created_at?}], you_wait_on: [...]}`. At most 50 per list, `system` lower-case
  `[a-z0-9_-]`, one-line text, `url` an `http` or `https` URL without credentials, unknown
  fields refused (`422 bad_brief`); nothing is stored if anything fails.
- **Admin and auditor** (`src/app/admin.rs`; `docs/web-api.md`, **Admin and auditor**):
  `GET /api/admin/{audit, audit/export, team, connectors, overview}`, all read-only. pmos-web
  checks the permission matrix first (`admin` everything; `auditor` all but connectors; else
  `403 forbidden`, with no upstream call), then calls the tool server's
  `GET /internal/admin/{audit, accounts, connectors, overview}?user=<viewer>`, signed for the
  viewer, where the role is checked again and the read is audited. The audit list and export
  re-pick each event field by field, so only metadata can reach the browser (counts of
  sources and titles, never the titles, Slack text, drafts, prompts, results or hashes). The
  export reads the tool server in pages of 1,000 (`purpose=export`, needing `export-audit`
  there too), up to 10,000 rows, with `X-Pmos-Truncated` saying when more were left out, and
  every cell quoted and formula-guarded. The team list comes from the user database (the tool
  server's `GET /internal/admin/users`, since slice U5a) and the page fails `502` when the tool
  server can't answer. Every admin read is
  logged here (handle, route, filter names only). `GET /api/me` also gives `permissions`
  (from `pmos_contract::permissions::granted`), so the app knows whether to show the Admin
  area; an older client ignores it.
- **Policy Admin and content admin** (`src/app/{policy, content, proposal}.rs`, logic shared
  with the tool server in `crates/policy/src/admin/` and re-exported as
  `src/{policy_admin, proposals}.rs`, plus `src/content_admin.rs`; `docs/web-api.md`, **Admin:
  policy** and **Admin: content and harness**; `docs/policy-admin.md`,
  `docs/content-admin.md`): read-and-propose. Reading needs `view-policies` / `view-content`
  (admins and auditors); validating and proposing need `edit-policies` / `edit-content`
  (admins), since a validation's diff shows the current text of the files it names, and every
  path named goes through the same read rule as the file route. Validation copies the
  deployed content tree to a scratch directory, applies the change, and runs the contract
  crate's lint and the policy crate's generation and rule checks there; nothing on disk
  changes. A proposal is validated again and sent to the tool server as a change set
  (`POST /internal/admin/proposals/open`, signed for the admin and bound to the body), which
  checks everything again and opens the pull request **with the admin's own linked GitHub
  account**; pmos-web holds no GitHub token and relays the answer in its own words. Role
  refusals, refused proposals and an admin's reads of personal or restricted files are
  recorded at the tool server (`POST /internal/admin/actions`), the reads before the text is
  served.

## Where the contract isn't met as written

- **`name` in `Me`** is the user database's display name, or the handle when it has none
  (`display_name` is served too, as untrusted text).
- **Sources are what the audit log can identify** (`docs/toolserver.md`, **What an item is
  shown with**): Jira, Confluence, docs and context titles are the ones recorded at the read;
  Slack sources are always `Slack thread in C01` (no Slack text or title is recorded); reads
  from before titles were recorded show ids (`PAY-212`, `Confluence page 12345`). `excerpt`
  is always `null`; `Source.url` can be `null`, which the contract shows as a string.
- **`external`** is known once the tool server has checked the channel's audience, which it
  does on an approve; it forgets that on restart. Before that a draft reads
  `external: false` and the approve answers `409 needs_external_confirm` as before.
- **`target.url`** is `null` where no link can be built from ids: a Confluence or Jira
  target without a linked Atlassian site.
- **Error codes:** every one, with whether it means nothing changed, is in
  `docs/web-api.md`, **Error codes**.

## Limits of "ask for a change"

- The prompt asks the bot not to write, but nothing enforces it: a personal bot that has write
  tools could still draft one. That draft would wait for approval like any other (nothing is
  written without one), and the suggestion fails with a plain sentence. A runtime task option
  such as `{"mode": "answer_only"}` that gives the harness no write tools would enforce it.
- The task's `result` is used as the body verbatim (less a code fence). A bot that adds a
  preamble puts it in the suggestion; the PM sees it before accepting.
- The draft's text is put in the prompt. It is the PM's own draft, but its content came from
  sources the bot read, so it is untrusted input to the model like any other.
- A suggestion lives in memory without `PMOS_WEB_DATABASE_URL`: a restart forgets it, and
  the task's result is then only on the task. With Postgres it is kept.

## Security

**Threat model.** Attackers we design for: another website the PM visits (CSRF, framing,
reading responses), someone on the network (TLS at the proxy, `Secure` cookie), a member
reaching another member's drafts or tasks by id, a member removed from the team who still
has a session, and a stolen copy of the session store. Out of scope until built: an attacker
who can reach the port while development sign-in is on (they are anyone), and a compromised
`pmos-web` host (it holds the tokens below).

**Development sign-in must never be exposed.** With `PMOS_WEB_DEV_LOGIN=1`,
`POST /api/auth/dev-login {handle}` believes the handle: anyone who reaches the port signs
in as any member, admins included, and can approve their drafts as them. The server logs a
warning at start-up. Bind it to `127.0.0.1` (the default) and never put it behind a public
proxy. Turning Entra ID on does not make it safe: with both on, development sign-in still
bypasses Entra entirely, and the server logs a louder warning.

**Development sign-in only on loopback, and never beside Entra by accident.** It refuses to
start on a `PMOS_WEB_BIND` that isn't loopback, and refuses to start beside Entra ID unless
`PMOS_WEB_DEV_LOGIN_WITH_ENTRA=1` says that is meant (a developer's machine).

**Entra ID sign-in proves who the browser is, not what they may do.** Membership and roles
come only from the user database; group and role claims are ignored. A person is found by
the token's `(tid, oid)`; a first sign-in links that key by `preferred_username` only (never
`email`), to an active user whose sign-in name an admin confirmed and who holds neither Admin
nor Auditor, and the tool server re-checks and audits it. Guests (`acct`), other identity
providers (`idp`) and a registration without the `acct` claim are refused before any
lookup; a name already linked to another account is refused, audited and flagged
(`docs/entra-sign-in.md`). Logout ends only the PM OS session.

**This backend is still a deputy, now a narrower one.** It holds its own tool server token,
which reaches only the approvals routes, the read-only admin routes, the admin proposal and
event routes (no sessions, grants, account linking or credentials; the admin routes check the
viewer's role themselves, and the proposal and event routes need a signed assertion in every
mode), a runtime token for the `web` client, and the two keys that sign its user assertions.
It holds **no GitHub token**: a proposal is opened by the tool server, re-checked there, with
the acting admin's own GitHub account, so a compromised `pmos-web` can't write to the
repository, and can open a proposal only as an admin, only on the allow-listed paths, only
with content that passes the tool server's own checks, and only as a pull request that branch
protection makes someone else approve. With the
runtime and the tool server in `PMOS_ASSERTION_MODE=require`, the tokens alone no longer act
for anyone: a call that names a PM needs an assertion for that PM, bound to that request and
good for 60 seconds, once. But the keys sit in this process: **a compromised `pmos-web` can
still sign for any team member**, not only those signed in. That stops only when the key is
held by something that checks the session itself (a separate signer, or an identity-provider
token the receivers verify), which is not built. See `docs/user-assertion.md`, "What this does
and does not fix".

## Persistence

With `PMOS_WEB_DATABASE_URL`, `src/pg.rs` keeps sessions, per-PM settings and follow-ups in
Postgres, migrating `migrations/` at start-up into its own table (`_pmos_web_migrations`).
Tables: `web_sessions` (the SHA-256 of the id only, never the cookie), `web_user_settings`,
`web_draft_edits`, `web_suggestions`, `web_practice_drafts`, `web_draft_flags`,
`web_revision_tasks`, `web_followups`, `web_brief_days`. Every row is keyed by handle and every
query filters on it. Idle and absolute session expiry are in the `UPDATE … WHERE` that reads a
session, so an expired one can't be used before a purge; the purge (sessions past either
limit, expired follow-ups and `changed` items, edits and suggestions untouched for 7 days,
flags older than 30) runs every ten minutes (`app::Housekeeping`) and at each sign-in; the
day's `changed` items are emptied, while the time of the last delivery is kept for
`GET /api/brief`'s `delivered_at`. `web_sessions` also keeps an Entra session's `entra_oid` and
`entra_tid` (migration `0002`). `SettingsStore::update` is one
transaction holding `FOR UPDATE` on the PM's settings row. Without a URL the stores are in
memory, a warning is logged, and a restart signs everyone out and forgets edits, suggestions,
practice drafts and follow-ups. The session's CSRF token is stored (it is not the cookie).
An edit of a real draft is no longer kept here: it lives on the item at the tool server,
which removes a Slack draft's edit with its text (24 hours after it expires or is decided).

**Not built:** rate limiting (on sign-in, on asks and on change requests; admin proposals have
the tool server's in-memory limit per admin); an audit record of web actions (the tool server
audits decisions, admin reads, admin proposals, admin refusals and sensitive reads, with `web`
as the client); a
per-PM limit on running tasks; admin tasks-by-status counts (the runtime has no team-wide read
for the `web` client); and, for Entra ID sign-in (`docs/entra-sign-in.md`, "Not built"): roles from
group or app-role claims, conditional access awareness, single logout, and token refresh (only the ID
token is used, once). The deploy kit (`deploy/`) does
not pass the `PMOS_WEB_ENTRA_*` variables to the `web` service yet.
