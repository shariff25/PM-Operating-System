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
  warning). `GET /api/auth/providers` tells the app which buttons to show.
- **Who you are:** the signed-in handle must be a member in `team.yaml`. Roles come from there.
  Entra ID finds the member by email: the token's `preferred_username` (or else `email`),
  lower-cased, against members' `email` field; never by handle. A member with `entra-oid` in
  `team.yaml` must also be that Entra account (the token's `oid`); otherwise sign-in ends at
  `reason=wrong_account`, and a session of the wrong account ends (`401 signed_out`) on its
  next request after `entra-oid` is added or changed.
- **HTTPS:** with an `https` `PMOS_WEB_PUBLIC_URL`, every response carries
  `Strict-Transport-Security: max-age=31536000`.
- **Health:** `GET /healthz` (outside `/api`, no session) answers `ok` while the process runs.
  `GET /readyz` answers `200` when it can serve and `503` when it can't, with
  `{"ready": bool, "checks": {"store": "ok"|"down", "team": …, "bot_runtime": …,
  "tool_server": …}}`: its own store answers, `team.yaml` reads, and the bot runtime and the
  tool server answer their `/healthz` within two seconds. No detail and no secrets; the
  reason goes to the server's log.
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
| `wrong_account` | Microsoft signed in an account with the right email, but `team.yaml` names another Entra account (`entra-oid`) for that member: the address was reassigned, or it is a second account. | "That Microsoft account isn't the one on the team list. Sign in with your own work account, or ask a platform owner." |
| `failed` | Anything else (a wrong or reused `state`, a refused code, a token that failed a check). | "Sign-in didn't work. Try again." |

An unknown `reason` should read as `failed`.

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
| `PUT /api/drafts/{id}` | `{title?, body}` | `Draft`, with `edited: true` and a new `version`. Saves the PM's edit **on the draft itself, at the tool server**: the Slack card and the console show it from then on, and approving anywhere (here, the Slack card, the console) writes it. Nothing is sent to the target system. A `title` is kept only for drafts that have one (Confluence pages, Jira issues); without one the saved title stays. Citation markers in `title` or `body` are removed before it is saved (see **Citations**). `409 not_pending` once decided, `409 expired` for an expired Slack draft, `422 bad_edit` if the tool server refuses the edited text for its target. A practice draft's edit stays in `pmos-web`. |
| `POST /api/drafts/{id}/changes` | `{instruction}` | `202 {suggestion: Suggestion}`. Asks the bot to revise the draft. One at a time: `409 change_in_progress`. |
| `GET /api/drafts/{id}/suggestion` | | `{suggestion: Suggestion \| null}` |
| `POST /api/drafts/{id}/suggestion/accept` | | `Draft`, with the suggested text as its body, saved as an edit is (`PUT` above): at the tool server, so every surface shows it |
| `POST /api/drafts/{id}/suggestion/reject` | | `Draft`, unchanged |
| `POST /api/drafts/{id}/approve` | `{confirm_external?: bool, version?: string}` | `Draft` with `status: "approved"` and `result_url`. Refusals (nothing was sent): `409 needs_external_confirm`, `409 audience_blocked`, `409 expired`, `409 not_pending`, `409 stale`, `409 not_approvable`, `422 bad_edit`, `503 audience_unavailable`, `503 unavailable`, `403 not_a_member`. **Outcome unknown:** `502` or `504 unknown_outcome` (see **Error codes**). |
| `POST /api/drafts/{id}/reject` | `{reason?}` | `Draft` with `status: "rejected"`. `reason` is one of `wrong_facts`, `wrong_target`, `tone`, `incomplete`, `should_not_draft`, `other`. The same refusals and `unknown_outcome` as approve, where they apply. |
| `POST /api/drafts/practice` | | `Draft`, a practice draft for this PM. Idempotent while one exists. |
| `DELETE /api/drafts/{id}` | | `204`, practice drafts only; `409 not_practice` otherwise. |

```json
// DraftSummary
{"id": "apr_…", "kind": "confluence_page", "title": "PRD: TCB grace period setting",
 "bot": "pm-ana", "status": "pending", "created_at": "…", "expires_at": null,
 "task_id": "tsk_…", "practice": false}

// Draft = DraftSummary plus
{"body": "markdown", "edited": false, "edited_at": null,
 "target": {"system": "confluence", "label": "Space PM, under “Specs”", "url": null},
 "sources": [Source], "result_url": null, "external": false, "version": "7",
 "restricted_sources": true, "audience": null,
 "marker": "Drafted with PM OS · run tsk_01JD…"}

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
- `Suggestion.status` is `working`, `ready` or `failed`. `body` is set when `ready`, `error`
  (a plain sentence) when `failed`.
- `expires_at` is set for Slack drafts, which expire after 24 hours.
- `version` is optional and opaque: it changes whenever the draft text or title changes (an edit,
  an accepted suggestion, from this app or another tab). The app sends back the `version` of the
  draft the PM was looking at in the approve body. When it is sent and does not match the stored
  draft, the server answers `409 stale` and publishes nothing; the app reloads the draft and
  tells the PM to read it again. A server that does not set `version` ignores the field, and an
  approve without it skips the check. If the draft was approved elsewhere (the Slack card) with
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
  **Slack labels never show a channel id**: PM OS keeps no channel names, so they read "A
  Slack thread", "A Slack channel", "A direct message thread" or "A direct message" (and the
  titles "Reply in a Slack thread", "Message to a Slack channel", "Direct message"). The
  channel's name is one tap away: `target.url` opens it in Slack, which shows the name to
  whoever can see it. Anything that isn't shaped like an id reads "An unrecognised channel"
  (or space, page, project, epic).
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
 "you_wait_on": [{"id": "fu_…", "system": "slack", "who": "Legal", "summary": "the DPA question", "url": "…", "age_days": 3}]}
```

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
[{system, who, summary, url, created_at?}], "you_wait_on": [...]}`. At most 50 items per list,
a 128 KiB body, one-line text (`summary` at most 200 characters, `who` 80), `system` made of
`a-z`, digits, `-` and `_`, every `url` `http` or `https` without credentials; anything else
is `422 bad_brief` (`413` for an oversize body, `401 bad_token`, `404` for a handle not on the
team) and nothing is stored. A follow-up sent again keeps its age and a dismissal; one left
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
| `403` | `not_a_member` | Not (or no longer) in `team.yaml`; every session of theirs ends. Also when the tool server doesn't count them a member on a decision. | Yes |
| `403` | `not_allowed` | Their role doesn't allow drafts, tasks or asking. | Yes |
| `403` | `forbidden` | Admin and auditor routes their role doesn't allow; the producer route called like a browser. | Yes |
| `404` | `not_found` | Unknown path or id, or someone else's draft, task or follow-up. | Yes |
| `405` | `method_not_allowed` | A known path with another method. | Yes |
| `409` | `not_pending` | The draft was already decided (perhaps in Slack). Read it again to show how. | Yes |
| `409` | `stale` | The draft changed since the `version` the PM approved. | Yes |
| `409` | `expired` | A Slack draft past its 24 hours. | Yes |
| `409` | `needs_external_confirm` | The channel is shared outside the company; ask, then approve with `confirm_external: true`. `audience` is now filled in. | Yes |
| `409` | `audience_blocked` | Built from restricted sources, and people outside the product team can see the channel. Only rejecting is left. | Yes |
| `409` | `not_approvable` | The tool server won't approve it as it stands (its tool or bot is no longer allowed). | Yes |
| `409` | `change_in_progress` | A change to this draft is already being written. | Yes |
| `409` | `no_suggestion` | Accepting a suggestion that isn't ready. | Yes |
| `409` | `not_practice` | Deleting a draft that isn't the practice draft. | Yes |
| `409` | `task_refused` | The runtime won't take the PM's task (not onboarded, no bot). | Yes |
| `413` | (no body) | The producer route's body is over 128 KiB. | Yes |
| `415` | `bad_content_type` | A write with a body that isn't JSON. | Yes |
| `422` | `bad_edit` | The edited text isn't valid for where the draft goes. | Yes |
| `422` | `bad_brief` | The producer route's body failed a check. | Yes |
| `501` | `not_built` | The daily brief is off on this server. | Yes |
| `502` | `upstream_unavailable` | The runtime or tool server couldn't be reached, refused, or answered something unreadable. On a decision, only when the request never reached the tool server, or it refused it. | Yes on a decision; for a task (`POST /api/tasks`), the task may have been made: reload the list |
| `502` | `unknown_outcome` | A decision was sent, but the answer was lost (the connection dropped, or the tool server failed with a `5xx`), and reading the draft again didn't show it decided. | **Unknown** |
| `504` | `unknown_outcome` | The same, when the tool server didn't answer in time (10 seconds). | **Unknown** |
| `503` | `audience_unavailable` | Slack couldn't say who is in the channel. | Yes |
| `503` | `unavailable` | `pmos-web`'s own store, or the tool server's, is down. | Yes |
| `503` | `team_unavailable` | `team.yaml` can't be read. | Yes |

After an `unknown_outcome` the server has already read the draft once: if it showed this PM's
approval (or rejection), the answer would have been that draft, `200`. Approving again is safe:
if the first approve went through, the second answers the approved draft and sends nothing.

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
