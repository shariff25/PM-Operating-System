# `pmos-web`: the web app's backend

`pmos-web` holds the browser session and serves `/api` for the web app (`web/`), exactly as
`docs/web-api.md` describes. It calls the bot runtime and the tool server for the signed-in
PM. The browser never sees a service token, a connector token or another PM's data.

```
browser ──HTTPS──► pmos-web :8770 ──► pmos-bots :8750        (client "web", X-Pmos-User + X-Pmos-Assertion)
                               └──► pmos-toolserver :8710  (approvals only, web token + X-Pmos-Assertion)
                               └──► content/team.yaml      (re-read on every request)
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

| Variable | Default | Notes |
| --- | --- | --- |
| `PMOS_WEB_BIND` | `127.0.0.1:8770` | |
| `PMOS_WEB_DEV_LOGIN` | off | `1` turns on development sign-in. **Required today**: with no provider the server refuses to start |
| `PMOS_BOTS_URL` | `http://127.0.0.1:8750` | |
| `PMOS_WEB_BOTS_TOKEN` | required | The `web` entry of the runtime's `PMOS_BOTS_TOKENS`. At least 32 characters |
| `PMOS_TOOLSERVER_URL` | `http://127.0.0.1:8710` | |
| `PMOS_WEB_TOOLSERVER_TOKEN` | required | The tool server's `PMOS_WEB_TOKEN`, which reaches only the approvals routes (older deployments used `PMOS_SERVICE_TOKEN`). At least 32 characters, different from the runtime token |
| `PMOS_ASSERTION_KEYS` | none | The keys pmos-web signs user assertions with: `web:bots:<kid>:<base64>` and `web:toolserver:<kid>:<base64>`, each optionally followed by `:<kid>:<base64>` for the previous key (`docs/user-assertion.md`, Keys). Without a key for an upstream, its calls carry no assertion. A malformed entry, a key under 32 bytes, or another service's pair refuses to start |
| `PMOS_CONTENT_ROOT` | `content` | Where `team.yaml` is. The server won't start if it doesn't parse |
| `PMOS_WEB_STATIC_DIR` | none | The built web app. Must contain `index.html`, served for any unknown non-`/api` path that isn't an asset; the icon is `favicon.svg` (or `favicon.ico`) here |
| `PMOS_WEB_PUBLIC_URL` | none | The origin browsers use. Writes must come from it; without it, `Origin` must match `Host` |
| `PMOS_WEB_SESSION_IDLE_MINUTES` | `480` | Idle expiry |
| `PMOS_WEB_SESSION_MAX_MINUTES` | `1440` | Absolute expiry from sign-in |
| `PMOS_WEB_DATABASE_URL` | none | Postgres for sessions, settings and follow-ups (`src/store_config.rs`). Without it everything is in memory and a start-up warning says a restart signs everyone out |
| `PMOS_WEB_BRIEF` | off | `1` turns on `GET /api/brief` and the follow-up routes; off, they answer `501 not_built` and the app hides the section |
| `PMOS_WEB_BRIEF_TOKEN` | none | The bearer token `PUT /internal/brief/{handle}` needs, at least 32 characters. Without it that route answers `501` |

`GET /healthz` answers `ok` with no sign-in. SIGTERM or Ctrl-C lets in-flight requests finish.

The `__Host-pmos` cookie is `Secure`, so browsers only keep it over HTTPS (or on
`http://localhost` in most browsers). Put a TLS proxy in front for anything else.

## How a request is checked

1. **Session:** the `__Host-pmos` cookie (`Secure; HttpOnly; SameSite=Lax; Path=/`, no
   `Domain`). The id is 256 random bits from the OS; the store keeps only its SHA-256. A
   session ends after 8 idle hours or 24 hours from sign-in, at logout, or when the person
   leaves the team. Signing in always makes a new id and ends the browser's old one.
2. **Membership:** the session's handle must be a member in `team.yaml`, re-read on every
   request. Someone removed gets `403 not_a_member` once and every session they have ends.
3. **Writes** (`POST`, `PUT`, `DELETE`): `Sec-Fetch-Site` must not be another site, `Origin`
   (or else `Referer`) must be the app (`403 bad_origin`), and `X-Pmos-Csrf` must equal the
   session's own random token, compared in constant time (`403 bad_csrf`).
4. **Roles,** from the permission matrix (`pmos_contract::permissions`): drafts need
   `decide-own-approval`, asking needs `ask-bot`, reading tasks `view-own-tasks`. An
   auditor-only member gets `403 not_allowed` on every draft and task route.
5. **Ownership:** a draft is the PM's when the approval item's `user` is their handle; a
   task when its `requested_by` is. Anything else is `404 not_found`, checked here before
   any decision is sent, because the tokens this backend holds could act for anyone.

