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
{"handle": "ana", "name": "Ana Silva", "roles": ["pm"], "csrf_token": "…",
 "first_visit": true, "hints_seen": ["sources"], "nudges": true}
```

`first_visit` is true until the PM first opens a draft. `nudges: false` also hides hints.

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
out is removed. **Nothing calls this route yet**: the Slack brief routine (`docs/daily-brief.md`)
or a later job is the intended caller.
