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
**Admin and auditor**); show the policy screens with `view-policies` and their propose
buttons with `edit-policies` (**Admin: policy**); show the content and harness screens with
`view-content` and their propose buttons with `edit-content` (**Admin: content and
harness**). An older server leaves `permissions` out: treat that as `[]`. An auditor's is
`["view-audit-log", "export-audit", "view-content", "view-policies"]`; an admin's has all 21.

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

## Admin: policy

The Policy Admin for the NVIDIA OpenShell sandbox every bot runs in (`docs/policy-admin.md`
says what it does and how it's set up). **Read-and-propose:** nothing here writes a file. A
change is opened as a GitHub pull request; a person reviews and merges it in GitHub.

| Route | `admin` | `auditor` | anyone else |
| --- | --- | --- | --- |
| `GET /api/admin/policy` | yes | yes | `403 forbidden` |
| `POST /api/admin/policy/validate` | yes | yes | `403 forbidden` |
| `GET /api/admin/policy/proposals` | yes | yes | `403 forbidden` |
| `POST /api/admin/policy/proposals` | yes | `403 forbidden` | `403 forbidden` |

Reading and validating need `view-policies`; proposing needs `edit-policies`. Signed out is
`401 signed_out`; every `POST` needs the CSRF token (`403 bad_csrf`) and JSON
(`415 bad_content_type`), as everywhere. The role check comes before anything else, so an
auditor proposing gets `403`, never `501`.

**What the server shows is its deployed revision.** The spec, the files and every validation
are computed from the content tree `pmos-web` is running with (`PMOS_CONTENT_ROOT`) and the
policy files beside it (`PMOS_WEB_POLICIES_ROOT`). That can be behind the repository's default
branch. A proposal is made on the default branch's head and is refused `409 stale_base` when a
file it writes differs there from the server's copy; `revision.commit` says which commit the
server has, when it can tell.

**What a policy is made from.** Policies are generated, never hand-written: `pmos policy
generate` derives each bot's policy from three inputs in the content tree, and those inputs are
the `spec` the admin edits:

- `bots.<bot>`: `role`, `scope` (`team` or `personal`) and `owner` (a handle), from
  `content/bots/<bot>/BOT.md`. A personal bot also reads its owner's
  `master/people/<owner>/AGENTS.md`, when there is one.
- `roles.<role>.skills`: from `content/roles/<role>/ROLE.md`. Each skill is uploaded read-only.
- `skills.<skill>.tools`: registry names (`docs.search`) from `content/skills/<skill>/SKILL.md`
  `metadata.tools`. They become the only tools `tools/call` may name (as MCP names,
  `docs_search`).

Everything else is `fixed`: the three destinations (tool server, gateway, runtime) and their
ports, REST routes, MCP methods and revisions, the worker binary, the system paths, the
workspace, the non-root identity and Landlock. They change only with code
(`crates/policy/src/lib.rs`), never here. New bots, roles and skills are content proposals
(**Admin: content and harness**), which regenerate the policies the same way.

### `GET /api/admin/policy`

```json
{"revision": {"commit": "5bb6de4c…", "source": "git"},
 "spec": {
   "bots": {"docs": {"role": "docs-helper", "scope": "team", "owner": "TODO"}},
   "roles": {"docs-helper": {"skills": ["docs"]},
             "pm-assistant": {"skills": ["ask", "meeting-notes", "…"]}},
   "skills": {"docs": {"tools": ["docs.search", "docs.read"]}, "…": {"tools": []}}},
 "spec_sources": {"bots": "content/bots/<bot>/BOT.md", "roles": "content/roles/<role>/ROLE.md",
                  "skills": "content/skills/<skill>/SKILL.md"},
 "fixed": {
   "services": [
     {"service": "toolserver", "rule": "pmos_toolserver", "host": "toolserver.pmos.internal",
      "port": 8710, "protocol": "mcp", "routes": null, "credential_profile": null},
     {"service": "gateway", "rule": "pmos_gateway", "host": "gateway.pmos.internal",
      "port": 8740, "protocol": "rest", "routes": ["POST /v1/chat/completions"],
      "credential_profile": "pmos-gateway"},
     {"service": "runtime", "rule": "pmos_runtime", "host": "bots.pmos.internal", "port": 8750,
      "protocol": "rest", "routes": ["POST /v1/worker/claim", "POST /v1/worker/tasks/*/complete",
      "POST /v1/worker/tasks/*/events", "POST /v1/worker/tasks/*/fail",
      "POST /v1/worker/tasks/*/heartbeat", "POST /v1/worker/tasks/*/trace"],
      "credential_profile": "pmos-worker"}],
   "mcp_path": "/mcp",
   "mcp_methods": ["initialize", "notifications/initialized", "tools/list", "tools/call"],
   "mcp_versions": ["2025-03-26", "2025-06-18", "2025-11-25"],
   "worker_binary": "/usr/local/bin/node", "workspace": "/sandbox/workspace",
   "content_mount": "/sandbox", "shared_content": ["templates", "reviewers", "playbooks"],
   "system_read_only": ["/app", "/dev/urandom", "/etc", "/lib", "/proc", "/usr", "/var/log"],
   "system_read_write": ["/dev/null", "/tmp"], "run_as": "sandbox",
   "landlock": "hard_requirement", "source": "crates/policy/src/lib.rs"},
 "bots": [
   {"bot": "docs", "role": "docs-helper", "scope": "team", "sandbox": "pmos-docs",
    "skills": ["docs"], "tools": ["docs_read", "docs_search"], "master": ["master/AGENTS.md"],
    "policy_file": "policies/bots/docs.yaml",
    "launch_spec_file": "policies/bots/docs.sandbox.yaml",
    "allowances_from": "checked_in",
    "allowances": {
      "network": [
        {"rule": "pmos_gateway", "service": "gateway", "host": "gateway.pmos.internal",
         "port": 8740, "ports": [], "allowed_ips": [], "protocol": "rest",
         "enforcement": "enforce", "tls": null, "access": null,
         "binaries": ["/usr/local/bin/node"], "credential": "pmos-docs-gateway",
         "routes": ["POST /v1/chat/completions"], "mcp_methods": [], "mcp_tools": [],
         "mcp_versions": []},
        {"rule": "pmos_toolserver", "service": "toolserver", "host": "toolserver.pmos.internal",
         "port": 8710, "ports": [], "allowed_ips": [], "protocol": "mcp",
         "enforcement": "enforce", "tls": null, "access": null,
         "binaries": ["/usr/local/bin/node"], "credential": null, "routes": [],
         "mcp_methods": ["initialize", "notifications/initialized", "tools/list", "tools/call"],
         "mcp_tools": ["docs_read", "docs_search"],
         "mcp_versions": ["2025-03-26", "2025-06-18", "2025-11-25"]}],
      "network_middlewares": [],
      "filesystem": {"include_workdir": false,
        "read_only": ["/app", "/dev/urandom", "/etc", "/lib", "/proc", "/sandbox/master/AGENTS.md",
                      "/sandbox/playbooks", "/sandbox/reviewers", "/sandbox/skills/docs",
                      "/sandbox/templates", "/sandbox/tools.yaml", "/usr", "/var/log"],
        "read_write": ["/dev/null", "/sandbox/workspace", "/tmp"]},
      "landlock": "hard_requirement",
      "process": {"run_as_user": "sandbox", "run_as_group": "sandbox"},
      "providers": ["pmos-docs-gateway", "pmos-docs-worker"],
      "uploads": ["master/AGENTS.md -> /sandbox/master/AGENTS.md",
                  "skills/docs -> /sandbox/skills/docs", "tools.yaml -> /sandbox/tools.yaml"],
      "summary": [
        "May connect to the model gateway at gateway.pmos.internal:8740 (rest, enforce) from /usr/local/bin/node: POST /v1/chat/completions.",
        "Uses the credential pmos-docs-gateway on gateway.pmos.internal:8740.",
        "May call these tools: docs_read, docs_search.",
        "May read this content: /sandbox/master/AGENTS.md, /sandbox/skills/docs, /sandbox/tools.yaml.",
        "May write only: /dev/null, /sandbox/workspace, /tmp.",
        "Runs as sandbox:sandbox; Landlock hard_requirement."]}}],
 "files": [
   {"path": "policies/bots/docs.sandbox.yaml", "state": "same",
    "generated": "# Generated by `pmos policy generate` …", "checked_in": "# Generated by …"},
   {"path": "policies/bots/docs.yaml", "state": "same", "generated": "…", "checked_in": "…"},
   {"path": "policies/providers/pmos-gateway.yaml", "state": "same", "generated": "…",
    "checked_in": "…"},
   {"path": "policies/providers/pmos-worker.yaml", "state": "same", "generated": "…",
    "checked_in": "…"}],
 "drift": {"checked_in_available": true, "in_sync": true, "violations": []},
 "generate_errors": [],
 "proposals_configured": true}
```

(Shortened where `…` appears; the real answer lists every item, every runtime route and every
summary line.)

| Field | Type | Meaning |
| --- | --- | --- |
| `revision.commit` | string or `null` | The commit the server's content is from: `PMOS_WEB_CONTENT_REVISION`, else read from the git files beside the content root. `null` when neither says. |
| `revision.source` | `"config"`, `"git"` or `null` | Where `commit` came from. |
| `spec` | object | The policy inputs, as above: every bot, role and skill the content defines (a file that doesn't lint is left out; see `generate_errors`). `owner` is a handle without `@`, `TODO` while unassigned. Lists are in file order. |
| `spec_sources` | object | Where each part of the spec lives in the repository. |
| `fixed` | object | What no spec can change (above). `routes` is `null` for the MCP endpoint; `credential_profile` is `null` for the tool server, which binds no provider (its session token is per task). |
| `bots[]` | array | One per bot the content generates a policy for, by name. Empty when `generate_errors` isn't. |
| `bots[].tools` | string[] | MCP tool names `tools/call` may name, sorted. |
| `bots[].master` | string[] | Master context files the bot reads, content-relative. |
| `bots[].allowances` | object or `null` | What the policy file allows, read field by field from it (below). `null` when the file is missing or doesn't parse. |
| `bots[].allowances_from` | `"checked_in"` or `"generated"` | `checked_in`: the file in the policies root (what OpenShell is given). `generated`: the server has no policies root, so this is what the content generates. |
| `files[]` | array | Every generated file, and every checked-in `.yaml` in `bots/` and `providers/`, by path. |
| `files[].state` | string | `same`, `differs` (drift: regenerate, or a hand edit), `missing` (generated, not checked in), `orphan` (checked in, not generated any more; a hand-written file there is also `orphan`) or `unknown` (no policies root). |
| `files[].generated`, `files[].checked_in` | string or `null` | The file's text: what generation writes, and what is checked in. `null` where there is none. |
| `drift.checked_in_available` | boolean | Whether the server has a policies root to compare with. |
| `drift.in_sync` | boolean or `null` | Every file `same` and no rule broken. `null` without a policies root. |
| `drift.violations[]` | `Issue[]` | Every PM OS rule a checked-in file breaks (`pmos policy check`'s rules: `net-egress`, `net-tools`, `fs-writable`, `cred-binding`, `proc-identity`, …), at its path. |
| `generate_errors[]` | `Issue[]` | Content lint errors that stop generation (then `bots` is empty). |
| `proposals_configured` | boolean | Whether GitHub is set up. When `false`, hide the propose button: proposals answer `501`. |

**Allowances** (one object per bot):

| Field | Type | Meaning |
| --- | --- | --- |
| `network[]` | array | One per endpoint, rule by rule. Anything not listed is denied. |
| `network[].rule` | string | The `network_policies` key, as OpenShell logs it. |
| `network[].service` | `"toolserver"`, `"gateway"`, `"runtime"` or `null` | Which PM OS service `host:port` is. **`null` is a destination PM OS doesn't know**: show it as a red flag (a hand edit). |
| `network[].host`, `network[].port` | string or `null`, integer or `null` | The destination. |
| `network[].ports`, `network[].allowed_ips` | arrays | Extra ports and IP ranges. Always empty in a generated policy; anything here is a hand edit. |
| `network[].protocol` | string or `null` | `rest` or `mcp` when inspected; `null` is not inspected. |
| `network[].enforcement` | string or `null` | `enforce`; anything else (`audit`, `null`) only logs. |
| `network[].tls`, `network[].access` | string or `null` | Always `null` in a generated policy (`skip` would relay TLS uninspected; an access preset is broad). |
| `network[].binaries` | string[] | The executables that may open these connections. |
| `network[].credential` | string or `null` | The provider instance bound to this endpoint. |
| `network[].routes` | string[] | REST: `METHOD path` allowed (in OpenShell's globs, `*` is one path segment). |
| `network[].mcp_methods`, `network[].mcp_tools`, `network[].mcp_versions` | string[] | MCP: the methods, the tools `tools/call` may name, and the protocol revisions. |
| `network_middlewares` | string[] | Always empty in a generated policy. |
| `filesystem.read_only`, `filesystem.read_write` | string[] | Paths inside the sandbox. Content is under `/sandbox/`. |
| `filesystem.include_workdir` | boolean | `true` would make the working directory writable too. |
| `landlock` | string or `null` | `hard_requirement`: the sandbox doesn't start without its filesystem rules. |
| `process.run_as_user`, `process.run_as_group` | string or `null` | The workload identity. |
| `providers` | string[] or `null` | The launch spec's `--provider` attachments; `null` without a launch spec. |
| `uploads` | string[] or `null` | The launch spec's read-only uploads, `from -> to` (`from` content-relative). |
| `summary` | string[] | The same, in sentences. |

**`Issue`** (here and in every validation below):

```json
{"severity": "error", "code": "role-skill-unknown", "path": "content/roles/docs-helper/ROLE.md",
 "line": null, "message": "skill `no-such-skill` is not in skills/"}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `severity` | `"error"` or `"warning"` | An error stops a proposal; a warning doesn't. |
| `code` | string | Stable: a content lint code (`docs/contract.md`; such as `skill-parse`, `role-skill-unknown`, `owner-unassigned`), a policy rule (`net-egress`, …), or a spec code (below). |
| `path` | string | A repository path (`content/…`, `policies/…`), or for a spec error a spec path (`spec.bots.docs.scope`, `spec.skills.docs.tools[2]`). |
| `line` | integer or `null` | 1-based line in the file, when the parser gave a position (front matter that doesn't parse). Usually `null`: most rules are about the whole file. |
| `message` | string | One sentence for the admin. |

### `POST /api/admin/policy/validate`

Runs the change on a scratch copy of the deployed content: applies the spec to the bot, role
and skill files, runs the content lint, regenerates every policy, checks each against the PM
OS rules, and compares with what's checked in. Nothing is saved.

Request: `{"spec": SpecPatch}`. A `SpecPatch` is any part of `spec`: leave out what doesn't
change. A field equal to the current value changes nothing.

```json
{"spec": {"skills": {"docs": {"tools": ["docs.search", "docs.read", "jira.search"]}},
          "bots": {"docs": {"owner": "ana"}},
          "roles": {"docs-helper": {"skills": ["docs"]}}}}
```

Unknown fields anywhere (`{"bots": {"docs": {"hosts": []}}}`) are `400 bad_request`: only
`role`, `scope`, `owner`, `skills` and `tools` exist.

Response, always `200` once the body parses (check `valid`):

```json
{"valid": true,
 "errors": [],
 "warnings": [
   {"severity": "warning", "code": "bot-reads-empty", "path": "content/bots/docs/BOT.md",
    "line": null, "message": "this team bot can read Jira, Confluence or Slack but lists nothing under `reads`, so it will read nothing there"}],
 "content_changes": [
   {"path": "content/skills/docs/SKILL.md", "change": "modified",
    "diff": "--- a/content/skills/docs/SKILL.md\n+++ b/content/skills/docs/SKILL.md\n@@ -6,7 +6,7 @@\n   version: \"0.1.0\"\n   model: \"extraction\"\n   data-class: \"public\"\n-  tools: \"docs.search docs.read\"\n+  tools: \"docs.search docs.read jira.search\"\n   surfaces: \"slack cli\"\n   ships-in: \"phase-0\"\n   track: \"Everyday\"\n"}],
 "policy_changes": [
   {"path": "policies/bots/docs.yaml", "change": "modified",
    "diff": "--- a/policies/bots/docs.yaml\n+++ b/policies/bots/docs.yaml\n@@ -100,6 +100,7 @@\n                 any:\n                   - docs_read\n                   - docs_search\n+                  - jira_search\n     name: pmos_toolserver\n process:\n   run_as_group: sandbox\n"}],
 "allowance_changes": [
   {"bot": "docs", "kind": "network.mcp_tool", "item": "jira_search", "before": null,
    "after": "jira_search", "direction": "wider", "widening": true,
    "description": "docs may now call the tool jira_search"}],
 "widening": true,
 "summary": {"wider": 1, "narrower": 0, "changed": 0}}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `valid` | boolean | No errors: it can be proposed. |
| `errors`, `warnings` | `Issue[]` | Every error in the changed tree, and the warnings that are new or on a changed file (the content's existing warnings elsewhere, such as unassigned owners, are left out). |
| `content_changes[]` | `FileDiff[]` | The content files the spec changes. |
| `policy_changes[]` | `FileDiff[]` | The generated files that change, compared with the checked-in ones (or, without a policies root, with what the deployed content generates). Empty when the changed tree doesn't lint. |
| `allowance_changes[]` | array | Each difference in what a bot may do (below). Empty: no sandbox changes. |
| `widening` | boolean | Any allowance change is `widening`. Show it loudly. |
| `summary` | object | Counts of `allowance_changes` by `direction`. |

`FileDiff`: `{path, change: "added" | "modified" | "deleted", diff}`, where `diff` is a unified
diff with three lines of context (`--- a/<path>`, `+++ b/<path>`, `/dev/null` for an added or
deleted side). Show it monospaced.

**Allowance changes:**

| Field | Type | Meaning |
| --- | --- | --- |
| `bot` | string | The bot (empty for `provider_profile`). |
| `kind` | string | `network.destination` (`host:port`, and extra ports or IP ranges), `network.binary`, `network.route`, `network.mcp_method`, `network.mcp_tool`, `network.mcp_version`, `network.middleware`, `network.protocol`, `network.enforcement`, `network.tls`, `network.access`, `network.credential`, `filesystem.read_only`, `filesystem.read_write`, `filesystem.include_workdir`, `landlock`, `process`, `credential.provider`, `upload`, or `provider_profile` (a shared provider profile file changes). |
| `item` | string | What it is about: a destination, a route, a tool, a path. Empty for bot-wide settings. |
| `before`, `after` | string or `null` | The old and new value (`null` on the side where it doesn't exist). |
| `direction` | `"wider"`, `"narrower"` or `"changed"` | Something added, something removed, or a setting changed. A new bot is all `wider`; a removed bot all `narrower`. |
| `widening` | boolean | `true` for `wider` and `changed`: **every widening is marked**, and a changed setting is treated as one for review. |
| `description` | string | One sentence (`docs may now call the tool jira_search`). |

**Spec error codes** (`path` is the spec path): `spec-unknown-bot`, `spec-unknown-role`,
`spec-unknown-skill` (it isn't in the content: add it as a content proposal), `spec-bad-name`
(a role, skill or tool name that isn't valid), `spec-bad-value` (a `scope` other than `team` or
`personal`, an `owner` that isn't a handle), `spec-duplicate`, `spec-too-long` (over 100 in a
list), `spec-file-missing`, `spec-unsupported-format` (the file's front matter has a layout the
editor can't change line by line, such as a folded multi-line value: change it as a content
proposal). With a spec error, the other arrays are empty.

### `POST /api/admin/policy/proposals`

Admins only. Request: `{"spec": SpecPatch, "title": "…", "reason": "…"}`.

- `title`: one line; control characters are dropped and whitespace collapsed; at most 120
  characters kept. Required. The pull request's title is `PM OS policy: <title>`.
- `reason`: free text for the reviewers; control characters other than newlines and tabs
  dropped; at most 4,000 characters kept. Required. It goes into the pull request body in a
  fenced block, so it can't format the page or mention anyone.

It validates again, then opens the pull request (`docs/policy-admin.md`, How a proposal is
opened). `201`:

```json
{"number": 42, "url": "https://github.com/acme/pmos/pull/42",
 "branch": "pmos/proposal/policy/adam-20261117-101509-d2aabc50",
 "base": {"branch": "main", "commit": "1111111111111111111111111111111111111111"},
 "files": ["content/skills/docs/SKILL.md", "policies/bots/docs.yaml"],
 "widening": true,
 "allowance_changes": [{"bot": "docs", "kind": "network.mcp_tool", "item": "jira_search",
   "before": null, "after": "jira_search", "direction": "wider", "widening": true,
   "description": "docs may now call the tool jira_search"}],
 "stale_check": "passed",
 "labels_added": true}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `number`, `url` | integer, string | The pull request. Link to it: review and merge happen there. |
| `branch` | string | Made by the server: `pmos/proposal/<kind>/<handle>-<yyyymmdd-hhmmss>-<random>`. |
| `base.branch`, `base.commit` | strings | What it was made on. |
| `files` | string[] | Every file the commit writes: the content files, then the regenerated policy files. |
| `widening`, `allowance_changes` | | As in validate. |
| `stale_check` | `"passed"` or `"skipped"` | `skipped` when GitHub cut the base's file list short (a very large repository): the base wasn't compared. |
| `labels_added` | boolean | `false` if GitHub refused the labels. The pull request is open regardless, and still listed below (by its branch). |

Errors, besides those above:

| Status and code | When |
| --- | --- |
| `400 bad_request` | The body doesn't parse, or the title or reason is empty after cleaning. |
| `400 no_change` | The spec changes nothing. |
| `422 invalid` | Validation found errors. The body also has `errors` and `warnings` (`Issue[]`). Nothing was sent anywhere. |
| `409 stale_base` | A file the pull request would write differs on the base branch from the server's copy. The body also has `paths` (string[]) and `base` (`{branch, commit}`). Nothing was written or recorded. Wait for the deployment to catch up, or make the change in GitHub. |
| `429 rate_limited` | Over this admin's proposals per hour (default 10). `Retry-After` gives the seconds. |
| `501 not_configured` | GitHub isn't set up on this server. |
| `502 audit_unavailable` | The tool server couldn't record the proposal, so nothing was written to GitHub. |
| `502 github_refused` | GitHub refused the token (it needs contents and pull requests write on the repository). |
| `502 github_unavailable` | GitHub didn't answer, couldn't find the repository or base branch, or refused the request (for example, the branch already exists). |
| `503 github_rate_limited` | GitHub's own rate limit. |

### `GET /api/admin/policy/proposals`

Open pull requests this feature made for policy: a branch under `pmos/proposal/policy/` or the
label `pmos-proposal:policy`. Read from GitHub on each request (the newest 300 open pull
requests). `501 not_configured` without GitHub.

```json
{"repo": "acme/pmos",
 "proposals": [
   {"number": 42, "url": "https://github.com/acme/pmos/pull/42",
    "title": "PM OS policy: Let the Docs bot search Jira",
    "branch": "pmos/proposal/policy/adam-20261117-101509-d2aabc50", "base": "main",
    "status": "open", "proposed_by": "adam",
    "base_commit": "1111111111111111111111111111111111111111", "author": "pmos-bot",
    "labels": ["pmos-proposal", "pmos-proposal:policy"],
    "created_at": "2026-11-17T10:00:00Z", "updated_at": "2026-11-17T10:00:00Z"}]}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `status` | `"open"` or `"draft"` | Only open pull requests are listed; merged and closed ones are in GitHub. Review state and checks are not read: link to the pull request. |
| `proposed_by` | string or `null` | The PM OS handle, read from the marker on the pull request body's first line; `null` if the body was edited. It is what the body says, not a verified fact: the audit log has the verified record. |
| `base_commit` | string or `null` | The base it was made on, from the same marker. |
| `author` | string or `null` | The GitHub login that opened it: the PM OS token's identity, not the admin. |
| `created_at`, `updated_at` | string or `null` | As GitHub gives them. |

## Admin: content and harness

The harness content (roles, bots, skills, playbooks, the context library, templates, reviewers,
master context) and what the running harness uses (`docs/content-admin.md`). The same
read-and-propose rules and deployed-revision caveat as **Admin: policy**.

| Route | `admin` | `auditor` | anyone else |
| --- | --- | --- | --- |
| `GET /api/admin/content` | yes | yes | `403 forbidden` |
| `GET /api/admin/content/file?path=` | yes | yes, but not a PM's own master file or `restricted` context (`403 forbidden`) | `403 forbidden` |
| `POST /api/admin/content/validate` | yes | yes | `403 forbidden` |
| `GET /api/admin/content/proposals` | yes | yes | `403 forbidden` |
| `POST /api/admin/content/proposals` | yes | `403 forbidden` | `403 forbidden` |
| `GET /api/admin/harness` | yes | yes | `403 forbidden` |

Reading and validating need `view-content`; proposing needs `edit-content`.

**Paths** are repository paths inside the content tree, such as `content/skills/docs/SKILL.md`.
A path is relative, `/`-separated, made of letters, digits, `.`, `_` and `-`, with no empty,
`.`, `..` or hidden (`.`-prefixed) component, at most 200 bytes and 8 deep. Anything else is
`400 bad_path`; anything outside `content/` (`policies/`, `deploy/`, `.github/`, code) is
`400 path_not_allowed`. Symbolic links are never followed (`400 bad_path`), and the tree never
lists one.

### `GET /api/admin/content`

```json
{"revision": {"commit": null, "source": null},
 "root": "content",
 "lint": {"errors": 0, "warnings": 93, "files_checked": 111,
          "checked_at": "2026-11-17T10:15:09.841Z", "other": []},
 "files": [
   {"path": "content/skills/docs/SKILL.md", "kind": "skill", "name": "docs", "title": null,
    "version": "0.1.0", "owner": "TODO", "sensitivity": null, "last_reviewed": null,
    "size": 1170, "editable": true, "readable": true,
    "lint": {"status": "warning", "errors": 0, "warnings": 1,
             "issues": [{"severity": "warning", "code": "owner-unassigned",
                         "path": "content/skills/docs/SKILL.md", "line": null,
                         "message": "owner is not assigned yet"}]}},
   {"path": "content/context/products/roadmap.md", "kind": "context", "name": null,
    "title": "Roadmap", "version": null, "owner": "adam", "sensitivity": "restricted",
    "last_reviewed": "2026-11-14", "size": 112, "editable": true, "readable": false,
    "lint": {"status": "ok", "errors": 0, "warnings": 0, "issues": []}}],
 "roles": [
   {"name": "docs-helper", "version": "0.1.0", "owner": "TODO", "model": "default",
    "data_class": "public", "skills": ["docs"], "bots": ["docs"]}],
 "skills": [
   {"name": "docs", "version": "0.1.0", "owner": "TODO", "model": "extraction",
    "data_class": "public", "tools": ["docs.search", "docs.read"], "used_by": ["docs-helper"]}],
 "bots": [{"name": "docs", "role": "docs-helper", "scope": "team", "owner": "TODO"}],
 "proposals_configured": true}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `revision` | object | As in **Admin: policy**. |
| `root` | string | The content tree's folder in the repository (`content`; empty if the content is the repository root). |
| `lint` | object | The content lint, run now on the deployed tree: counts, when, and `other` (`Issue[]` about a folder rather than a file, such as a skill folder with no `SKILL.md`). |
| `files[]` | array | Every regular file in the tree, sorted by path (no symlinks, nothing hidden). |
| `files[].kind` | string | `role`, `bot`, `skill`, `skill_file` (another file in a skill's folder), `playbook`, `template`, `reviewer`, `context`, `readme`, `master` (`master/AGENTS.md`), `personal_master` (`master/people/<handle>/AGENTS.md`), `team`, `tools` or `other`. |
| `files[].name`, `title`, `version`, `owner`, `sensitivity`, `last_reviewed` | string or `null` | From the file's front matter, where it has them (a skill's `version` and `owner` come from `metadata`). `null` when absent or unparsable. |
| `files[].size` | integer | Bytes. |
| `files[].editable` | boolean | Whether a proposal may change it: text (`.md`, `.yaml`, `.yml`, `.txt`) and not `team.yaml` (members are managed through onboarding). |
| `files[].readable` | boolean | Whether this viewer may open it (`GET …/file`). An auditor sees personal master files and `restricted` context listed, not opened. |
| `files[].lint` | object | `status` (`ok`, `warning` or `error`), counts, and that file's `Issue[]`. |
| `roles[]` | array | Each role: its skills, model role, data class, and the bots made from it. |
| `skills[]` | array | Each skill: its tools and `used_by` (the roles that list it). A skill that doesn't lint isn't here (its file shows the error). |
| `bots[]` | array | Each bot: role, scope, owner. |
| `proposals_configured` | boolean | As in **Admin: policy**. |

### `GET /api/admin/content/file?path=content/…`

```json
{"path": "content/skills/docs/SKILL.md", "kind": "skill", "size": 1170,
 "sha256": "7937723eb58e838aa8dbafc2e758c1163a2151b27d894414c04f47a34c45efa9",
 "text": "---\nname: docs\n…",
 "front_matter": {"name": "docs", "description": "…",
                  "metadata": {"owner": "TODO", "version": "0.1.0", "tools": "docs.search docs.read"}},
 "front_matter_error": null,
 "editable": true}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `sha256` | string | Of `text`, hex. Send it back as `base_sha256` with a change to this file. |
| `text` | string | The whole file, UTF-8. Files over 256 KiB, not UTF-8, or with NUL bytes are refused (`413 too_large`, `400 not_text`). |
| `front_matter` | object or `null` | A `.md` file's YAML front matter, parsed. `null` for other files, or when it's missing or doesn't parse. |
| `front_matter_error` | string or `null` | Why it's `null` for a `.md` file. |

Errors: `400 bad_path`, `400 path_not_allowed`, `400 not_a_file` (a folder), `404 not_found`,
`403 forbidden` (an auditor and a personal or restricted file).

### `POST /api/admin/content/validate`

Request: `{"changes": [Change]}`, 1 to 50 changes, each path once:

```json
{"changes": [
  {"path": "content/playbooks/bar.md", "text": "---\ntitle: The bar\n…", "base_sha256": "7937…"},
  {"path": "content/playbooks/new.md", "text": "---\ntitle: New\nowner: \"adam\"\nversion: \"0.1.0\"\n---\n\nBody\n"},
  {"path": "content/templates/old.md", "delete": true}]}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `path` | string | Required. An editable path (above). |
| `text` | string | The whole new file. At most 256 KiB, no NUL. Give either `text` or `delete: true`. |
| `delete` | boolean | Remove the file (it must exist). Folders left empty go too. |
| `base_sha256` | string, optional | The `sha256` from `GET …/file` when the admin opened it. If the server's copy has changed since, `409 stale_base`. Leave it out for a new file. |

A change identical to the deployed file is dropped. The response has the same shape as
`POST /api/admin/policy/validate` (`valid`, `errors`, `warnings`, `content_changes`,
`policy_changes`, `allowance_changes`, `widening`, `summary`): a content change that alters a
sandbox (a skill's tools, a role's skills, a bot's role, a new or removed bot) regenerates the
policies, and its widenings are marked the same way.

```json
{"valid": false,
 "errors": [
   {"severity": "error", "code": "role-skill-unknown", "path": "content/roles/docs-helper/ROLE.md",
    "line": null, "message": "skill `docs` is not in skills/"},
   {"severity": "error", "code": "skill-parse", "path": "content/skills/docs/SKILL.md", "line": 6,
    "message": "invalid front matter: did not find expected ',' or ']' at line 5 column 1, while parsing a flow sequence at line 4 column 10"}],
 "warnings": [],
 "content_changes": [{"path": "content/skills/docs/SKILL.md", "change": "modified", "diff": "…"}],
 "policy_changes": [], "allowance_changes": [], "widening": false,
 "summary": {"wider": 0, "narrower": 0, "changed": 0}}
```

Refusals, before anything is linted:

| Status and code | When |
| --- | --- |
| `400 bad_request` | The body doesn't parse (unknown fields included), a path is listed twice, or a change has both or neither of `text` and `delete`. |
| `400 bad_path` | A path that isn't plain (above), or one that goes through a symlink. |
| `400 path_not_allowed` | Outside the content tree, or `content/team.yaml`. |
| `400 not_text` | Not `.md`, `.yaml`, `.yml` or `.txt`, or text with a NUL byte. |
| `400 not_found` | Deleting a file that doesn't exist. |
| `400 no_change` | No changes, or every change equals the deployed file. |
| `409 stale_base` | A `base_sha256` doesn't match the server's copy now. |
| `413 too_large` | A file over 256 KiB. |
| `413 too_many_files` | More than 50 changes. |

The whole request may be up to 16 MiB.

### `POST /api/admin/content/proposals`

Admins only. Request: `{"changes": [Change], "title": "…", "reason": "…"}` (title and reason as
in **Admin: policy**). Validates again and opens a pull request with the changed content files
and any regenerated policy files. The `201` answer and the errors are those of
`POST /api/admin/policy/proposals`, for kind `content`: the branch is
`pmos/proposal/content/…`, the labels `pmos-proposal` and `pmos-proposal:content`, the title
`PM OS content: <title>`. A `409 stale_base` comes from either check: the admin's
`base_sha256` against the server's copy (message only), or the server's copy against the base
branch (with `paths` and `base`).

### `GET /api/admin/content/proposals`

As `GET /api/admin/policy/proposals`, for branches under `pmos/proposal/content/` or the label
`pmos-proposal:content`.

### `GET /api/admin/harness`

What the running harness uses, read-only.

```json
{"bots": [
   {"name": "docs", "role": "docs-helper", "scope": "team", "owner": "TODO",
    "model_role": "default", "gateway_alias": "pmos/default", "gateway_model": "qwen3.6-35b-a3b",
    "skills": ["docs"], "tools": ["docs.read", "docs.search"], "loaded_by_runtime": true}],
 "model_roles": [
   {"role": "default", "alias": "pmos/default", "model": "qwen3.6-35b-a3b", "upstream": "qwen",
    "workarounds": ["strip-think"]},
   {"role": "extraction", "alias": "pmos/extraction", "model": "nemotron-3-nano-30b-a3b",
    "upstream": "nemotron", "workarounds": []},
   {"role": "reasoning", "alias": "pmos/reasoning", "model": "qwen3.6-35b-a3b", "upstream": "qwen",
    "workarounds": ["strip-think"]}],
 "gateway_config": "ok",
 "runtime": {"reachable": true, "bots": ["docs"]},
 "content_revision": {"commit": null, "source": null}}
```

| Field | Type | Meaning |
| --- | --- | --- |
| `bots[]` | array | Every bot in the deployed content. |
| `bots[].model_role` | string or `null` | Its role's model role (`default`, `extraction`, `reasoning`); `null` if its role doesn't lint. |
| `bots[].gateway_alias` | string or `null` | What the harness asks the gateway for (`pmos/<model_role>`). |
| `bots[].gateway_model` | string or `null` | The served model name that alias maps to in the gateway config; `null` without the config, or when it has no such role. |
| `bots[].tools` | string[] | Registry names the bot may call (its role's skills' tools), sorted. |
| `bots[].loaded_by_runtime` | boolean or `null` | Whether the bot runtime lists it (`GET /v1/bots`); `null` when the runtime didn't answer. `false` means the runtime is running other content. |
| `model_roles` | array or `null` | Each gateway model role: the alias, the served model name, the upstream's **name** (never its address or a key) and its workarounds. `null` when `gateway_config` isn't `ok`. |
| `gateway_config` | `"ok"`, `"not_configured"` or `"unreadable"` | Whether `PMOS_WEB_GATEWAY_CONFIG` is set and parses. This is the server's copy of the gateway config, not what the running gateway loaded. |
| `runtime.reachable` | boolean | Whether the bot runtime answered. |
| `runtime.bots` | string[] or `null` | The bot names the runtime has loaded, sorted. |
| `content_revision` | object | As `revision` above: the server's, not the workers'. |

**Not here, because nothing exposes it to `pmos-web`:** worker health (the runtime has no
worker-status read for the `web` client) and the content revision each worker loaded (workers
don't report one).