6. **JSON only:** a `POST`, `PUT` or `DELETE` with a body must be `Content-Type:
   application/json` (`415 bad_content_type`), and sign-in always must, so a cross-site
   form can't reach a route or sign a victim in as someone else (login CSRF). A write with
   no body (logout, `DELETE /api/me/hints`) needs no type; it still needs the CSRF token.

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
  send now (the item's arguments with the saved edit applied; for a practice draft, its
  title and body). **Approve** takes an optional `version`: a different one is
  `409 stale` and nothing is sent.
- **Edit** (`PUT`) is kept here. **Approve** sends `edit` with the edited arguments when
  there is a saved edit and `approve` otherwise, always with `decided_by` = the signed-in
  handle; the version is checked against the same arguments it sends. The tool server's
  `409 needs_external_confirm`, `audience_blocked` and `expired` pass through; its
  `503 audience_unavailable` too. A `409` without a code is `not_pending` when the item has
  since been decided (in Slack, say), else `not_approvable`; a `400` on edited arguments is
  `422 bad_edit`.
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
  every tool-server call that names the PM (the inbox, sources, decisions) also carries
  `X-Pmos-Assertion`: issuer `web`, the upstream as audience, the PM as subject, roles from
  `team.yaml`, 60 seconds, bound to the method, path, query and body sent, signed afresh for
  each call. The re-read of one item after a conflicting decision (`approval(id)`) names no
  one and is sent without one; `ToolServer::approval_for(user, id)` signs it, once `app.rs`
  passes the handle there. `sid` is the fixed `unknown`: the upstream layer is given the
  handle, not the session id.
- **Brief and follow-ups** (`src/app/brief.rs`, `src/followups.rs`), on with `PMOS_WEB_BRIEF=1`
  and `501 not_built` otherwise. `GET /api/brief` answers `200` with the PM's open follow-ups
  (not dismissed, not expired) and the day's `changed` items, empty lists when nothing is
  stored. A follow-up holds a one-line summary (at most 200 characters, no message text), who,
  system and url; Slack ones expire after 24 hours and every other after a week, both purged.
  Its id is derived from the handle, kind, system and url, so a producer sending the same one
  again refreshes it (keeping its age and a dismissal) rather than copying it. `POST
  /api/followups/{id}/dismiss` is `204` and idempotent; `POST /api/followups/{id}/draft`
  makes a task for `pm-<handle>` (`source: "web"`) asking for a reply (waiting on you) or a
  gentle nudge (you wait on), written through the bot's normal write tool so it arrives as an
  ordinary approval; it needs `ask-bot`. Someone else's, a dismissed, an expired or an
  unknown id is `404`.
- **`PUT /internal/brief/{handle}`** is the producer's route and **nothing calls it yet**:
  the Slack brief routine or a later job will. It needs `Authorization: Bearer
  <PMOS_WEB_BRIEF_TOKEN>` (`401 bad_token`, constant-time compare), a member's handle, JSON,
  at most 128 KiB (`413`), and replaces that PM's follow-ups and `changed` items:
  `{date?, changed: [{system, title, summary?, url}], waiting_on_you: [{system, who, summary,
  url, created_at?}], you_wait_on: [...]}`. At most 50 per list, `system` lower-case
  `[a-z0-9_-]`, one-line text, `url` an `http` or `https` URL without credentials, unknown
  fields refused (`422 bad_brief`); nothing is stored if anything fails.

## Where the contract isn't met as written

- **`name` in `Me`** is the handle: a `team.yaml` member (`pmos_contract::model::TeamMember`)
  has no display name field, and the name onboarding takes from Slack stays on the join
  request and is not written to `team.yaml`. The user database will have one.
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
- **Error codes added** beyond the contract: `bad_origin`, `bad_request`,
  `bad_content_type` (`415`), `method_not_allowed` (`405`), `not_approvable`, `bad_edit`, `no_suggestion` (accepting
  before the text is ready), `audience_unavailable`, `task_refused`,
  `upstream_unavailable`, `team_unavailable`, `unavailable`. `stale` (`409`) is the
  contract's optional `version` check.

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
proxy.

**This backend is still a deputy, now a narrower one.** It holds its own tool server token,
which reaches only the approvals routes (no sessions, grants, account linking or credentials),
a runtime token for the `web` client, and the two keys that sign its user assertions. With the
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
flags older than 30) runs at sign-in, so no scheduler is needed. `SettingsStore::update` is one
transaction holding `FOR UPDATE` on the PM's settings row. Without a URL the stores are in
memory, a warning is logged, and a restart signs everyone out and forgets edits, suggestions,
practice drafts and follow-ups. The session's CSRF token is stored (it is not the cookie).
An edit of a Slack draft is Slack text: the tool server drops the draft's own text after 24
hours and the edit is unreachable by then, but the row itself waits for the 7-day purge.

**Not built:** Entra ID sign-in (`auth::EntraProvider` documents the seam and is deliberately
not an `AuthProvider`); rate limiting (on sign-in, on asks and on
change requests); an audit record of web actions (the tool server audits decisions, with the
`web` as the client); and a per-PM limit on running tasks.
