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
- **Providers:** sign-in sits behind one interface. **Entra ID** (`docs/entra-sign-in.md`) is
  the production provider, on when `PMOS_WEB_ENTRA_TENANT_ID`, `PMOS_WEB_ENTRA_CLIENT_ID`,
  `PMOS_WEB_ENTRA_CLIENT_SECRET` and `PMOS_WEB_PUBLIC_URL` are all set. **Development
  sign-in** (`PMOS_WEB_DEV_LOGIN=1`) enables the development route below and logs a warning.
  `pmos-web` starts with either or both (both logs a loud warning) and refuses to start with
  neither, or with Entra half-configured. `GET /api/auth/providers` tells the app which buttons
  to show.
- **Who you are:** the signed-in handle must be a member in `team.yaml`. Roles come from there.
  Entra ID finds the member by email: the token's `preferred_username` (or else `email`),
  lower-cased, against members' `email` field; never by handle.
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
| `GET /api/auth/providers` | | `{providers: ["entra", "dev"]}`: the providers that are on, Entra first. No session needed. |
| `GET /api/auth/login[?return_to=/path]` | | `303` to Microsoft's authorize endpoint, setting the `__Host-pmos-entra` flow cookie (ten minutes). `return_to` is kept only if it is a same-origin path (`/…`, not `//…`); else `/`. `404` unless Entra is on. If Microsoft's metadata can't be fetched: `303` to `/#/signed-out?reason=unavailable`. |
| `GET /api/auth/callback?code&state` | | Microsoft's redirect back. `303` to the `return_to` path (default `/`) with the session cookie, or `303` to `/#/signed-out?reason=<reason>` with no session. Always clears the flow cookie. `404` unless Entra is on. |
| `POST /api/auth/dev-login` | `{handle}` | `204` and the cookie. `404` unless development sign-in is on. `403 not_a_member` for a handle not in `team.yaml`. |
| `POST /api/auth/logout` | none | `204`; the session ends. Only PM OS's session: Microsoft's own sign-in session is untouched, so "Sign in with Microsoft" may then sign straight back in without a prompt. |

The callback's `reason`s, for the app's signed-out page:

| `reason` | Meaning | Suggested text |
| --- | --- | --- |
| `not_a_member` | Microsoft signed them in, but no `team.yaml` member has that email. | "You're not on the team list yet. Ask a platform owner to add you." |
| `cancelled` | They cancelled at Microsoft (`access_denied`). | "Sign-in was cancelled." |
| `expired` | No sign-in in progress in this browser, or it took over ten minutes. | "That sign-in expired. Try again." |
| `unavailable` | Microsoft couldn't be reached. | "Microsoft sign-in isn't answering. Try again in a minute." |
| `failed` | Anything else (a wrong or reused `state`, a refused code, a token that failed a check). | "Sign-in didn't work. Try again." |

## Me

| Method and path | Body | Answer |
| --- | --- | --- |
| `GET /api/me` | | `Me` |
| `PUT /api/me/settings` | `{nudges?: bool}` | `Me` |
| `POST /api/me/hints` | `{seen: "sources" \| "ask" \| "approve"}` | `Me` |
| `DELETE /api/me/hints` | | `Me`, with `hints_seen` empty ("Show hints again") |

```json
{"handle": "ana", "name": "Ana Silva", "roles": ["pm"],
 "permissions": ["ask-bot", "view-own-tasks", "manage-own-routines", "decide-own-approval",
                 "view-own-traces", "link-own-accounts"],
 "csrf_token": "…", "first_visit": true, "hints_seen": ["sources"], "nudges": true}
```

`first_visit` is true until the PM first opens a draft. `nudges: false` also hides hints.

`permissions` lists what the person's roles allow (the permission matrix,
`crates/contract/src/permissions.rs`), kebab-case, in a fixed order; an unknown role adds
nothing. It is for showing and hiding parts of the app; the server checks every request
itself. Show the Admin area when it contains `view-audit-log` or `manage-connectors` (see
**Admin and auditor**). An older server leaves `permissions` out: treat that as `[]`. An
auditor's is `["view-audit-log", "export-audit"]`; an admin's has all 18.

## Drafts

A draft is a pending write (a Confluence page, a Jira issue, a Slack reply or message) that a
bot prepared and the PM has not decided yet: the tool server's approval item, shown as a
document.

