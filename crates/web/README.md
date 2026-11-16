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
`index.html` is `no-cache`.

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
  linked Atlassian site); a title of at most 200 characters; at most 20. `excerpt` is always
  `null`: nothing keeps source text to quote. If the call fails the draft still loads with
  `sources: []`.
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
- **Brief and follow-ups:** `501 not_built`.

## Where the contract isn't met as written

- **`name` in `Me`** is the handle: `team.yaml` has no display name. The user database will.
- **Sources are what the audit log can identify** (`docs/toolserver.md`, **What an item is
  shown with**): titles are ids for Jira, Confluence and Slack (`PAY-212`, `Confluence page
  12345`, `Slack thread in C01`), not issue summaries or page titles, because the audit log
  keeps ids only; `excerpt` is always `null`; `Source.url` can be `null`, which the contract
  shows as a string.
- **`external`** is known once the tool server has checked the channel's audience, which it
  does on an approve; it forgets that on restart. Before that a draft reads
  `external: false` and the approve answers `409 needs_external_confirm` as before.
- **`target.url`** is `null` where no link can be built from ids: a Confluence or Jira
  target without a linked Atlassian site.
- **Error codes added** beyond the contract: `bad_origin`, `bad_request`,
  `bad_content_type` (`415`), `not_approvable`, `bad_edit`, `no_suggestion` (accepting
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
