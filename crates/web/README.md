# `pmos-web`: the web app's backend

`pmos-web` holds the browser session and serves `/api` for the web app (`web/`), exactly as
`docs/web-api.md` describes. It calls the bot runtime and the tool server for the signed-in
PM. The browser never sees a service token, a connector token or another PM's data.

```
browser ──HTTPS──► pmos-web :8770 ──► pmos-bots :8750        (client "web", X-Pmos-User: <handle>)
                               └──► pmos-toolserver :8710  (approvals, service token)
                               └──► content/team.yaml      (re-read on every request)
```

## Running it

```sh
PMOS_WEB_DEV_LOGIN=1 \
PMOS_WEB_BOTS_TOKEN=<the runtime's "web" client token> \
PMOS_WEB_TOOLSERVER_TOKEN=<the tool server's service token> \
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
| `PMOS_WEB_TOOLSERVER_TOKEN` | required | The tool server's `PMOS_SERVICE_TOKEN`. At least 32 characters, different from the runtime token |
| `PMOS_CONTENT_ROOT` | `content` | Where `team.yaml` is. The server won't start if it doesn't parse |
| `PMOS_WEB_STATIC_DIR` | none | The built web app. Must contain `index.html`, served for any unknown non-`/api` path |
| `PMOS_WEB_PUBLIC_URL` | none | The origin browsers use. Writes must come from it; without it, `Origin` must match `Host` |
| `PMOS_WEB_SESSION_IDLE_MINUTES` | `480` | Idle expiry |
| `PMOS_WEB_SESSION_MAX_MINUTES` | `1440` | Absolute expiry from sign-in |

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

Every response carries `X-Content-Type-Options: nosniff`, `X-Frame-Options: DENY`,
`Referrer-Policy: same-origin`; `/api` answers are `Cache-Control: no-store` with a CSP that
loads nothing, and the app's files get `default-src 'self'` (no inline script or style).

## How the contract maps upstream

- **Drafts** are the PM's tool server approval items (`GET /internal/approvals?user=`).
  `kind`, `title`, `body` and `target` come from the write tool's arguments (the table in
  `src/drafts.rs`). Items of other write tools (`bets.record`) are not drafts and are left
  out. `executing` reads as `approved` with no `result_url` yet; `result_url` is the
  `url` of the item's `result`.
- **Edit** (`PUT`) is kept here. **Approve** sends `edit` with the edited arguments when
  there is a saved edit and `approve` otherwise, always with `decided_by` = the signed-in
  handle. The tool server's `409 needs_external_confirm`, `audience_blocked` and `expired`
  pass through; its `503 audience_unavailable` too. A `409` without a code is
  `not_pending` when the item has since been decided (in Slack, say), else
  `not_approvable`; a `400` on edited arguments is `422 bad_edit`.
- **Ask for a change** creates a task for `pm-<handle>` (`source: "web"`) whose prompt holds
  the instruction and the current text and says to call no write tool, make no draft or
  approval, and answer with the revised text only. The task's `result` (without a code fence)
  becomes the suggestion's `body`. These tasks are hidden from `GET /api/tasks`.
- **Practice drafts** live only here; nothing is sent upstream for them, and asking for a
  change to one answers at once with a canned note.
- **Tasks:** `POST /v1/tasks {bot: "pm-<handle>", requested_by, text, source: "web"}`, and
  reads of the PM's own tasks and their events, each with `X-Pmos-User: <handle>`.
- **Brief and follow-ups:** `501 not_built`.

## Where the contract isn't met as written

- **`name` in `Me`** is the handle: `team.yaml` has no display name. The user database will.
- **`sources` is always `[]`.** Neither upstream says what the bot read for a draft: the
  approval item has no sources, the runtime's task events are free-text milestones, and the
  trace (which has the tool calls) is the console client's only. Needed, one of:
  - tool server: `GET /internal/approvals/{id}/sources` → `{sources: [{system, id, title,
    url, excerpt?}]}`, from the audit events of the item's `run_id` (they already record
    `sources`), with the same token rules as `GET /internal/approvals/{id}`; or
  - runtime: `GET /v1/tasks/{id}/sources` for clients `slack` and `web` (own tasks), from the
    trace's `tool_result` entries of read tools.
- **`external`** is only known after the tool server answered `needs_external_confirm` once
  (it is remembered for that draft). To know it up front the tool server would need an
  `audience` field on the item, or `GET /internal/approvals/{id}/audience` →
  `{external, members, outsiders}`.
- **`target.url`** is always `null`: the item holds a space key, project key or channel id,
  not a link. The tool server would need to add `target_url` to items it can link (a parent
  page, a Slack thread permalink).
- **`expires_at`** assumes the tool server's default 24 hours (`PMOS_SLACK_DRAFT_TTL_HOURS`);
  the item should carry its own `expires_at`.
- **Error codes added** beyond the contract: `bad_origin`, `bad_request`, `not_approvable`,
  `bad_edit`, `no_suggestion` (accepting before the text is ready), `audience_unavailable`,
  `task_refused`, `upstream_unavailable`, `team_unavailable`, `unavailable`.

## Limits of "ask for a change"

- The prompt asks the bot not to write, but nothing enforces it: a personal bot that has write
  tools could still draft one. That draft would wait for approval like any other (nothing is
  written without one), and the suggestion fails with a plain sentence. A runtime task option
  such as `{"mode": "answer_only"}` that gives the harness no write tools would enforce it.
- The task's `result` is used as the body verbatim (less a code fence). A bot that adds a
  preamble puts it in the suggestion; the PM sees it before accepting.
- The draft's text is put in the prompt. It is the PM's own draft, but its content came from
  sources the bot read, so it is untrusted input to the model like any other.
- A suggestion lives in memory: a restart forgets it, and the task's result is then only on
  the task.

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

**This backend is a confused deputy, like the console.** It holds the tool server's
service token, which may decide any user's approvals (and link accounts), and a runtime token
that may create tasks for any PM it names in `X-Pmos-User`. Ownership is checked here, before
every call; a bug or a compromise of `pmos-web` lets it act for anyone. The planned fix (Web 1,
`docs/web-client.md`): a short-lived signed user assertion (the handle, an audience, a few
minutes' expiry, signed by a key only the web server holds), sent with each call and checked by
the runtime and the tool server, so a request can only act for the person who signed in.

**Not built:** Entra ID sign-in (`auth::EntraProvider` documents the seam and is deliberately
not an `AuthProvider`); persistence (sessions and settings are in memory: a restart signs
everyone out and forgets edits, suggestions and practice drafts; `src/session.rs` and
`src/settings.rs` say what a Postgres store needs); rate limiting (on sign-in, on asks and on
change requests); an audit record of web actions (the tool server audits decisions, with the
service token as the decider); and a per-PM limit on running tasks.