| Method and path | Body | Answer |
| --- | --- | --- |
| `GET /api/drafts` | | `{drafts: [DraftSummary]}`, newest first; pending ones, then the 20 most recently decided |
| `GET /api/drafts/{id}` | | `Draft` |
| `PUT /api/drafts/{id}` | `{title?, body}` | `Draft`. Saves the PM's edit; nothing is sent to the target system. `409 not_pending` once decided. |
| `POST /api/drafts/{id}/changes` | `{instruction}` | `202 {suggestion: Suggestion}`. Asks the bot to revise the draft. One at a time: `409 change_in_progress`. |
| `GET /api/drafts/{id}/suggestion` | | `{suggestion: Suggestion \| null}` |
| `POST /api/drafts/{id}/suggestion/accept` | | `Draft`, with the suggested text as its body |
| `POST /api/drafts/{id}/suggestion/reject` | | `Draft`, unchanged |
| `POST /api/drafts/{id}/approve` | `{confirm_external?: bool, version?: string}` | `Draft` with `status: "approved"` and `result_url`. `409 needs_external_confirm`, `409 audience_blocked`, `409 expired`, `409 not_pending`, `409 stale`. |
| `POST /api/drafts/{id}/reject` | `{reason?}` | `Draft` with `status: "rejected"`. `reason` is one of `wrong_facts`, `wrong_target`, `tone`, `incomplete`, `should_not_draft`, `other`. |
| `POST /api/drafts/practice` | | `Draft`, a practice draft for this PM. Idempotent while one exists. |
| `DELETE /api/drafts/{id}` | | `204`, practice drafts only; `409 not_practice` otherwise. |

```json
// DraftSummary
{"id": "apr_…", "kind": "confluence_page", "title": "PRD: TCB grace period setting",
 "bot": "pm-ana", "status": "pending", "created_at": "…", "expires_at": null,
 "task_id": "tsk_…", "practice": false}

// Draft = DraftSummary plus
{"body": "markdown", "edited": false,
 "target": {"system": "confluence", "label": "Space PM, under Specs", "url": null},
 "sources": [Source], "result_url": null, "external": false, "version": "7"}

// Source
{"id": "s1", "system": "jira", "title": "PAY-212: TCB grace period", "url": "https://… or null",
 "excerpt": null}

// Suggestion
{"status": "working", "instruction": "shorten the risks section", "body": null, "error": null}
```

- `kind` is `confluence_page`, `jira_issue`, `slack_reply` or `slack_message`.
- `status` is `pending`, `approved`, `rejected`, `expired` or `failed`.
- `Suggestion.status` is `working`, `ready` or `failed`. `body` is set when `ready`, `error`
  (a plain sentence) when `failed`.
- `expires_at` is set for Slack drafts, which expire after 24 hours.
- `version` is optional and opaque: it changes whenever the draft text or title changes (an edit,
  an accepted suggestion). The app sends back the `version` of the draft the PM was looking at in
  the approve body. When it is sent and does not match the stored draft, the server answers
  `409 stale` and publishes nothing; the app reloads the draft and tells the PM to read it again.
  A server that does not set `version` ignores the field, and an approve without it skips the
  check.
- `external` is true when approving would post to a channel shared outside the company; the
  app must then ask before sending `confirm_external: true`.
- **A practice draft** (`practice: true`) lives only in `pmos-web`. Approving it creates
  nothing anywhere: it becomes `approved` with `result_url: null`.
- **Sources** are what the bot read for the task that made the draft. When the runtime cannot
  say, `sources` is empty and the app says so. `title` is the item's own title where PM OS
  recorded one at the read: `PAY-212: TCB grace period` for a Jira issue, the page title for
  Confluence and the docs, the file's title for the context library. Slack sources are
  always `Slack thread in C01` or `Slack message in C01` (Slack text is kept at most 24
  hours, so no title is recorded), and reads from before titles were recorded show the id
  (`PAY-212`, `Confluence page 12345`). `excerpt` is always `null`: no source text is kept.
  `url` may be `null`.
- **`body`** is shown as the bot wrote it, except that a body written entirely on one line
  with the characters `\n` where line breaks belong is shown with real line breaks (and
  `\t` as tabs). That repair is for display only: approving without an edit sends the bot's
  own text. Saving an edit (`PUT`) makes what the PM saw the text that is sent.

## Asking

| Method and path | Body | Answer |
| --- | --- | --- |
| `POST /api/tasks` | `{text}` | `201 Task`. The PM's personal bot takes it. |
| `GET /api/tasks/{id}` | | `Task` |
| `GET /api/tasks` | | `{tasks: [Task]}`, the PM's 20 most recent, without `events` |

```json
{"id": "tsk_…", "bot": "pm-ana", "text": "…", "status": "working",
 "result": null, "error": null, "draft_ids": [], "created_at": "…", "updated_at": "…",
 "progress": {"done": 2, "total": 3}, "retrying": false,
 "events": [{"at": "…", "kind": "milestone", "text": "Reading Jira"},
            {"at": "…", "kind": "milestone", "text": "Drafting in Confluence"}]}
```

`status` is `queued`, `working`, `needs_approval`, `done`, `failed` or `cancelled`. A failed
task carries a plain sentence in `error`. The app polls `GET /api/tasks/{id}` every two
seconds until the task settles.

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

## The daily brief and follow-ups

| Method and path | Body | Answer |
| --- | --- | --- |
| `GET /api/brief` | | `Brief`: the PM's open follow-ups and `changed` items, with empty lists when nothing is stored; or `501 {"code":"not_built"}` while the server's `PMOS_WEB_BRIEF` is off, and the app then hides that part of the home page |
| `POST /api/followups/{id}/dismiss` | | `204` ("Not mine"), the same again if repeated; `404` for an id that isn't the PM's, or is dismissed or expired |
| `POST /api/followups/{id}/draft` | | `201 Task`, a reply (waiting on you) or gentle nudge (you wait on) draft that arrives as a draft to approve; `404` as above |

```json
{"date": "2026-11-16",
 "changed": [{"system": "jira", "title": "PAY-212 moved to Blocked", "summary": "…", "url": "…"}],
 "waiting_on_you": [{"id": "fu_…", "system": "slack", "who": "Ben", "summary": "asked about the pricing page", "url": "…", "age_days": 2}],
 "you_wait_on": [{"id": "fu_…", "system": "slack", "who": "Legal", "summary": "the DPA question", "url": "…", "age_days": 3}]}
```

Every row has a `url` to its source. Nothing is posted from a follow-up without the PM
approving the draft it produces.

A follow-up is a one-line summary, never message text. Slack-derived ones expire after 24
hours, all others after a week, and expired ones are deleted, not just hidden; `changed`
items last 24 hours.

**Producer route (server to server, not for the app).** `PUT /internal/brief/{handle}` with
`Authorization: Bearer <PMOS_WEB_BRIEF_TOKEN>` replaces that member's follow-ups and `changed`
items: `{"date"?: "2026-11-16", "changed": [{system, title, summary?, url}], "waiting_on_you":
[{system, who, summary, url, created_at?}], "you_wait_on": [...]}`. At most 50 items per list,
a 128 KiB body, one-line text (`summary` at most 200 characters, `who` 80), `system` made of
`a-z`, digits, `-` and `_`, every `url` `http` or `https` without credentials; anything else
is `422 bad_brief` (`413` for an oversize body, `401 bad_token`, `404` for a handle not on the
team) and nothing is stored. A follow-up sent again keeps its age and a dismissal; one left
out is removed. **The caller is the Slack service** (`pmos-slack`, `crates/slack/src/brief_feed.rs`):
when a PM's daily brief finishes, it sends the items of the brief's machine-readable block
(`docs/daily-brief.md`, "The web app's panels") here, with `PMOS_WEB_URL` and
`PMOS_WEB_BRIEF_TOKEN` set on it. It sends nothing until a brief runs, so a PM with no brief
routine has empty lists.

## Admin and auditor

Read-only views for the two roles beyond PM. Every route is `GET`; any other method is
`405 method_not_allowed`. Roles come from `team.yaml`:

| Route | `admin` | `auditor` | anyone else (`pm`, `reviewer`, `skill-author`) |
| --- | --- | --- | --- |
| `GET /api/admin/audit` | yes | yes | `403 forbidden` |
| `GET /api/admin/audit/export` | yes | yes | `403 forbidden` |
| `GET /api/admin/team` | yes | yes | `403 forbidden` |
| `GET /api/admin/overview` | yes | yes | `403 forbidden` |
| `GET /api/admin/connectors` | yes | `403 forbidden` | `403 forbidden` |

The checks are the permission matrix: `audit` needs `view-audit-log`, `audit/export`
`export-audit`, `team` and `overview` `view-audit-log` or `manage-members`, `connectors`
`manage-connectors`. A person with several roles gets the union.

**Errors** (always `{"code", "error"}`):

| Status and code | When |
| --- | --- |
| `401 signed_out` | No live session. |
| `403 not_a_member` | The handle is no longer in `team.yaml` (every session is ended). |
| `403 forbidden` | The role doesn't allow this route. Also when the tool server, which checks the role again on its side, refuses. |
| `400 bad_request` | A query parameter is malformed (see each route). Nothing is sent upstream. |
| `502 upstream_unavailable` | The tool server didn't answer, or answered something unreadable. |
| `503 team_unavailable` | `team.yaml` can't be read. |

**What the auditor sees, and doesn't.** Metadata only: who did what, when, through which
client, to which system and target id, with what outcome. Never a draft body, Slack text, a
prompt, a model reply, a task result, a source title, an argument hash or a token. Audit
events are kept (the table is append-only); nothing here deletes them. The auditor does not
see connectors at all, and sees no PM's drafts or tasks (those routes need PM permissions an
auditor doesn't have).

**Every admin read is itself audited.** The tool server records each call as an event of its
own (`action` `admin.audit`, `admin.audit_export`, `admin.accounts` for the team page,
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
| `from` | RFC 3339 time | Events at or after it. |
| `to` | RFC 3339 time | Events strictly before it. |

Unknown parameters are ignored; an empty value counts as absent. `400 bad_request` for a
`limit` outside 1–100 or not a number, an unknown `outcome`, a malformed time, `from` after
`to`, or an `actor`, `action` or `before` with other characters. A `before` the tool server
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
| `actor` | string | The handle the event is about: the PM whose session or approval it was, the admin who read the log. |
| `client` | string | Who made the call: `mcp` (a bot or skill run, from its sandbox), `orchestrator` (the bot runtime or a platform-owner tool), `bot` (the Slack app), `web` (this web app), `console` (the operator console). |
| `action` | string | What was done: a tool name (`jira.search`, `confluence.create_draft`, `slack.post_reply`, `memory.write`, …), `credentials.unlink`, or `admin.*` for these reads. |
| `system` | string | The part of `action` before the first dot (`jira`, `confluence`, `slack`, `docs`, `context`, `memory`, `credentials`, `admin`, …). |
| `target` | string or `null` | What was written or decided: a page or issue URL, an approval id (`apr_…`), a memory note (`memory:…`), the provider of an unlink. `null` for reads and most refusals. |
| `outcome` | string | `ok` (ran), `denied` (refused: not allowed, above the data ceiling, wrong person), `pending` (a write queued for approval), `approved` (approved and written), `rejected`, `granted` (approved in the bot's own flow), `started` (an approved write about to run), `error` (failed), `failed` (an approved member couldn't be committed), `expired` (a Slack draft nobody decided; its text was removed). |
| `detail` | string or `null` | A short note, only when it is one of the fixed notes the tool server writes itself: `edited before approval`, `reason: <wrong_facts\|wrong_target\|tone\|incomplete\|should_not_draft\|other>`, `invalid draft (1 of 2)`, `approved in the inbox; writing`, `undecided after 24 h: …` and a few more. Otherwise `null`: a refusal's or a connector's message can quote arguments or content, so it is never served. |
| `sources` | integer ≥ 0 | How many items the call read (context files, Jira issues, pages, Slack messages). The items are not served. |
| `titled_sources` | integer ≥ 0 | How many of those had a title recorded. **Only the count:** a title is a Jira issue's summary or a page's title, which can carry content, so titles are never served. |
| `skill` | string or `null` | The run's skill, for a skill session. |
| `bot` | string or `null` | The run's bot, for a bot session. |
| `task_id` | string | The run: a task id (`tsk_…`) for bot tasks, a run id otherwise, `admin` for admin reads. Not a link: an auditor can't open tasks. |

### `GET /api/admin/audit/export`

The same events as CSV, newest first. Takes the same filters as `GET /api/admin/audit`
(`from`, `to`, `actor`, `action`, `outcome`, with the same `400`s), not `limit` or `before`.
Needs `export-audit`.

- `200`, `Content-Type: text/csv; charset=utf-8`,
  `Content-Disposition: attachment; filename="pmos-audit.csv"`.
- At most **10,000 rows**. `X-Pmos-Truncated: true` when more matched and were left out (narrow
  `from` and `to` and export again), `false` otherwise. `X-Pmos-Row-Count` is the number of
  rows, the header row not counted.
- A header row, then one row per event, every line ending `\r\n`. Columns, in order: `at`,
  `actor`, `client`, `action`, `system`, `target`, `outcome`, `detail`, `sources`,
  `titled_sources`, `skill`, `bot`, `task_id`, `id` (the fields above; `null` is an empty cell).
- Every cell is quoted, with `"` doubled. **Formula-safe:** a cell starting with `=`, `+`, `-`,
  `@`, a tab or a carriage return gets a leading `'`, so a spreadsheet shows it as text.
- Errors are JSON, as above.

```
"at","actor","client","action","system","target","outcome","detail","sources","titled_sources","skill","bot","task_id","id"
"2026-11-16T09:05:12.481Z","ana","web","jira.create_issue","jira","https://acme.atlassian.net/browse/PAY-301","approved","edited before approval","3","2","","pm-ana","tsk_01JD…","ev_8812"
```

The app downloads it with `fetch` (the session cookie goes along) and saves the blob, or links
to it directly.

### `GET /api/admin/team`

Every member in `team.yaml`, in file order (team bots' service identities, `bots.<name>`, left
out). Read-only: members change through Slack onboarding or a pull request to `team.yaml`.

```json
{"members": [
  {"handle": "ana", "name": "ana", "roles": ["pm"], "has_email": true, "bot": "pm-ana",
   "accounts": {"atlassian": true, "slack": true, "github": false}},
  {"handle": "audrey", "name": "audrey", "roles": ["auditor"], "has_email": false,
   "bot": "pm-audrey", "accounts": {"atlassian": false, "slack": false, "github": false}}
 ],
 "accounts_available": true,
 "linking": true}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `handle` | string | The member's handle. |
| `name` | string | The handle again: `team.yaml` has no display name yet. |
| `roles` | string[] | As written in `team.yaml`, including roles this server doesn't know. |
| `has_email` | boolean | Whether `team.yaml` has an email for them (Microsoft sign-in needs one). The address is not served. |
| `bot` | string | Their personal bot's name. It may not exist yet if they haven't finished onboarding. |
| `accounts` | object or `null` | Whether each account is linked, as booleans: `atlassian` (Jira and Confluence share one Atlassian link), `slack`, `github`. Never a token, scope, site or account id. `null` when the tool server couldn't be asked. |
| `accounts_available` | boolean | `false` when the tool server didn't answer: the list still shows, with every `accounts` `null`. |
| `linking` | boolean or `null` | Whether account linking is on (live connectors). With fake connectors nothing can be linked and every account is `false`. `null` when the tool server didn't answer. |

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
{"since": "2026-11-09T09:10:00.120Z", "pending_drafts": 4,
 "approved": 37, "rejected": 5, "denied": 2}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `since` | string, RFC 3339 UTC | Seven days before now: the window of the three counts below. |
| `pending_drafts` | integer | Drafts waiting for a decision now, across every member of the team. |
| `approved` | integer or `null` | Approved writes that ran since `since` (audit outcome `approved`). |
| `rejected` | integer or `null` | Drafts rejected since `since`. |
| `denied` | integer or `null` | Actions refused since `since` (audit outcome `denied`, admin reads refused included). |

The three counts are `null` when the tool server's audit log can't be read back. **Tasks by
status are not here:** the bot runtime gives the `web` client only the signed-in PM's own
tasks, with no team-wide read, so `pmos-web` can't count them.

### Known limits

- **The tool server's own role check believes the named viewer only as far as
  `PMOS_ASSERTION_MODE` does.** Each admin call names the viewer (`user=`) and carries a user
  assertion for them, and the tool server checks that viewer's role in its own `team.yaml`.
  With `require`, a call without a valid assertion for that viewer is refused. With `off` or
  `verify`, a call with no assertion is believed, so a stolen `PMOS_WEB_TOKEN` could read the
  audit log by naming an admin. Run `require` in production.
- **A read refused by `pmos-web` itself** (a `400`, or its own `403`) never reaches the tool
  server, so it is only in `pmos-web`'s log, not the audit log.
- **No free-text search**; `actor` and `action` match exactly.
