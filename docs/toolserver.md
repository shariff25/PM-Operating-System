# Tool server

`pmos-toolserver` (Rust, `crates/toolserver`) is the only path from an agent sandbox to the
context library, memory, and the systems of record: Jira and Confluence (Atlassian Cloud) and
Slack. The sandbox network policy allows only this server and the model gateway. The team
service and personal agents go through the same server, so they follow the same rules
(MOD-3).

## Rules it enforces

| Rule | How | PRD |
| --- | --- | --- |
| A skill calls only the tools it declares | `tools/list` returns the session skill's tools only; any other call is refused and audited as `denied` | Authorization |
| A bot calls only its role's tools, and never a `never` action | A bot session's `tools/list` is its role's tools less any write set to `never`; anything else is refused and audited as `denied` (see **Bot sessions**) | D7 |
| A skill reads nothing above its data class | `context_search` drops results above the skill's `data-class` (a bot session's role's); `context_read` refuses them | Data classes |
| A team bot reads only what its owner chose | Its Jira, Confluence and Slack reads use its own account and stay inside its `reads` list; anything else is `403`, audited as `denied` | Decision 26 |
| External content is data, not instructions | Every Jira, Confluence and Slack item comes back wrapped as `{"untrusted": true, "ref": …, "source": …, "content": …}` (`ref` is what a draft's source marker names it by; see **Source markers**); `bets_list` and `bets_read` results carry `"untrusted": true` and a note too | Untrusted content |
| A note is as sensitive as what it was built from | A memory note stores the highest data class its writing session had read, and reading it raises the reader to that (see **Memory**) | Data classes |
| The Slack bot's token can't act as everyone | It names the PM it acts for unsigned, so it reads one approval item only for that PM, sees no approval ids in the ledger, can't edit where a draft goes, and can't make an admin (see **Who holds which token**) | — |
| Nothing is written outside PM OS without approval | A write runs only with a matching grant or an inbox approval by its own user; otherwise it returns `pending_approval` and does nothing | OUT-4 |
| Everything is audited | One event per call and per decision: user, skill, run, tool, outcome, sources read (with a short title for Jira, Confluence, docs and context items, never Slack), argument hash, output reference. Never message bodies | ADM-2, SLK-8 |
| Arguments match the tool's schema | Required keys, no unknown keys, types and enums checked before any handler runs | — |
| A bad draft gets one correction | A write tool refused twice for invalid arguments in a session is refused for the rest of it, without reaching the connector (see **Invalid drafts**) | — |
| Every draft is labelled | Each approval item has a row in the feedback ledger: what became of it, never its text (see **Feedback ledger**) | — |
| A Slack post reaches only who it may | Before a Slack write runs, its channel's members are checked against the user database and the draft's sources: restricted sources never reach outsiders, and a Slack Connect channel needs a second confirmation (see **Slack drafts**) | SLK-6 |
| No Slack text past a day | An undecided Slack draft expires after 24 hours; a decided one loses its text 24 hours after the decision (see **Slack drafts**) | SLK-8 |

## How a write gets approved

```
harness ──tool call──► interruptOn pause ──► PM approves / edits / rejects (Slack card or web)
                                                   │ approve or edit
                                                   ▼
                     orchestrator ──POST /internal/grants {session, tool, args}──► tool server
harness ──same call (edited args if edited)──► tool server: grant matches → runs once
```

- A grant matches one exact call: the same session, the same tool, and the same arguments
  (the SHA-256 of their canonical JSON, so key order doesn't matter). It is single-use and
  expires after 15 minutes.
- A write with no grant never runs. This covers a personal agent without approval pauses, a
  model that skips the pause, or edited arguments that don't match. The write becomes a
  pending item in the PM's inbox instead.
- Only the item's own user can approve it there (`403` for anyone else). Approve and edit
  run the write as that user.
- **Edits are checked before anything changes.** Edited arguments are checked against the
  tool's schema like any call (`400` if they fail), and the item's skill must still declare
  the tool (`409` if not). A refused decision leaves the item pending.
- **An edit from the Slack bot can't change where a draft goes.** Its edited arguments must
  keep these as drafted, or the decision is `422 {code: "target_changed", error}`, audited as
  `denied` with detail `target_changed: …`, the item still pending:
  `slack.post_reply`: `channel`, `thread_ts`, `message_ts`; `slack.send_message`: `channel`;
  `confluence.create_draft`: `space`, `parent_id`; `confluence.update_page`: `page_id`,
  `version`; `jira.create_issue`: `project`, `issue_type`, `epic`; `bets.record`: `key`. An
  optional one absent from both is unchanged; adding one is a change. The list is
  `EDIT_IMMUTABLE_ARGS` in `src/schemas.rs`. The orchestrator's edits are not limited.
- **A saved edit** (`PUT /internal/approvals/{id}/draft`, the web app's **Save**): the item's
  own PM changes a pending draft's text without deciding it. Its arguments become the edited
  ones (with a new `args_hash`), so the Slack card, the console and the web app all show the
  edit, and an `approve` from any of them writes it. The same rules as an edit decision: the
  owner only, pending only (a Slack draft past its time expires first), arguments checked
  against the tool's schema, and no change to where the draft goes (`422 target_changed`,
  audited `denied`) from any client but the orchestrator. Source markers may stay in the saved
  text; they are removed when it is written. It is audited as `approval.draft_saved`, outcome
  `ok`, with the new args hash and the item id, never the text. The bot's own arguments are
  kept as `drafted_args` (never served) for the ledger: an approval of a saved edit counts as
  `edited`, with the edit distance from the bot's text, and its audit `detail` is `edited
  before approval`. Saving the bot's own text back clears the edit. When a Slack draft expires
  or loses its text, the saved edit and the bot's text go with it. The item answers `edited`
  (`true` once a person changed the bot's arguments, by a saved edit or an edit decision) and
  `edited_at` (the last save).
- **A saved edit can raise the draft's sensitivity, never lower it.** Text a bot wrote in a
  separate revision run (the web app's accepted suggestion) names that run:
  `revision_run` (its runtime task id) and, optionally, `revision_sensitivity` (the level the
  runtime recorded for it). The item's `sources_sensitivity` becomes the highest of its own
  (unrecorded counts as `restricted`), the level this tool server's own sessions for that run
  reached (`restricted` when it has none), and `revision_sensitivity`, which counts only as a
  floor. It is worked out under the item's row lock, so concurrent saves can't lower it. The
  audit `detail` says `a bot revision's text; sources internal -> restricted`. Text a person
  typed (no `revision_run`) carries no level the machine knows: the item keeps its own. So a
  revision that read a restricted document makes the draft restricted, and an approve for a
  channel with someone outside the team is `409 audience_blocked`.
- **What the person was shown.** A decision may carry `expected_args_hash`: the start (16 to
  64 lowercase hex characters) of the `args_hash` of the draft the person saw. If the item now
  holds other arguments (a save landed in between), the decision is `409 {code: "stale"}`,
  audited `denied` with detail `stale: …`, and nothing changes; the store checks it again
  under the decision's row lock. It is **required on `approve` from the Slack bot and
  `pmos-web`** (`400 {code: "expected_args_hash_required"}` without it, `400 {code:
  "bad_expected_args_hash"}` when malformed) and optional for the orchestrator.
- **An internal failure** (PM OS itself failed running an approved write, not the target
  system) answers `500` with a generic message; the detail goes to the tool server's log.
- **An item moves through states:** `pending`, then `executing` while the write runs, then
  `approved` (with the result) or `failed`. A pending Slack draft nobody decides becomes
  `expired` (see **Slack drafts**). `approved`, `rejected`, `failed` and `expired` are closed.
  - The write runs in its own task, so an approver who disconnects can't cancel it halfway.
  - If the process dies mid-write, the item stays `executing`. On the next start, and every
    10 minutes for anything stuck longer than that, it becomes `failed` with "it may or may
    not have happened: check Jira before trying again". Nothing is retried automatically.
- **No write runs unless the audit log records it starting** (outcome `started`). If that
  record fails, the write doesn't run, and a harness grant is put back.
- **A timeout or 5xx on a write** is reported as "may or may not have happened", never as
  "unavailable", so neither the model nor the PM retries into a duplicate.

### What an item is shown with

For the web app (`crates/web`), each item as the internal API answers it carries four more
fields, which older readers ignore. None needs a network call when the item is read:

- `expires_at`: for a Slack draft, `created_at` plus `PMOS_SLACK_DRAFT_TTL_HOURS`.
- `target_url`: a link to where the write goes, built only from target ids that pass a
  shape check (`sources.rs`): a Confluence parent page or space, the page being updated, a
  Jira epic or project on the PM's linked Atlassian site (read from the stored
  credential's site fields, never its token), a Slack thread or channel
  (`https://slack.com/archives/…`). Left out when no link can be built.
- `external`: for a Slack draft whose channel's audience has been checked (on an approve),
  whether the channel is shared outside the company. Kept in memory per item and channel:
  a restart forgets it, and it is left out until the next check.
- `target_channel`: for a Slack draft, its channel as Slack last described it (see **Slack
  draft targets**): `{kind, name, with, checked_at}`. Left out until the channel has been
  read, and when the item's `channel` is no longer the one read.
- `revision` and `recommendations` (slice U6a): the version of the draft's text, and the
  reviewers' live recommendations on it, each marked `stale` when given on an earlier
  version; the note never reaches the Slack bot or the console. See **Reviews**.

**Slack draft targets.** So the web app can say "#launch-acme" rather than "A Slack thread",
the tool server records what `conversations.info` (the PM's own token) says of a Slack
draft's channel: once when the draft is made (best effort: if Slack can't say, the draft is
made anyway and reads without it), and again at every audience check, so a renamed channel
reads its new name after the next approve or edit. It is stored on the item
(`approvals.target_conversation`, a nullable `jsonb`, migration 0014) as `{channel, kind,
name?, peer?, checked_at}` and served as `target_channel`:
- `kind`: `channel`, `private_channel` (`is_private`, or an old `G…` group), `im` or `mpim`.
- `name`: a channel's or private channel's name without `#`, only if it is shaped like one
  (letters, digits, `-`, `_`, `.`, at most 80; `connectors::channel_name`), else `null`.
  Names are metadata, not message text, but they come from Slack and are treated as untrusted.
  A DM's or group DM's `name` is never kept.
- `with`: for a DM, the other person's handle in the user database (as last read) when their Slack id (`user`, kept
  only if shaped like a member id) belongs to a member; `null` otherwise. No extra lookup.
- `checked_at`: when it was read.
It is kept when a draft's text is removed: it holds no message text.

`GET /internal/approvals/{id}/sources?user=` lists what the drafting run read, from the
audit log: `user`'s successful read-tool calls with the item's `run_id`, at or before the
item's `created_at`. Each source appears once (its latest read), newest first, at most 20
(`truncated` says more were read); at most 500 audit events are scanned. It is metadata
only, as the audit log is: no message text, page body, snippet or excerpt is stored or
returned.

| Audit `sources` entry | `system` | `title` | `url` |
| --- | --- | --- | --- |
| a context path (context tools) | `context` | the file's title in the context index, else the title recorded at the read, else the path | `null` |
| `jira:PAY-212` | `jira` | `PAY-212: {summary}` with the recorded summary, else `PAY-212` | `{site}/browse/PAY-212` |
| `jira:rollup:status` | `jira` | `Jira rollup by status` | `null` |
| `confluence:12345` | `confluence` | the recorded page title, else `Confluence page 12345` | `{site}/wiki/pages/viewpage.action?pageId=12345` |
| `slack:C01:1731.0001` | `slack` | `Slack thread in C01` or `Slack message in C01` (the channel id); never a recorded title | `https://slack.com/archives/C01/p17310001` |
| `docs:https://…/page.md` | `docs` | the recorded page title, else the page's slug | the docs URL |

**Source titles in the audit log.** So that a PM sees "PAY-212: Grace period setting" rather
than "PAY-212", each successful read records, next to its `sources`, a short title for each
item it returned, in `source_titles`: a map from the `sources` entry to the title.

- **What:** a Jira issue's summary (without the key, which the entry already has), a
  Confluence page's title, a docs page's title, a context file's title from the index. Only
  for these four systems (`audit::TITLED_SYSTEMS`); not for a Jira rollup.
- **How much:** control characters (line breaks included) become spaces, runs of white
  space one space, at most 120 characters (`audit::source_title`). Never a body, a
  description, a comment, a snippet or an excerpt.
- **Never Slack.** A Slack read records only `slack:{channel}:{ts}`, as before: no title,
  channel name or text, because Slack text may be kept at most 24 hours (SLK-8, D6) and the
  audit log is kept for good. The connector's Slack "titles" are built from the channel
  and the message, so none is recorded, and the sources endpoint ignores any it finds.
- **Why it is metadata:** a title names the thing that was read, like the key or page id
  already in `sources`; it is what the PM sees as a link in Jira or Confluence anyway. It
  is as sensitive as the item's existence, and the audit log is already readable only by
  platform owners (the Postgres table, the JSON-lines file) and, for one item, by the item's
  own user through this endpoint.
- **Where:** a nullable `jsonb` column `audit_events.source_titles` (migration 0010, which
  adds the column without a default, so no row is rewritten and the append-only triggers
  stand), the in-memory log, and a `source_titles` object in each JSON-lines line, left out
  when empty. Events recorded before 0010 have none, and their sources read as ids, as
  before.

**Source markers.** Each source also has `cite`: `src-` and the first six hex characters of
the SHA-256 of its audit `sources` entry (`sources::cite_ref("jira:PAY-212")`, a context
path for a context file). Every item a read tool returns carries the same value as `ref`
(the connectors' `untrusted` wrapper, and `context.search`/`context.read` results), so a bot
can end a sentence of a draft with `[[src-3f9a1c]]` to say which source it came from
(`content/roles/pm-assistant/ROLE.md`; the grammar is in `crates/contract/src/cite.rs`).
The ref is made from ids only and needs nothing stored; the web app resolves markers
against this list, so only sources the run really read can be cited (`docs/web-api.md`,
Citations).

**Markers never reach a target system.** Every approved write runs through
`ToolService::execute_write`, which removes markers from every string argument
(`cite::strip_json`) before the connector (or the bets register) sees them: on an inbox
approve, an edit and a harness grant alike, including an edit from the Slack bot or
`pmos-web` that keeps every target argument (a page update's `page_id` and `version`) as
drafted: the target check compares the arguments as stored, and only then are markers removed
from what is written. Text inside code spans and fenced code is left
as written. The stored item keeps its markers (they are what the PM reviewed), so its
`args_hash`, the grant match and the web app's `version` are unchanged. The Slack approval
cards and the console show the arguments with markers removed too.

Limits: Jira and Confluence links need a linked Atlassian site (else `url` is `null`); a
read that found nothing, failed or was refused records no sources, so it isn't listed; a
reference of an unknown shape is left out; memory, the bets register and the stats tools
record no sources; and with only a JSON-lines audit file (no Postgres, no in-memory log)
the log can't be read back, so the answer is `available: false` with no sources.

## Endpoints

| Endpoint | Auth | Purpose |
| --- | --- | --- |
| `GET /healthz` | none | Liveness: the process answers |
| `GET /readyz` | none | Readiness: `200` when storage answers within a second and every connector's own dependency (the docs server) answers; `503` otherwise, naming the check that failed |
| `POST /mcp` (Streamable HTTP) | `Bearer <session token>` | MCP for the harness: `tools/list`, `tools/call` |
| `POST /internal/sessions` `{user, skill, run_id, ttl_minutes?}` | service or bot token | Create a session; returns `token` and the session's tools. `403` if the user isn't an active user holding `pm`, `reviewer` or `admin` in the user database, or the bot asks for a skill outside `PMOS_BOT_SKILLS`. |
| `POST /internal/sessions` `{user, bot, run_id, ttl_minutes?, history_sensitivity?, persona?}` | service token | Create a bot session (see **Bot sessions**). Send `skill` or `bot`, never both (`400`). `404` for an unknown bot; `403` if `user` may not use it. `history_sensitivity` (`public`, `internal` or `restricted`; anything else is refused) starts the session at that level: see **Conversation history**. `persona` runs the session as a marketplace bot (`400` with `skill`); refusals are `403 {code, error}` with `persona_needs_personal_bot`, `persona_unavailable` or `persona_not_on_team`, and the answer adds `persona: {key, display_name, title, role}` (`null` for Nudge): see **Personas and teams**. |
| `GET /internal/runs/{run_id}/sensitivity` | service token | `{run_id, sensitivity}`: the highest level any stored session of that run reached, or `null` when none is stored (never made, or purged after it expired). The runtime records it on each finished task. |
| `DELETE /internal/session` + `X-Pmos-Session` | service or bot token | Revoke a session (sandbox ended) |
| `POST /internal/call` + `X-Pmos-Session` `{tool, args}` | service or bot token | One tool call for the session, with the same rules as MCP: allowlist, argument checks, approvals for writes, audit (which records the caller). |
| `POST /internal/grants` `{session_token, tool, args}` | service token | Record an approval given in the harness |
| `GET /internal/approvals?user=&status=` | service, web or console token | `{items: […], truncated, total_pending}`: a PM's inbox, newest first (ties by id). **Every pending item is listed**: the cap of 200 (`approvals::LIST_LIMIT`) applies only to items that aren't pending. Past a much higher safety cap on pending items (1,000, `approvals::PENDING_CAP`) the newest are listed with `truncated: true`, and `total_pending` counts them all (it is logged too). With `status=pending` only the pending ones; with another `status` (`executing`, `approved`, `rejected`, `failed` or `expired`; `400` for anything else) `{items}`, at most 200 (an older item is still read by its id) |
| `GET /internal/approvals?user=` | bot token | The App Home Inbox: `{approvals: […], total}`, only `pending` items whatever `status` says, oldest first, at most 50. Each item reads as `GET /internal/approvals/{id}` returns it; `total` counts every pending item. `403` if `user` isn't a team member (live connectors) or is a team bot's identity; `400` without `user` |
| `GET /internal/approvals/{id}` | service or console token | One item (from the console: under `require` only with a user assertion, and then only the subject's own unless the subject is an active admin in the user database; anything else is `404`): `id`, `user`, `skill` or `bot`, `run_id`, `tool`, `args`, `args_hash`, `status`, `created_at`, and when set `decided_at`, `decided_by`, `result`, `sources_sensitivity`, `redacted_at` and `edited_at`; always `edited` (whether a person changed the bot's arguments); plus, when known, `expires_at`, `target_url` and `external` (see **What an item is shown with**). Listings and decision answers carry the same fields |
| `GET /internal/approvals/{id}?user=<handle>` | bot or web token | The same item, for the PM the client acts for. `404 {error: "no such approval item"}` without `user`, when `user` isn't a team member, when the item isn't `user`'s, or when there is no such item: the client can't tell which ids exist. With the web token, the user assertion must name `user` |
| `PUT /internal/approvals/{id}/draft?user=<handle>` `{args, revision_run?, revision_sensitivity?}` | service or web token | Save `user`'s edit of their pending item (see **How a write gets approved**, "A saved edit"; with `revision_run`, the item's level is raised to what that run read: "A saved edit can raise the draft's sensitivity"); answers the item as `GET /internal/approvals/{id}` does, with `edited` and `edited_at`. With the web token the user assertion must name `user` (as for the other approvals routes); the bot and console tokens get `403`. `404` without `user`, for someone else's item or a missing one; `409 {code: "not_pending"}` once decided, `409 {code: "expired"}` for an expired Slack draft; `400` for arguments that don't fit the tool; `422 {code: "target_changed"}` for a change of where it goes; `403` when `user` isn't an active PM OS user |
| `GET /internal/approvals/{id}/sources?user=` | service or web token | What the run that drafted the item read: `{sources: [{system, title, url, ref, cite, tool, read_at}], available, truncated}`. `user` is required (`400`) and must be the item's user: anyone else's item, or a missing one, is `404`. See **What an item is shown with** |
| `POST /internal/approvals/{id}/decision` `{decided_by, decision, args?, reason?, confirm_external?, expected_args_hash?}` | service, bot or web token | `approve`, `edit` (with `args`) or `reject`. `expected_args_hash` is the start of the `args_hash` the person was shown: a mismatch is `409 {code: "stale"}` and nothing is decided; it is required on `approve` from the bot and web tokens (`400 {code: "expected_args_hash_required"}`), optional for the service token (see "What the person was shown"). `decided_by` must be the item's user (`403`), and an active PM OS user (`403`; a deactivated owner's drafts are held this way). `reason` is optional and only with `reject` (`422` with anything else, or for an unknown value): `wrong_facts`, `wrong_target`, `tone`, `incomplete`, `should_not_draft` or `other` (`owner_deactivated` is the system's own, never accepted from a client). The audit records which token decided, and the reason in `detail` (`reason: tone`). For a Slack write, `confirm_external: true` confirms posting to a Slack Connect channel. Slack answers, each `{code, error, …}`: `409 audience_blocked` (with `outsiders`, `members`, `external`), `409 needs_external_confirm` (with `members`, `external`), `503 audience_unavailable`, and `409 expired`; see **Slack drafts**. From the bot or web token, an `edit` that changes where the draft goes is `422 target_changed` (see **How a write gets approved**). An internal failure is `500` with a generic `error`. |
| `GET /internal/ledger?since=&until=&bot=&outcome=&min_edit=&limit=` | service or bot token | `{rows: […]}`, newest first; for the bot token, rows have no `id` (use `run_id`). See **Feedback ledger**. `422` for a bad parameter |
| `GET /internal/ledger/summary?since=&bot=` | service token | Per bot (or skill) and tool: counts by outcome, label coverage, median edit distance, rejection reasons. `422` for a bad `since` |
| `POST /internal/reindex` | service token | Rebuild the context index from the content checkout and re-read the bots and their roles (users and roles are in the database); `422` if it fails `pmos lint` |
| `GET /internal/oauth/{atlassian\|slack\|github}/start?user=` | service or bot token | The URL to send a PM to so they can link their account. The bot token gets URLs for team members only (`403` otherwise). `user=bots.<name>` (service token) gives the URL for a team bot's own account (`404` unless `<name>` is a team bot); GitHub links PMs only. |
| `GET /oauth/{atlassian\|slack\|github}/callback` | none (the PM's browser) | Finishes linking; shows a short page |
| `GET /internal/credentials?user=` | service token | A PM's linked accounts, without tokens |
| `DELETE /internal/credentials/{user}/{provider}` | service or bot token | Unlink (the PM disconnects, or a platform owner revokes): `204`, or `404` if nothing was linked. The bot token may unlink team members' accounts only (`403` otherwise, never a team bot's own), for the App Home's **Disconnect**. Audited as `credentials.unlink` with the client, `output_ref` the provider |
| `POST /internal/memory/repo` `{user, repo}` | service or bot token | Check `repo` (`org/name`) with the PM's GitHub token and record it as their memory repo: `200 {repo, verified_at}`. `422 {error, code}` with `not_in_org`, `not_private`, `archived`, `no_push`, `not_found` or `bad_request`; `409 github_not_linked`; `504 github_unavailable` if GitHub doesn't answer within 1.8 s (it runs inside a Slack modal); `404 github_off` without GitHub |
| `GET /internal/memory/repo?user=` | service or bot token | `{repo, verified_at}`, or `404` |
| `POST /internal/members/requests` `{slack_id, email, name}` | service or bot token | A request to join: `201 {request}`, or `200 {request}` with the one already pending for that Slack id. `409 already_member`, `422 bad_request`, `429 too_many_pending` (past 20) |
| `GET /internal/members/requests?status=` | service or bot token | `{requests: […]}`, oldest first; `status` is `pending`, `approved`, `rejected` or `failed` |
| `POST /internal/members/requests/{id}/decision` `{decision, decided_by, handle?, roles?}` | bot token only (the service token gets `403`: review of `b469d5b`, UM1) | `approve` or `reject`, by an active `admin` in the user database (`403 not_admin`). `roles` may be `pm` only (the token is unsigned): `admin` is `403 admin_not_from_slack`, any other `403 role_not_from_slack`, audited as `denied`, the request still pending. See **Members**. `200 {request}` (the user is created first; when the personal bot couldn't be committed, `commit` is null and `error` says why); `409 already_decided` with `request`; `422 bad_handle`; `409 already_member` |
| `POST /internal/members/{handle}/remove` `{decided_by}` | bot token only (UM1) | Admin only; not oneself (`403 self_removal`). `200 {handle, commit}`, `404 not_a_member`, `409 owns_bots` if they still own a team bot |
| `GET /internal/onboarding?user=` or `?slack_id=` | service or bot token | A PM's setup checklist: `member`, `handle`, `admin`, `linked` (`atlassian`, `slack`, `github`), `memory_repo`, `personal_bot` (in the loaded content), and for a non-member by Slack id, `pending_request` |
| `GET /internal/admin/audit?user=&before=&limit=&actor=&action=&outcome=&from=&to=&purpose=` | service or web token | The whole audit log, newest first: `{events: [...], next}`. Needs the viewer `user` to have `view-audit-log` (`export-audit` with `purpose=export`). See **Admin reads** |
| `GET /internal/admin/accounts?user=` | service or web token | `{linking, members: [{handle, accounts: {atlassian, slack, github}}]}`, booleans only. `view-audit-log` or `manage-members` |
| `GET /internal/admin/connectors?user=` | service or web token | `{connectors: [{name, mode, health, error_class, checked_at}]}`, checked now. `manage-connectors` |
| `GET /internal/admin/overview?user=` | service or web token | `{since, pending_drafts, held_drafts, approved, rejected, denied, retention_days, oldest_event_at, last_purge, users}`: pending items across the team now, audit outcomes over 7 days, audit retention (see **Audit retention**) and the user database's flags (below). `view-audit-log` or `manage-members` |
| `POST /internal/admin/proposals/open?user=` `{kind, title, reason, base_sha, changes}` | web token **and** a signed assertion for `user`, in every mode | Check an admin's policy or content change again and open it as a pull request with their own linked GitHub token. `201 {number, url, author, branch, base, files, widening, allowance_changes, stale_check, labels_added}`. See **Admin proposals** |
| `GET /internal/admin/proposals?user=&kind=policy\|content` | the same | Open proposals, read with the viewer's own GitHub token: `{repo, github: "linked", proposals: [...]}`, or `{repo, github: "not_linked", proposals: []}`. `view-policies` / `view-content` |
| `GET /internal/admin/protection?user=` | the same | `{configured, branch, required_reviews, verified, admins_enforced, bypass_allowed, source?, github}`: whether `PMOS_CONTENT_BRANCH` requires reviews, as the viewer's token can read it. `verified` is `true` only when reviews are required and the answer showed admins bound (`enforce_admins`, or a ruleset with no bypass actor) and nobody on a bypass list, or when no review is required at all; a setting the answer doesn't expose is `null` and never verified (UM4). Best effort, never an error. `view-policies` or `view-content` |
| `POST /internal/admin/actions?user=` `{action, outcome, detail, count, target?}` | the same | `pmos-web` records an admin refusal or an admin's sensitive read: `200 {recorded: true}`; `503 audit_unavailable` if it can't be recorded (pmos-web then serves nothing) |
| `POST /internal/admin/events[?user=]` `{action, provider, reason?, attempt?, member?}` | web token only | A sign-in event from `pmos-web`: `web.sign_in` or `web.sign_out` (for the member named as `user=`, with a user assertion for them), `web.sign_in_refused` or `users.identity_conflict` (no `user=`). Fixed values only. See **Sign-in events** |
| `GET /internal/directory` (+ `If-None-Match`) | service or bot token | Every user in the user database, `{version, users: [...]}`, with `ETag: "<version>"`; `304` when it matches. See **The user database's read API** |
| `GET /internal/users/resolve?by=entra&tid=&oid=` / `?by=sign_in_name&name=` / `?by=handle&handle=` | web or console token, no assertion | One user, or `404`; `429 resolve_misses` past 20 misses a minute, `429 resolve_rate` past 600 hits a minute. See **The user database's read API** |
| `POST /internal/users/{handle}/identities/entra` `{id_token, nonce}` | web token, no assertion | The first Entra link, from an ID token this server validates itself; `GET /internal/users/entra-link` (web token) says for which tenant and client. See **The user database's read API** |
| `GET /internal/admin/users?user=` | web token (assertion as on the admin reads) | Every user, as an admin or an auditor may see them. See **The Admin API** |
| `POST /internal/admin/users`, `PUT …/{handle}/roles`, `PUT …/{handle}/fields`, `POST …/{handle}/sign-in-name/confirm`, `POST …/{handle}/deactivate`, `POST …/{handle}/reactivate`, `DELETE …/{handle}/identities/{kind}`, `PUT /internal/admin/reviewers/{reviewer}` (all `?user=<admin>`) | web token **and** a signed assertion for `user`, and assertion `require` (else `503 assertion_required`) | User and role writes. See **The Admin API** |
| `GET /internal/console/members?user=` | console token (assertion as on its approvals reads) | `{handles}` for an active admin's Approvals screen. See **The Admin API** |
| `GET /internal/personas` | service, bot or web token (no assertion) | `{catalog_sha, personas}`: the offered marketplace bots, Nudge first. See **Personas and teams** |
| `GET /internal/team/{handle}` | web token **and** a signed assertion for `handle`, in every mode | `Team`: the person's own team. See **Personas and teams** |
| `PUT /internal/team/{handle}` `{add?, remove?, via}` | the same, and assertion `require` (else `503 assertion_required`) | Change one's own team, atomically. See **Personas and teams** |
| `GET /internal/admin/personas?user=` | web token (assertion as on the admin reads) | `AdminPersonas`: who uses which bot. `view-audit-log` or `manage-members`. See **Personas and teams** |
| `POST /internal/admin/personas/drafts/{id}/release?user=` | web token, a signed assertion for `user`, and assertion `require` | An Admin (`edit-content`) releases a draft held because its bot was withdrawn. See **Personas and teams** |
| `GET /internal/reviews?user=`, `GET /internal/reviews/{id}?user=` | web token **and** a signed assertion for `user`, in every mode | A reviewer's inbox and one draft of a PM assigned to them; metadata only when restricted. See **Reviews** |
| `POST /internal/reviews/{id}/recommendation?user=` `{verdict, note?, draft_revision, args_hash}` | the same, and assertion `require` (else `503 assertion_required`) | A reviewer's recommendation, bound to the version read (`409 draft_changed`). Never decides anything. See **Reviews** |

### Admin reads

`app/admin.rs`. The read-only routes behind `pmos-web`'s admin area and auditor view
(`docs/web-api.md`, **Admin and auditor**). Only `GET`; only the service and web tokens (the
bot and console tokens get `403`).

- **The viewer is checked here.** Every call names its viewer as `user=` (else `400`), and the
  viewer's roles in this server's user database must grant the route's permission
  (`pmos_contract::permissions`), else `403 {"code":"forbidden"}`, whatever the caller already
  checked. With the web token, the user assertion must name that same person, as on the
  approvals routes (`PMOS_ASSERTION_MODE`).
- **Metadata only.** An event is served as `{id, at, actor, client, action, system, target,
  skill, bot, task_id, outcome, detail, sources, titled_sources}`: `sources` and
  `titled_sources` are counts (titles can carry content), `target` is `output_ref`, and
  `detail` only when it is one of the fixed notes this server writes itself (`safe_detail`);
  a rejection's free text, a refusal's or connector's message is `null`. `args_hash` and the
  source list are never served.
- **Paging:** `limit` 1 to 1000 (default 100); `next` is an opaque cursor (`c1.` and base64url
  of the last event's time and id), passed back as `before`. `400 bad_request` for a bad
  parameter. `501 not_available` when the audit log can't be read back.
- **Audited:** each call records `admin.audit`, `admin.audit_export`, `admin.accounts`,
  `admin.connectors` or `admin.overview` with the viewer, the client, `run_id` `admin`, and
  `ok` or `denied`; no query values.
- Migration 0011 widens `audit_events.client`'s check to `web` and `console` (Postgres refused
  events from those clients before) and indexes `(at, id)` for the search.

### Admin proposals

`src/proposals.rs`, routes in `app/admin.rs`; the shared checks in `pmos_policy::admin`
(`docs/policy-admin.md`, **How a proposal is opened**). `pmos-web` holds no GitHub token: it
validates a policy or content change for the admin and sends the change set here; this server
checks it again from nothing and opens the pull request **as the admin**.

- **Who may call:** the web token only (the service, bot and console tokens get `403`), with
  `pmos-web`'s signed user assertion for the `user=` it names, bound to the exact request and
  body, **in every assertion mode, `off` included** (`401 bad_assertion` otherwise). The
  subject must be an admin in this server's user database (`edit-policies` or `edit-content` by
  kind), else `403 forbidden`.
- **The checks** (`changeset::recheck`): the path allow-list (content files under
  `PMOS_CONTENT_DIR`, and only the generated `bots/<bot>.yaml`, `bots/<bot>.sandbox.yaml` and
  `providers/<profile>.yaml` under `PMOS_POLICIES_DIR`; never `.github/`, `deploy/`, code,
  `team.yaml`, a hidden file or the repository root); 50 content and 150 policy files, 256 KiB
  each, text only; a policy proposal touches only bot, role and skill files; each file's
  `base_blob_sha` against this server's own content checkout (`409 stale_base`); the content
  lint and the policy rules on the changed tree (`422 invalid`); the policy files exactly as
  this server regenerates them (`422 policy_mismatch`); then each file against the head of
  `PMOS_CONTENT_BRANCH` (`409 stale_base` with `paths` and `base`).
- **As whom:** the admin's GitHub user token from the vault; `409 github_not_linked` without
  one. Every GitHub call (`GET git/ref`, `git/commits`, `git/trees`; `POST git/blobs`,
  `git/trees`, `git/commits`, `git/refs` for a new `pmos/proposal/<kind>/…` branch only,
  `pulls`, `issues/{n}/labels`) carries it. The default branch is never written, nothing is
  merged.
- **Rate limit:** `PMOS_PROPOSALS_PER_HOUR` (default 10) per admin, in memory (one replica;
  a restart forgets): `429 rate_limited`, `retry_after` and `Retry-After`.
- **Audit:** `admin.policy_proposal` / `admin.content_proposal`: `started` first (no record, no
  pull request: `502 audit_unavailable`), then `ok` with `github:<repo>#<n>` or `error`; paths
  as `sources` (served as a count), never file text or the token. Every refusal is `denied`
  with a fixed note (`role`, `invalid`, `stale base`, `policy mismatch`, `rate limited`,
  `github not linked`, `not configured`), served as `detail`.
- **Configuration:** on with `PMOS_CONTENT_REPO` and the GitHub App; `PMOS_PROPOSALS=off`
  turns it off. With it on, start-up refuses a `PMOS_CONTENT_DIR` or `PMOS_POLICIES_DIR` that is
  the repository root or nested in the other, and a proposal is refused (`501
  not_configured`) if such a layout reaches it anyway. `PMOS_POLICIES_ROOT` (default: the
  policies folder of the checkout `PMOS_CONTENT_ROOT` is in) is what the re-check compares
  regenerated policies with.
- **Admin actions** (`POST /internal/admin/actions`, apart from the sign-in events below,
  which have their own rules): what `pmos-web` refused (a role refusal on an
  admin policy or content route; a proposal its own checks refused, `invalid` or `stale base`)
  as `denied`, and an admin's read of a PM's personal master file or restricted context
  (`admin.content_sensitive_read`, `ok`, note `personal file` or `restricted context`), which
  `pmos-web` records before it serves the text. A fixed list of actions and notes; `count` is
  how many files (kept as that many `content` entries in `sources`, served as a count); the
  only target allowed is `content:personal:<handle>`. A sensitive read needs `edit-content`
  (else `403`, recorded as `denied`); a refusal record needs a team member.

### Sign-in events

`POST /internal/admin/events` (`app/admin.rs`, `sign_in_event`) records `pmos-web`'s
sign-ins, sign-outs and refused sign-ins, so they are in the same audit log as everything
else and in the admin audit list with the usual fields.
- **Only `pmos-web`'s token** (the orchestrator, bot and console get `403`).
- **Only fixed values:** `action` is `web.sign_in`, `web.sign_out`, `web.sign_in_refused` or
  `users.identity_conflict`; `provider` is `entra`, `dev` or `password` (a password
  sign-in's session, `docs/password-sign-in.md`; its refusals the password routes record
  themselves); `reason` (refusals only) is
  `not_a_member`, `wrong_account` (no longer sent by `pmos-web`), `expired`, `cancelled`,
  `unavailable`, `failed`, or, since slice U4c, `deactivated`, `sign_in_unconfirmed`,
  `entra_needs_host`, `entra_misconfigured` or `users_unavailable`; `attempt` (refusals
  only) is 12 lowercase hex characters; `member` (with `wrong_account` only) is a team handle.
  Unknown fields, other values and free text are `400 bad_request`.
- **An identity conflict** (slice U4c; spec §7 step 3): `pmos-web` flags a known Microsoft
  account that signed in with a `preferred_username` that is another user's sign-in name
  (the person signs in as their own `oid` says). `provider` `entra`, `member` (required) the
  user whose sign-in name it was, nothing else, no `user=`. Recorded as the first-link route
  records a conflict: `users.identity_conflict`, actor `anonymous`, outcome `denied`, target
  that handle, detail `entra`, so the overview's `identity_conflicts` counts it. A first
  sign-in whose name is already linked to another account is recorded by the first-link
  route itself (`409 linked`/`taken`).
- **Sign-in and sign-out** name the member as `user=` (a team member, else `403`), with a user
  assertion for them as on every admin route. Recorded as actor = the member, client `web`,
  run `web-auth`, outcome `ok`, `detail` `provider: entra`.
- **A refusal** names no one (`user=` is `400`): actor `anonymous`, outcome `denied`,
  `detail` `provider: entra; reason: not_a_member[; attempt: 3f9a1c2b4d5e]`, and for
  `wrong_account` the target `member:<handle>` whose email was used. The attempt is a keyed
  hash `pmos-web` computes of the name tried; the name never reaches the tool server.
- No IP address is recorded (PM OS records none anywhere), no token, no email.
- `503` when the event couldn't be recorded, so `pmos-web` can count it (`docs/web-api.md`,
  **Sign-in events**, for why sign-in goes ahead anyway).

### Audit retention

Owner's decision: audit events are purged at 90 days, with no archive. The whole design, and
why a newer event can't be deleted by anyone, is in `docs/audit-retention.md`. In short:
- Migration 0013 keeps the append-only triggers and gives them one exception: a row-level
  DELETE of a row older than 90 days (the trigger checks the row's age itself), from inside
  `purge_audit_events(interval, integer)` (a `SECURITY DEFINER` function that alone sets
  `pmos.audit_purge`). UPDATE and TRUNCATE stay refused for everyone.
- The function refuses a retention under 90 days, deletes at most a batch (1 to 100000) of the
  oldest eligible rows, records the batch as `audit.purge` (client `system`, actor
  `pmos-retention`, `detail` `purged <n> events older than <cutoff>`) in the same
  transaction, and returns `ran = false` without doing anything if another purge holds its
  advisory lock. `EXECUTE` is revoked from `PUBLIC`.
- **The job** (`retention.rs`): `PMOS_AUDIT_RETENTION_DAYS` (default and minimum 90, at most
  3650; under 90 the tool server refuses to start), first run 5 to 10 minutes after start-up
  (random jitter), then daily, batches of 10000, at most 100 batches a run, stopped on
  shutdown. In memory (`PMOS_DEV=1`) the same rules apply.
- The JSON-lines file (`PMOS_AUDIT_LOG`) is not purged; it gets a copy of each purge event.
- The tool server can't record a `system` event itself (`PgStore::record` refuses it), so the
  overview's `last_purge` is always one the database function wrote.

**Who holds which token:**
- **Service token** (`PMOS_SERVICE_TOKEN`): the orchestrator, the bot runtime and platform-owner tools. Every route, and the only token that starts bot sessions.
- **Bot token** (`PMOS_BOT_TOKEN`): the Slack bot. Only sessions for the skills in `PMOS_BOT_SKILLS` and calls within them, plus reading and deciding one approval item (`GET /internal/approvals/{id}?user=<handle>` and its `decision`), for Slack approval cards, the onboarding routes, `GET /internal/ledger` for the daily triage digest, and for the App Home (`docs/slack-home.md`) a team member's pending items (`GET /internal/approvals?user=`) and unlinking a team member's account (`DELETE /internal/credentials/{user}/{provider}`). A session started for another skill, or any bot session, looks to the bot like one that doesn't exist, and every other route (the ledger summary among them) returns `403`.
  Socket Mode gives the tool server nothing signed to check which PM the bot names, so what
  that trust allows is kept small: a read of one item must name its PM (`?user=`, else
  `404`); ledger rows come without `id`; an edit may change a draft's text but not where it
  goes (`422 target_changed`); and a member decision may grant `pm` only
  (`403 admin_not_from_slack`, `403 role_not_from_slack`: only the host makes an admin).
- **Web token** (`PMOS_WEB_TOKEN`): `pmos-web`, which acts for a signed-in person. Only the
  five approvals routes above (list, one item, sources, decision, saved edit), the four
  read-only `GET /internal/admin/*` reads, the admin proposal and action routes and the sign-in
  events (all of which check the viewer's role themselves; the proposal and action routes need
  a signed assertion in every mode); every other
  route (sessions, grants, calls, account linking, credentials, the ledger, reindexing,
  onboarding) is `403`. It has at least the bot token's limits: a read of one item must name
  its PM (`?user=`, else `404`); it never reaches the ledger (so sees no ledger ids) or the
  member decisions (so can't grant `admin`); and an edit may change a draft's text but not
  where it goes (`422 target_changed`). The audit records it as `web`. Older deployments gave
  it the service token; `deploy/setup.sh` now generates its own (`PMOS_TS_WEB_TOKEN` in
  `deploy/.env`).
- **Console token** (`PMOS_CONSOLE_TS_TOKEN`, optional): the web console, read-only.
  `GET /internal/approvals?user=` and `GET /internal/approvals/{id}`, answered as for the
  service token (`{items: […]}` and the item; no `user` needed for one item); every other
  route (sources and decisions included) is `403`, and an unknown token `401`. A decision
  from it is refused by the service as well (`decide_verdict`), so no later route can widen
  it. With a console user assertion, a list names that person, and one item must be theirs
  or the subject must be an active admin in the tool server's user database (the admin view; the
  assertion's roles don't count). Under `require`, its read of one item needs that
  assertion. The console is given it as its `PMOS_TOOLSERVER_TOKEN`; the audit records it as
  `console`. `tests/approvals_assertion.rs` checks every approvals route and method for each
  token against the exact set allowed, so a new route can't silently widen a client.
- **Session token:** each sandbox gets only its own.

### Approvals auth: user assertions

With the web or console token, the person a call names (`user=`, `decided_by`) is believed
only as `PMOS_ASSERTION_MODE` allows (`docs/user-assertion.md`):

| Mode | A valid `X-Pmos-Assertion` | A bad one | None |
| --- | --- | --- | --- |
| `off` | ignored | ignored | `user=` and `decided_by` believed, as before |
| `verify` | checked | `401` | believed, as before |
| `require` | checked | `401` | `401` for any call that names a person (a list, sources, a decision) and for any read of one item, web or console |

- **Secure by default:** with `PMOS_WEB_TOKEN` or `PMOS_CONSOLE_TS_TOKEN` set, an unset mode is
  `require`, each set token needs its key, and `off` or `verify` refuse to start unless
  `PMOS_ASSERTION_INSECURE_OFF=1` on a fake-data development stack (logged as `INSECURE` at
  error level on every start). The override itself refuses to start with
  `PMOS_CONNECTORS=live`, or with `PMOS_DATABASE_URL` set without `PMOS_DEV=1`, and
  `deploy/compose.yaml` never passes it. With neither token, an unset mode is `off`.

- **Checked:** the signature with the `web:toolserver` or `console:toolserver` key from
  `PMOS_ASSERTION_KEYS`, the issuer (the client whose token was presented: the web token
  cannot use the console's assertion), the audience `toolserver`, the time, the request
  (method, path and query as received, raw body) and the nonce (once per process).
- **Names:** the subject must equal `user=` and `decided_by`. From the web token,
  `GET /internal/approvals/{id}` must name its PM (`?user=`), so it is checked like the rest;
  from the console token it may name no one, but under `require` it still needs an assertion.
  With one, an item that isn't the subject's is `404`, except to the console when the subject
  is an active admin in the user database. In `off` and `verify`, the console's unsigned read of one item
  still reads any item.
- **Answer:** every failure is `401 {"code": "bad_assertion"}`; the log has the client and the
  kind of failure, never the assertion, a key or a handle.
- **Not checked:** the service token and the bot token, in every mode. The runtime's approval
  polling and the Slack approval cards are unchanged.

Session tokens are sent in headers, never in a URL, so access logs and proxies don't record them. A tool error (refused, invalid arguments, not found) comes
back as an MCP tool result with `isError: true`, so the model can read why.

## Feedback ledger

One row per approval item, so one per drafted outbound action
(`docs/improvement-loop-spec.md` §1, `src/ledger.rs`). The approval store writes it in the
same operation that creates the item, decides it and stores its write's outcome (in Postgres,
the same transaction), so a row is never missing. A write approved in the harness with a grant
makes no item, so no row.

| Field | Meaning |
| --- | --- |
| `id` | The approval item's id |
| `run_id` | The task id (`tsk_…`) or skill run id |
| `user` | The PM the draft is for |
| `bot`, `skill` | Who drafted it; one of them is `null` |
| `tool` | Registry name, such as `jira.create_issue` |
| `content_sha` | The content repo's `HEAD` when the item was created (`git rev-parse HEAD`, read at start-up and on every reindex or content sync); `null` outside a git checkout |
| `model_alias` | The drafting skill's or role's gateway alias, such as `pmos/default` |
| `outcome` | `pending`, `approved` (unchanged), `edited` (approved after an edit), `rejected`, `failed` (approved, but the write failed or its outcome is unknown) or `expired` |
| `edit_distance` | `0.0`–`1.0` once approved; `null` while pending and when rejected |
| `reason` | Only for `rejected`, when one was given |
| `created_at`, `decided_at`, `decided_by` | |

- **No text.** Rows hold labels and metadata only: never the draft's arguments, never Slack
  text. Every field but `id` goes to the Slack bot: the bot token's rows have no `id`, so
  it never learns approval ids it wasn't shown on a card (the triage digest uses `run_id`).
- **`expired`** is stored for a Slack draft that expired (see **Slack drafts**), with no
  `decided_at`. For every other tool it is worked out on read: a row still `pending` and
  never decided is reported as `expired` once it is older than `PMOS_LEDGER_EXPIRE_DAYS`
  (default 7). An approved write still running stays `pending` until its outcome is stored.
- **`edited`** means the decision changed the item's arguments (their hash differs). An
  `edit` decision that sends back the same arguments is `approved`.
- **The edit distance** is the normalized character-level Levenshtein distance between the
  original and the final text, divided by the longer one's length. The text is the item's
  string arguments, at any depth, in key order, joined with newlines. A text longer than
  10,000 characters is compared on its first 10,000 (each side), and a shared prefix and
  suffix are skipped first, so small edits to long drafts stay cheap. The Postgres store
  computes it before its decision transaction, so no row lock is held while it runs.
- **Items from before the ledger** (migration `0007_ledger.sql`) get a row each, with
  `content_sha`, `model_alias` and `edit_distance` `null`.

**Reading it.** `GET /internal/ledger` (service or bot token) returns `{rows: […]}`, newest
first:

| Parameter | Meaning |
| --- | --- |
| `since`, `until` | RFC 3339 times. They match a row's last change, `decided_at` once decided, else `created_at`: `since` inclusive, `until` exclusive. So "the past day" includes an older draft rejected today. |
| `bot` | One bot's rows |
| `outcome` | Comma-separated reported outcomes, such as `rejected,edited`; `expired` and `pending` filter as reported |
| `min_edit` | `0`–`1`: drop rows whose edit distance is below it. Rows with no edit distance (pending, rejected, expired) are kept, so `outcome=rejected,edited&min_edit=0.3` reads "rejected, or edited by at least 0.3" |
| `limit` | `1`–`1000`, default `1000` |

**The summary.** `GET /internal/ledger/summary?since=&bot=` (service token) returns
`{since, rows, label_coverage, groups: […]}`. Each group is one bot (or skill) and tool, with
`rows`, `counts` (every outcome, zeros included), `label_coverage` (the share of rows that are
decided or expired: phase 0's 95% gate), `median_edit_distance` (over rows that have one;
`null` if none) and `reasons` (rejections by reason, `none` for those without one).
`since` matches like the ledger's.

## Slack drafts

The tool server's part of the Slack reply assistant (`docs/slack-reply-spec.md` §3): it
applies to `slack.post_reply` and `slack.send_message` items, from any skill or bot.

**One draft per thread.** A `slack.post_reply` call first looks for the same user's reply to
the same `channel` and `thread_ts`, created in the last 7 days, that is `pending`,
`executing` or `approved`. If there is one, nothing new is queued, and the call succeeds
with `{"status": "already_drafted", "approval_id": "<that one>", "message": "A reply to this
thread is already waiting for <user> (or was posted); don't draft another."}`, audited as
`ok` with detail `already drafted`. A rejected, failed or expired reply doesn't block a new
one. In Postgres a transaction-scoped advisory lock per user and thread makes concurrent
drafts create one item.

**One draft per tagged message.** When the PM tags PM OS in a thread
(`docs/slack-mentions-and-home-spec.md`), the call also carries `message_ts`, the tagged
message (optional; `^\d{1,12}\.\d{1,9}$`, checked with the other arguments). With it, only a
reply with the same `channel`, `thread_ts` **and** `message_ts` blocks a new one, and the
message says "A reply to this message …". Without it the rule is per thread, as above, and
tagged replies in that thread count. `message_ts` is never sent to Slack (the connector
posts only the channel, thread and text), is kept through expiry and the purge like
`channel` and `thread_ts`, and isn't part of the ledger's edit-distance text. It is stored
in `args`; no migration.

**What a draft was built from.** Each session records the highest data class it has read
(`sessions.sources_sensitivity`, which only rises):
- `context_search` and `context_read`: each result's own `sensitivity`;
- Jira, Confluence and Slack reads: `internal`;
- `memory_read`: the highest `sensitivity` among the notes returned (all of them), at least
  `internal`; `bets_read`: that bet's note's, `bets_list`: the highest among the bets
  listed, each at least `internal` (see **Memory**);
- the public docs: `public`.

A read that finds nothing counts for nothing. Every new approval item stores the session's
value as `sources_sensitivity`; items made before migration 0008 have none, and the audience
check treats them as `restricted`.

**The audience check (SLK-6).** Approving or editing a Slack write first asks Slack who can
see the channel, with the PM's own token (never a team bot's): `conversations.info`
(`is_ext_shared`, `is_shared`, `is_im`) and `conversations.members` (paged, at most 1,000; a
channel with more counts as having outsiders). An edit is checked against its edited
channel. Then, in order:

1. **Restricted sources never reach outsiders.** If `sources_sensitivity` is `restricted` and
   any member isn't on the product team (an active user holding `pm`, `reviewer` or `admin`,
   found by Slack id: a deactivated or auditor-only person is an outsider), or the channel is shared outside the
   company (`is_ext_shared`): `409 {code: "audience_blocked", error, outsiders, members,
   external}`. Nothing is written and the item stays `pending`; the PM can reject it. An edit
   doesn't change what the draft was built from, so it is blocked too.
2. **Slack Connect needs a second confirmation.** If the channel is externally shared and the
   decision lacks `confirm_external: true`: `409 {code: "needs_external_confirm", error,
   members, external}`. The item stays `pending`; the same decision with `confirm_external:
   true` posts.
3. Otherwise the write goes ahead.

If either Slack call fails (no such channel, a missing scope, Slack down), the decision fails
closed: `503 {code: "audience_unavailable", error}`, nothing written, the item still
`pending`. A block or confirmation request is audited as `denied` with its code in `detail`
(`audience_blocked: 2 not on the product team, external: false`, `needs_external_confirm`); an
unavailable check as `error` (`audience_unavailable: …`). Rejecting is never checked. A write
approved in the harness with a grant doesn't go through this check.

The members count against the user database, read for each decision: with an empty
directory, everyone is an outsider; with the database unreachable, nothing is posted.

**The 24-hour expiry (SLK-8).** A sweeper runs at start-up and every 10 minutes:
- **Undecided drafts expire.** A `pending` Slack draft older than `PMOS_SLACK_DRAFT_TTL_HOURS`
  (default 24) becomes `expired`. Every string argument except a top-level `channel`,
  `thread_ts` and `message_ts` becomes `"[expired]"`; the `args_hash` is kept, and `redacted_at` records when.
  The ledger row is stored as `expired`, and the item is audited with outcome `expired`.
- **Decided drafts lose their text** the same way, the same time after their decision
  (`approved`, `rejected` or `failed`; the status stays). The posted copy lives in Slack, and
  `result` keeps its link.
- **Deciding an expired draft,** or a pending one already past its time that the sweeper
  hasn't reached (it is expired then and there), answers `409 {code: "expired", error}`.

Other tools' items never expire this way; the ledger still reports them as `expired` after
`PMOS_LEDGER_EXPIRE_DAYS`.

## Invalid drafts

A write call refused for invalid arguments (a schema error, or a connector's bad-argument
answer such as a bad Jira field) is the one refusal a model can act on, so it gets one
correction (`docs/improvement-loop-spec.md` §2):
- **Counted per session and write tool**, in memory with the session (a restart starts the
  counts again). Reads, and other refusals, aren't counted.
- **The second refusal's message** ends "This is the last attempt for this action. Tell the
  PM what was wrong instead of retrying."
- **Then refused for the session.** Any further call to that tool in the session is refused
  (`403` on `/internal/call`, an MCP tool error) with a message ending the same way, before any
  check, grant or connector call, so nothing is drafted or written.
- **Audited** as `denied`, with `detail` `invalid draft (1 of 2)`, then `invalid draft (2 of 2)`;
  later refusals `invalid draft (2 of 2): no attempts left in this session`.

## Bot sessions

A named bot (`content/bots/<name>/BOT.md`, made from one role in `content/roles/`) gets a
session per task: the bot runtime sends `POST /internal/sessions {user, bot, run_id}` with the
service token. Bots and roles load with the skills at start-up and reload on
`POST /internal/reindex`; a live session follows the new settings on its next call, and a
session whose bot is gone (or whose personal bot changed owner) stops working.
Spec: `docs/bot-runtime-spec.md` §2.

| Session property | Rule |
| --- | --- |
| Tools | `role_tools(role, skills)`: the union of the role's skills' tools |
| Data class | the role's `data-class` |
| A write set to `never` (`bot.action_setting(role, tool)`, the bot's override, else the role's) | not in `tools/list`; a call is refused (`403` on `/internal/call`, an MCP tool error), audited as `denied`, and never reaches the inbox. A harness grant can't make it run. |
| A write set to `approve` | the usual grant-or-inbox path. The item records the bot, and is checked again when decided: if the bot is gone, its role lost the tool or it is now `never`, the decision is `409` and the item stays pending. |
| `user` | the PM the task is for. A personal bot serves only its owner (`403` otherwise); a team bot, any active PM OS user (the user database). Never a `bots.*` identity. |

Sessions, approval items and audit events record either `skill` or `bot`, never both; a bot
session's `skill` is `null`. A personal bot's session may also run as a persona (below).

## Personas and teams

The bot marketplace (`docs/bot-marketplace-spec.md`, slice B1): Chief, Sprout, Echo and any
other **persona** in `content/personas/` is a role a PM's own personal bot (`pm-<handle>`, role
`pm-assistant`) runs as, chosen per ask. Which ones a PM may ask is their **team**, one
`user_bots` row per persona they picked (migration 0024). Nudge, the always-on generalist, is on
every team and is never a row. `src/team.rs` (the store, in memory and Postgres),
`src/service/team.rs` (the rules), `src/app/team.rs` (the routes), `tests/team.rs` (every rule on
both stores).

**A persona session** (`POST /internal/sessions {user, bot, run_id, persona}`, service token
only, `ToolService::create_bot_session_as`). After the bot session's own checks, in order, each
refusal `403 {code, error}` with a sentence the runtime passes on to the PM:

1. the bot is a personal bot whose role is `pm-assistant` (`persona_needs_personal_bot`);
2. the persona is in the catalog, offered and its role loaded (`persona_unavailable`: "Chief
   isn't offered in PM OS any more. Ask Nudge instead."; an unknown key reads "That bot", and
   the request's text is never echoed);
3. it is `nudge`, or a row on `user`'s team (`persona_not_on_team`: "Chief isn't on your team
   any more. Add it on the Bots page in PM OS, or ask Nudge.").

The session stores the persona (`sessions.persona`; `null` for Nudge, also when the request
names `nudge`). What it may do, with `B` the personal role and `P` the persona's role:

| Session property | Rule |
| --- | --- |
| Tools | `role_tools(P) ∩ role_tools(B)`, less any write whose effective setting is `never`. `tools/list` shows only these. |
| A write's setting | the stricter of the bot's own setting (its override, else `B`'s), `B`'s and `P`'s: `never` beats `approve`. A tool `P` adds, or a write `B` sets to `never`, stays out even if content that fails `pmos lint` were loaded (`tests/team.rs`, "holds the ceiling without the lint"). |
| Data class | the lower of `P`'s and `B`'s |
| Model (ledger) | `P`'s |
| Re-checked | on every call, as any session: a persona withdrawn (`offered: false`) or gone from the catalog ends it. Removing the persona from the PM's team does **not** end a live session: the task already started finishes (within the session's TTL). |

Every audit event of the session records `persona`; every approval item it drafts records it
(`approvals.persona`, copied at creation, never changed: no route writes it and a trigger
refuses an update). **Decisions** check the item's bot and its own role, never the persona, so a
draft stays the PM's and decidable after its persona leaves the PM's team; the decision's event
records the item's `persona`. Sensitivity, the audience check, the owner-only rule and the
reviewer rules are the same for every persona. Item views (`GET /internal/approvals…`, the
reviewer's views, restricted metadata-only included) add `persona` (`null` is Nudge).

**A withdrawn bot's drafts are held** (slice M1, the security review): a pending item whose
persona is no longer offered (withdrawn, or gone from the catalog) can be rejected by its owner
but not approved or edited (`409 {code: "held"}`, recorded `denied` with the note `held: the
draft's bot is no longer offered`) until the persona is offered again or an Admin releases it
(`POST /internal/admin/personas/drafts/{id}/release`, below; stored in
`approval_hold_releases`, migration `0026`). Its views carry `held: true` (left out otherwise).

**A write a persona role doesn't list is refused** (`403`, "the role doesn't list it in its
`actions`"): a persona role's unlisted write is `never`, not `approve`. `pmos lint` refuses such
a role too (`persona-action-unlisted`).

**A persona's draft notice** (`draft-notice` in its `PERSONA.md`; Echo's "Draft for internal
review. Not approved for external use.") is put at the top of the `body` of every
`confluence.create_draft` and `confluence.update_page` it drafts when the item is recorded, and
again when the approved write runs, so neither the model nor an edit leaves it out. Jira issues
and Slack replies don't carry it. No extra approval step goes with it.

**The session body is strict**: `POST /internal/sessions` refuses a field it doesn't know
(`422`), so a caller that sends a field a later version adds fails closed instead of getting a
session without it (the runtime also requires the answer to echo the persona and its role).

**Routes.**

| Route | Token | Answer |
| --- | --- | --- |
| `GET /internal/personas` | service, bot, web (no assertion; the console `403`) | `{catalog_sha, personas: [Persona]}`, the offered ones, Nudge first, then by `order`. `Persona` is `{key, display_name, title, mascot, always_on, preview, summary, examples, can_do, reads, drafts, needs: [{source, label, without, met}]}`, nothing else of the file (`preview: true` is "Preview: not yet benchmarked"); `reads`, `drafts` and `needs[].met` are derived from the role's tools by `pmos lint`. |
| `GET /internal/team/{handle}` | web, signed for `handle` in every mode | `Team` = `{handle, bots: [{key, always_on, added_at, offered, added_via?}], drafts_this_month: {key: n}}`. `bots` is Nudge (`added_at: null`), then each row in catalog order (a row whose persona left the catalog last); `offered: false` marks a withdrawn or deleted one. `drafts_this_month` counts the person's approval items per persona since the 1st, UTC (Nudge's under `nudge`). |
| `PUT /internal/team/{handle}` | the same, and assertion `require` | `{add?: [key], remove?: [key], via: "onboarding" \| "bots_page" \| "picker"}` (no other field): `Team`. Atomic; adding a key already there or removing one that isn't is a no-op. |
| `GET /internal/admin/personas?user=<viewer>` | web (assertion as the mode says) | `{personas: [{key, display_name, offered, always_on, preview, users, handles, needs: [{source, label, met}], drafts_30d, name_clashes}], unknown: [{key, users, handles, drafts_30d}]}`: every persona in the catalog, offered or not (Nudge's `users` and `handles` are `null`); `unknown` lists rows naming a persona no longer in the catalog; `name_clashes` the handles of users whose handle or display name reads as the persona's key or display name. Keys, handles and counts only. Needs `view-audit-log` or `manage-members` (`403 forbidden`), recorded as `admin.personas_read`. |
| `POST /internal/admin/personas/drafts/{id}/release?user=<admin>` | web, signed for `user`, assertion `require` (else `503 assertion_required`) | Release a held draft (above): `200` the item. `403 forbidden` (not an active person with `edit-content`), `404 not_found`, `409 not_held` (decided, not a persona's, its persona offered, or already released). Every attempt is recorded as `approvals.release_hold` (`ok` or `denied` with the code), with the item's `persona`. |

Team route errors, each `{code, error}`: `401 bad_assertion` (missing, invalid, or not for this
request); `403 forbidden` (signed for someone other than `handle`: nobody changes another
person's team, an admin neither); `403 not_allowed` (active but without `ask-bot`, such as an
Auditor-only person); `403 not_a_member` (not a user, or deactivated: their rows are kept and
grant nothing); `400 bad_request` (a body that doesn't parse or names another field, both lists
empty, a key that isn't a persona name, a key in both lists, `via` not one of the three);
`409 always_on` (`nudge` in either list); `409 bot_unavailable` (adding a key that isn't offered:
unknown, withdrawn or role missing; removing a withdrawn or deleted persona's row is allowed);
`409 team_full` (more than 16 rows); `429 rate_limited` (more than 30 change attempts in an hour
by one person, counted in memory); `503 assertion_required` (a change without assertion
`require`).

**History is the audit log.** Every change attempt is a `team.change` event (client `web`,
`user` the person, `output_ref` their handle), written in the same transaction as the rows in
Postgres. Allowed: `ok`, detail `add=chief,sprout remove=echo via=onboarding` (keys only, `-` for
an empty list). Refused: `denied`, detail one fixed note: `not_offered`, `always_on`,
`rate_limited`, `not_allowed`, `not_a_member`, `bad_request`, `team_full`, or before the person
is verified (user `anonymous`) `forbidden`, `bad_assertion`, `assertion_required`. The admin
audit view serves these details and each event's `persona`. Events are purged at 90 days by the
one retention job; `user_bots` is current state (a removed bot's row is deleted) and nothing
here deletes an event.

**Defaults and limits.** Everyone starts with Nudge only, existing users included: the migration
adds no rows, and rows from before it read `persona` as `null`. At most 16 rows per person (in
code and in a deferred constraint trigger that takes the same per-person lock as a team change, so
concurrent writers are serialised: migration `0026`). The 30-an-hour count is checked and taken
under one lock, so concurrent requests can't pass it either (`tests/team.rs`). The eval stack's `evals` account gets every offered
persona (`added_via: seed`) when the in-memory user store is seeded from `PMOS_USERS_SEED`; never
with a database.

### Conversation history

A follow-up task in a conversation replays earlier answers to the model
(`docs/bot-runtime-spec.md` §3, Conversations). Those answers may carry what the earlier tasks
read, so the follow-up's session must not start at `public`:

- The runtime reads each finished task's level (`GET /internal/runs/{task id}/sensitivity`)
  and stores it; for a new task with history, it sends the highest level of the replayed turns
  as `history_sensitivity` (an unknown one as `restricted`).
- The session starts at it (`sources_sensitivity`), and only rises from there. Every draft the
  session makes records it, so a Slack draft to a channel with someone outside the team, or a
  Slack Connect channel, is `409 audience_blocked` when the history was restricted, though
  the session read nothing itself.
- Only the runtime (the service token) sets it; the worker in the sandbox never sees or sends
  it. `tests/history_e2e.rs` runs the runtime against a real tool server for this case.

**Who a bot reads and writes as.**
- **Personal bots** read as their owner, with the owner's linked accounts and no scope.
- **Team bots** read Jira, Confluence and Slack as the bot's own service identity,
  `bots.<name>`. A platform owner links its read-only accounts with
  `GET /internal/oauth/{provider}/start?user=bots.<name>` (service token), then opens the
  URL signed in as the bot's account. The user-database identity check is skipped only for
  `bots.<valid bot name>`; such a handle is never a PM, so it gets no session and can't
  decide approvals. `GET /internal/credentials?user=bots.<name>` and
  `DELETE /internal/credentials/bots.<name>/{provider}` list and unlink it. If it isn't
  linked, a read fails with "the `<name>` bot has no working … account", never falling back
  to the PM's.
- **Writes** from any bot session run as `user`, with the approving PM's own token, never as
  the bot.

**A team bot's read scope** is its `reads` list in `BOT.md`, enforced in the connectors (the
fake ones apply it too):

| Tool | How |
| --- | --- |
| `jira.search` | the JQL becomes `project in ("A", "B") AND (<jql>)`; a trailing `ORDER BY` stays outside the group |
| `jira.read` | the key's project must be listed, checked before the call and again on the issue returned (a moved issue changes key) |
| `confluence.search` | the CQL becomes `space in ("A") AND (<cql>)` |
| `confluence.read` | the page's space (by id, from `GET /wiki/api/v2/spaces?keys=`) must be listed, checked after the fetch and before anything is returned; a page with no space is refused |
| `slack.search` | results outside the listed channels are dropped |
| `slack.read_thread` | the channel must be listed |

- An empty list means no access to that system. An out-of-scope read is `403`, audited as
  `denied`.
- **No query injection.** Project and space keys must match `^[A-Z][A-Z0-9_]{0,19}$` and
  channels must be ids (`C…` or `G…`); an entry that doesn't is refused at read time (and
  logged at start-up), never put in a query. A query is wrapped only if its quotes are
  closed, its parentheses balanced, it has no backslash outside a quote and its `ORDER BY` is
  a plain list of fields, so nothing in it can reach outside the `( … )`. Otherwise it is
  `400`.
- `docs.*` and `context.*` have no read scope: the docs are public, and context follows the
  data class.

## Tools

Tool names in MCP are the registry names with dots turned into underscores
(`jira.create_issue` is `jira_create_issue`), the same names the harness's `interruptOn`
uses. The registry is `content/tools.yaml`; each tool's input schema is in
`crates/toolserver/src/schemas.rs`. The server refuses to start if a registry tool has no
schema or no connector.

**Write arguments are bounded** (`maxLength` in the schemas, refused as invalid arguments):
Slack `text` 40,000 characters; Confluence `body` 200,000, `title` 255 and
`version_comment` 500; Jira `summary` 255 and `description` 32,000; a memory note's `text`
4,000.

### Memory

`memory.read` returns all of the session user's notes, `{"notes": [{key, text, updated_at,
sensitivity}]}`; `memory.update {key, text}` creates or replaces one. Where they are kept:
in Postgres (`memory_notes`) or in memory by default, or in the PM's own GitHub repo (see
**Personal memory in GitHub**).
- **A note carries the sensitivity of what it was built from.** `memory.update` stores the
  writing session's highest data class read so far (`public`, `internal` or `restricted`);
  `bets.record` stores its approval item's `sources_sensitivity` (an item from before
  migration 0008, with none, counts as `restricted`), or the bet note's own if that is
  higher, since the merge keeps the bet's earlier text. A note with none (written before
  migration 0009, or a GitHub note without a known `sensitivity:`) reads as `internal`.
- **Reading raises the session to it.** `memory_read` raises the session to the highest
  among the notes returned; `bets_read` to that bet's; `bets_list` to the highest among the
  bets listed. Never below `internal`, and nothing for a read that returns nothing.
- **So:** once a PM's memory holds a restricted note, a Slack reply drafted in a session
  that read it is blocked in channels with people outside the team (`409 audience_blocked`,
  see **Slack drafts**), until the note is rewritten from less sensitive sources.
- **Only the PM's own.** A team bot is refused both memory tools (`403`, audited as
  `denied`), at the call and when an item is decided, as with the bets tools.
- **Bets are the register's.** `memory.update` refuses keys that start `bet-` (invalid
  arguments): only an approved `bets.record` writes a bet.

### The stats tools

`stats.sample_size` and `stats.compare` (system `stats`, kind `read`) do the arithmetic of an
experiment, so a model never does. The `experiment-design` and `experiment-readout` skills
declare them (`docs/pm-craft.md`).
- **Deterministic:** the same arguments always give the same answer.
- **No outside system:** they compute inside the tool server, so they need no linked account
  and have no read scope.
- **Allowlisted and audited** like any read: a skill must declare the tool, and every call is
  in the audit log.

**`stats.sample_size`: how many users a test needs.**

| Argument | Type | Default | Meaning |
| --- | --- | --- | --- |
| `metric` | `"proportion"` or `"mean"` | required | What is measured: a rate, or a mean |
| `baseline` | number | required | The baseline rate, above 0 and below 1, for a proportion; the baseline mean for a mean |
| `std_dev` | number > 0 | required for `mean` | The standard deviation |
| `mde` | number > 0 | required | The minimum detectable effect |
| `mde_kind` | `"relative"` or `"absolute"` | `"relative"` | Relative: `0.05` means 5% of the baseline. Absolute: in the metric's own units |
| `alpha` | number | `0.05` | The significance level |
| `power` | number | `0.8` | The chance of detecting an effect of size `mde` |
| `two_sided` | boolean | `true` | A two-sided test |
| `variants` | integer, at most 10 | `2` | Total arms, including control |
| `daily_users` | number | none | Users entering the experiment per day, all arms together |

It returns:

| Field | Meaning |
| --- | --- |
| `per_arm` | Users each arm needs, rounded up |
| `total` | Users in all arms |
| `days` | `ceil(total / daily_users)`; only when `daily_users` is given |
| `assumptions` | The values it used, defaults included |
| `note` | Says to run whole weeks |

A result that isn't a finite number, or is more than 10^12 users per arm, is refused as
invalid arguments (the effect is too small to detect), and the totals are multiplied with
overflow checks.

The formulas, with `z_{α/2}` and `z_β` the normal quantiles for `alpha` and `power`:
- **Proportion:** the two-proportion z-test with unpooled variance, where `p1` is the
  baseline and `p2` the baseline with the effect:
  `n = (z_{α/2} + z_β)² · (p1(1−p1) + p2(1−p2)) / (p2 − p1)²`.
- **Mean:** `n = 2 · (z_{α/2} + z_β)² · σ² / δ²`, where `σ` is `std_dev` and `δ` the absolute
  effect.

**`stats.compare`: reading a finished test.**

| Argument | Type | Default | Meaning |
| --- | --- | --- | --- |
| `control` | `{users, conversions}` | required | The control arm's counts |
| `treatment` | `{users, conversions}` | required | The treatment arm's counts |
| `alpha` | number | `0.05` | The significance level |

It returns:

| Field | Meaning |
| --- | --- |
| `control_rate`, `treatment_rate` | Conversions ÷ users, for each arm |
| `absolute_lift` | The treatment rate minus the control rate |
| `relative_lift` | The absolute lift ÷ the control rate |
| `ci_low`, `ci_high` | The confidence interval of the absolute lift, at `1 − alpha` |
| `p_value` | From a two-proportion z-test |
| `significant` | Whether `p_value` is below `alpha` |
| `note` | Warns that a result read before the planned sample size is not reliable |

The p-value uses the pooled variance; the interval uses the unpooled variance.

**Bad input is refused** as invalid arguments (`400` on `/internal/call`, an MCP tool result
with `isError: true`):
a rate outside 0 to 1, zero users, more conversions than users, or an `mde` of 0 or less.

### `jira.rollup`: counts across teams

`jira.rollup` (system `jira`, kind `read`) counts issues across projects, so a model never does.
The `portfolio-review`, `roadmap-tradeoffs` and `business-review` skills declare it
(`docs/levels.md`). It returns no descriptions or comments.

| Argument | Type | Default | Meaning |
| --- | --- | --- | --- |
| `jql` | string, at most 2,000 characters | required | Which issues to count |
| `group_by` | `project`, `epic`, `status`, `assignee` or `fix_version` | `project` | How to group them |
| `max_issues` | integer, 1 to 1,000 | `500` | The most issues it reads |

- **How it reads:** `GET /rest/api/3/search/jql` with the PM's own token, 100 issues a page,
  following `nextPageToken`, asking only for `status`, `project`, `parent`, `fixVersions`,
  `duedate`, `issuelinks`, `assignee` and `issuetype`. It stops at `max_issues` or after 45
  seconds in all. A team bot's Jira scope applies as for search.
- **It returns one item,** titled `Jira rollup: <n> issues by <group_by>`:

```json
{"counted": 6, "truncated": false, "reason": null, "group_by": "project",
 "groups": [{"key": "PM", "name": "PM", "total": 3, "todo": 2, "in_progress": 1,
             "done": 0, "overdue": 0, "unassigned": 0}],
 "links": [{"from": "PM-102", "from_group": "PM", "relation": "blocks",
            "to": "CLD-13", "to_group": "CLD", "to_state": "todo"}],
 "links_truncated": false}
```

| Field | Meaning |
| --- | --- |
| `truncated`, `reason` | `reason` is `"cap"` or `"time"` when it stopped early |
| `groups[].todo`, `in_progress`, `done` | From Jira's status category: `new`, `indeterminate`, `done` |
| `groups[].overdue` | A due date before today, and not done |
| `groups[].unassigned` | No assignee |
| `links` | Only links that cross groups, or leave the query (`to_group` is `null`), each pair once |

**The limits:**
- **Groups:** at most 50, largest first; the rest are summed into one group with key `other`.
  An issue with no value for the grouping is in group `none`. With `fix_version`, an issue
  counts in each of its versions.
- **Links:** at most 200 (`links_truncated`). With `group_by: project`, the other issue's
  project is its key's prefix.

**The fake fixtures** (used only by the rollup, so search results don't change) hold six
issues whatever the JQL says: the three `PM` issues (PM-101 in progress, PM-102 and PM-103 to
do) and `CLD-11` (epic, in progress), `CLD-12` (story, done) and `CLD-13` (story, to do, due
2026-10-01, unassigned). `PM-102` blocks `CLD-13`. By project: `PM` 3 (2 to do, 1 in
progress, 0 done) and `CLD` 3 (1, 1, 1), with one overdue and one unassigned in `CLD`, and
one link.

### `stats.cashflow`: payback and net present value

`stats.cashflow` (system `stats`, kind `read`) does the arithmetic of a business case. The
`investment-case` and `pricing-proposal` skills declare it. Like the other stats tools it is
deterministic, needs no linked account and has no read scope.

| Argument | Type | Default | Meaning |
| --- | --- | --- | --- |
| `scenarios` | 1 to 5 of `{name, upfront, years}` | required | `upfront` is 0 or more; `years` is 1 to 10 of `{revenue, cost}`, each 0 or more |
| `discount_rate` | number from 0 to 1 | `0.1` | The yearly rate for net present value |

It returns `{scenarios, assumptions: {discount_rate}, note}`. Each scenario has:

| Field | Meaning |
| --- | --- |
| `net_by_year` | Revenue minus cost, each year |
| `cumulative_by_year` | Running total, starting from minus `upfront` |
| `payback_years` | When the cumulative figure reaches zero, in years and fractions (straight-line within the year); `null` if it never does |
| `npv` | Minus `upfront`, plus each year's net divided by `(1 + rate)^year` |
| `total_revenue`, `total_cost` | The sums |

Money is rounded to 2 decimals and `payback_years` to 1. Bad input is refused as invalid
arguments. The `note` says the result is arithmetic on the inputs, not a forecast.

**Check:** upfront 100; year 1 revenue 60, cost 10; year 2 revenue 80, cost 10; rate 0.1 gives
net 50 and 70, cumulative -50 and 20, payback 1.7 and npv 3.31. The `principal-craft`
benchmark and `docs/levels.md` quote these.

### The bets register

`bets.list`, `bets.read` and `bets.record` (system `bets`) keep a PM's bets across months
(`docs/levels.md`). The `bets-register` skill declares them. A bet is one note in the PM's own
memory, key `bet-<key>`, so it lives wherever their memory does (their private GitHub repo
when memory is on GitHub). The tool server owns the note's format: it parses and renders it,
and the model never writes the whole note.

A bet has `key`, `title`, `status` (`open`, `confirmed`, `refuted`, `dropped`), `bet` (one
sentence), `metric`, `riskiest_assumption`, `test`, `opened`, `review_by`, `evidence`
(`[{date, text, source?}]`), `decisions` (the same shape) and `updated_at`.

| Tool | Kind | Arguments | Returns |
| --- | --- | --- | --- |
| `bets.list` | read | `status?` | `{"untrusted": true, "note": …, "bets": [{key, title, status, review_by, updated_at}]}`, soonest review first |
| `bets.read` | read | `key` | `{"untrusted": true, "note": …, "bet": {…}}`; an unknown key is not found |
| `bets.record` | write | below | `{"bet": {…}, "created": true\|false}`, after approval |

**`bets.record` arguments:**
- `key`: 1 to 60 characters of `a-z`, `0-9` and `-`;
- optional `title`, `status`, `bet`, `metric`, `riskiest_assumption` and `test` (each at most
  500 characters), and `review_by` (`YYYY-MM-DD`);
- optional `evidence`: up to 5 of `{date, text (at most 500), source?}`;
- optional `decision`: `{date, text (at most 500), source?}`.

**The rules:**
- **A new bet** needs `title`, `bet`, `metric`, `riskiest_assumption` and `review_by`. Its
  status starts `open` and `opened` is today.
- **An existing bet:** the fields given replace the old ones; evidence and the decision are
  appended.
- **An approval, like every write.** Nothing is saved before approval. On approval the server
  merges and saves.
- **A full note** (over the memory limit of 4,000 characters) drops its oldest evidence until
  it fits, and records how many it dropped.
- **Who may use them:** a PM's own sessions and their personal bot, as with the memory tools.
  Reading a bet raises the session to its note's sensitivity, at least `internal` (see
  **Memory**).
- **Untrusted:** a bet's text came from earlier sessions and the sources they read, so
  `bets_list` and `bets_read` results carry `"untrusted": true` and a note to treat them as
  data, like connector results.
- **Role:** `pm-assistant` has `bets.record: approve` in its `actions`.

## Connectors and account linking

With `PMOS_CONNECTORS=live`, every Jira, Confluence and Slack call acts as the PM, with the
PM's own token, so a PM can only read and change what they could in the product itself. The
one exception is a team bot's reads, which use the bot's own read-only accounts inside its
read scope (see **Bot sessions**); its writes still act as the PM.

| System | Auth | Calls |
| --- | --- | --- |
| Jira | Atlassian OAuth 2.0 (3LO), via `api.atlassian.com/ex/jira/{cloudid}` | `GET /rest/api/3/search/jql`, `GET /rest/api/3/issue/{key}`, `POST /rest/api/3/issue` (description in Atlassian Document Format) |
| Confluence | same token, via `api.atlassian.com/ex/confluence/{cloudid}` | `GET /wiki/rest/api/search?cql=`, `GET /wiki/api/v2/pages/{id}`, `GET /wiki/api/v2/spaces?keys=`, `POST /wiki/api/v2/pages` (always `status: draft`), `PUT /wiki/api/v2/pages/{id}` (version + 1) |
| Slack | the PM's user token (`xoxp`), never a bot token | `assistant.search.context` (Real-time Search; results are never stored), `conversations.replies`, `chat.postMessage`, and for the audience check before a post, `conversations.info` and `conversations.members` (always the PM's token, even in a team bot's task) |
| Public docs | none (public docs); the tool server's `PMOS_DOCS_TOKEN` to `pmos-docs` | `search_docs`, `get_doc` over MCP; see `docs/docs-server.md` |

These API shapes were checked against developer.atlassian.com, docs.slack.dev and
docs.github.com in November 2026, and tested against local mocks (`tests/live_connectors.rs`,
`tests/github.rs`). The first real call happens on the Spark after Gate 1.

**Linking an account.**
1. The Slack bot (its Accounts tab, or a first-use prompt) asks `/internal/oauth/{provider}/start` for a URL and sends the PM there.
2. Atlassian or Slack sends the PM's browser back to `/oauth/{provider}/callback`.
3. The tool server exchanges the code and checks the account belongs to that PM in the user
   database: the Slack user must be their Slack identity; the Atlassian account (from `/me`,
   scope `read:me`) must be the one they linked first (its `account_id`), or on a first link
   have their `atlassian_email` (see **The tool server decides from the user database**). For GitHub, see
   **GitHub**. Only then does it seal the tokens in the vault. A link forwarded to someone else
   attaches nothing.

The `state` value is HMAC-signed, names the user and provider, expires in 10 minutes and is
accepted once. Atlassian asks for `offline_access`, so it returns a refresh token, and
`accessible-resources` gives the site's `cloudid`. Set `PMOS_ATLASSIAN_SITE` if a grant covers
several sites.

**Refresh.** An Atlassian token within 60 seconds of expiry is refreshed under a per-user lock,
so concurrent calls make one refresh. Atlassian refresh tokens rotate: each refresh returns a
new one, which replaces the old.
- **Only a refused grant unlinks.** If Atlassian answers `invalid_grant` (revoked, or unused for
  90 days), the account is unlinked and the PM is asked to reconnect.
- **Any other refusal keeps the account.** For example `invalid_client` after a mistyped
  client secret, or a firewall page. The call fails as unavailable and an error is logged for
  the platform owner, rather than making every PM reconnect.
- **Saving the new token is conditional.** It is written only over the row it was refreshed
  from, and retried if storage blips. Unlinking waits for any refresh in flight, so an
  unlinked account never comes back. Re-sealing after a key rotation is conditional the
  same way.

**Errors the model sees** are written for the PM:
- "connect Atlassian (Jira and Confluence) in PM OS first" when no account is linked.
- "reconnect" on a 401 or a revoked token.
- "permission denied" on a 403, audited as `denied`.
- A rate limit with its `Retry-After`.

**What the model gets back** is bounded, because the context window on the Spark's local model is shared by every session:
- **Bodies:** a Jira description, a Confluence page or a docs page is cut at 12,000 characters and marked `truncated`.
- **Jira comments:** at most 20, each cut at 1,000 characters.
- **Slack threads:** at most 50 messages, each cut at 2,000 characters.
- **Searches:** at most 20 results.
- **Input:** query strings are at most 500 characters (JQL and CQL 2,000), and a search uses at most 32 distinct words.

**What gets written.**
- Every Slack post and Jira description ends with "Drafted with PM OS", and the Jira and
  Confluence ones also carry the run id, so a reader can trace them back to the run (SLK-7, OUT-1).
- Raw HTML in a Confluence draft's markdown is escaped, so a draft can't add macros the PM
  didn't see in the approval card.
- **Updating a Confluence page keeps its status**, so updating a draft doesn't publish it.
- **A Confluence update names the version it read.** `confluence_read` returns the page's
  `version` number in its content, and `confluence_update_page` requires it (`version`, an
  integer, 1 or more). If the page's current version differs, the update is refused before
  any request that changes the page, with "the page changed since PM OS read it; read it
  again"; otherwise it sends `version + 1`, and a `409` from Confluence (someone saved in
  between) gives the same message. The fake connector keeps a version per page and behaves
  the same way. An edit from the Slack bot can't change `page_id` or `version`. MCP marks
  `confluence_update_page` as destructive, because it replaces the body.
- Caller-supplied ids (issue keys, page ids, Slack channels and timestamps) are validated
  before they reach a URL.

### GitHub

PM OS uses a GitHub App with **user** access tokens only: each PM links GitHub like Atlassian
and Slack, and every GitHub call acts as a PM. Memory reads and writes act as the memory's
owner; content-repo commits act as the platform owner who approved them. The tool server
never uses an installation token, and GitHub limits a user token to what that PM can already
reach.

**Linking** (`/internal/oauth/github/start`): `{PMOS_GITHUB_AUTH_URL}/login/oauth/authorize`
with no scope (the App's permissions apply); the code is exchanged at
`/login/oauth/access_token`. Before anything is stored:
1. `GET /user` gives the login.
2. `GET /user/memberships/orgs/{PMOS_GITHUB_ORG}` must say `state: active`. A pending
   invitation or no membership is refused.
3. After their first link, the GitHub numeric user id must be the one recorded then (`409
   different_account` otherwise). Before it: if the member has a `github_login` in the user
   database, the login must equal it (ignoring case).
   Otherwise one **verified** address from `GET /user/emails` must equal their `atlassian_email`.
4. The login is kept as the credential's `external_id`.

**Tokens** expire in 8 hours; the refresh token in about 6 months, and it rotates. GitHub
answers some errors as `200` with `{"error": …}`: `bad_refresh_token` unlinks the account,
like Atlassian's `invalid_grant`; any other error keeps it and the call fails as unavailable.

**Calls** (`src/github.rs`) send `Accept: application/vnd.github+json`,
`X-GitHub-Api-Version: 2022-11-28` and a `User-Agent`, with a 5 s connect and 20 s overall
timeout. A `401` asks the PM to reconnect (it doesn't unlink). A `403` or `429` with
`Retry-After`, or with `x-ratelimit-remaining: 0` and `x-ratelimit-reset`, is a rate limit,
reported with the wait. A commit of several files uses the git data API (get the ref, its
commit and tree, post blobs, post a tree with `base_tree`, post the commit, then `PATCH` the
ref with `force: false`). A `422` on the ref update means the branch moved: the commit is
rebuilt on the new head, up to 3 more times.

**Setting up the GitHub App** (a platform-owner PM, once):
1. In your GitHub org: Settings → Developer settings → GitHub Apps → New GitHub App.
2. **Callback URL:** `{PMOS_PUBLIC_URL}/oauth/github/callback`. Turn on **Expire user
   authorization tokens**. No webhook.
3. **Permissions:** Repository: Contents read and write, Pull requests read and write (admin
   proposals), Metadata read, and optionally Administration read (only so the admin screens
   can verify branch protection; `docs/policy-admin.md`). Account: Email addresses read.
   Organization: Members read. Nothing else.
4. **Where it can be installed:** only on this account. Install it on your org, on
   all repositories or on the repos PM OS uses: the content repo and the PMs' memory repos.
   With "only select repositories", add each new memory repo to the installation, or its PM
   gets `not_found`.
5. Generate a client secret. Set `PMOS_GITHUB_CLIENT_ID` (the App's Client ID, not its App
   ID) and `PMOS_GITHUB_CLIENT_SECRET`, `PMOS_GITHUB_ORG`, and `PMOS_CONTENT_REPO` for
   approving members. The App's private key isn't needed: PM OS never acts as the App.
6. Each platform owner who approves members links GitHub (`setup` in a Slack DM) and needs
   push access to the content repo. If `main` is protected, allow these commits (the
   platform owners, or the App) to push to it. Each admin who proposes policy or content
   changes links GitHub too: the pull request is opened as them, and branch protection on
   `main` must require someone else's review (`docs/policy-admin.md`, **Nobody approves
   their own change**).

### Personal memory in GitHub

With `PMOS_MEMORY=github`, `memory_read` and `memory_update` use the memory repo of the
session's user, a private repo of their own in the org (`src/github_memory.rs`). The
record of which repo (`memory_repos`) is set with `POST /internal/memory/repo` (the Slack
`setup` checklist does this).
- **Layout:** one file per note, `notes/<key>.md`, on the repo's default branch, with front
  matter: `updated:` (RFC 3339), `source:` (the bot or skill and the run id, such as
  `"pm-ana, task 0d3f…"`) and `sensitivity:` (`public`, `internal` or `restricted`; see
  **Memory**). The rest of the file is the note. A note edited by hand stays readable: no
  front matter, or a missing or unknown `sensitivity:`, reads as `internal`.
- **Reads** list the tree recursively and fetch only the blobs whose sha changed since the last
  read (a per-repo cache in memory); an unchanged tree fetches no blob.
- **Writes** commit only that file, with the message `memory: <key> (<source>)`. The first
  note in an empty repo goes in through the contents API, which also creates the branch.
- **Re-verification:** a record checked more than 10 minutes ago is checked again. A repo that
  became public, was archived, left the org or lost push access refuses reads and writes with a
  tool error naming the reason. A failed check doesn't renew the record.
- **Tool errors:** no repo set gives `Set up your memory repo first: DM PM OS "setup".`; GitHub
  not linked asks the PM to link it.
- With `PMOS_MEMORY=store` (the default), memory stays in Postgres (or in memory) as before.

### Members

Someone not in the user database asks in Slack; a platform owner (an active `admin` in the
user database) approves or rejects there (`src/onboarding.rs`, `docs/onboarding-spec.md`).
Requests are stored in `member_requests`; at most one is pending per Slack id, and at most 20
in all. The proposed handle comes from `propose_handle`, avoiding every handle in the user
database (deactivated people's too: handles are never reused) and pending requests. A Slack
id that belongs to a deactivated user is `409 deactivated` ("your access was turned off"),
not a new request.

**Approving** (since slice U3, `docs/user-database-spec.md` §4) first **creates the user in the
database** (`active`, the display name from the request, cleaned; the Slack identity; the
request's email as both `sign_in_name` and `atlassian_email`), so the person can use PM OS at
once. Only the Slack bot's token may decide (the service token gets `403` since the review of `b469d5b`, UM1); it names the deciding admin unsigned, so the join gets
**`pm` only**: `admin` is `403 admin_not_from_slack` (was `admin_by_pull_request`), any other
role `403 role_not_from_slack`, audited as `denied`, the request still pending, nothing
written. The sign-in name came through an unsigned client, so it is **unconfirmed**: the
person can use Slack and link Atlassian and GitHub, but can't first-link Entra until an admin
confirms it in the web app (slice U5). An address that belongs to another user is `409
already_member`.

**Then** it commits, in **one commit** on `PMOS_CONTENT_BRANCH` with the approving admin's own
GitHub token:
- `{PMOS_CONTENT_DIR}/bots/pm-<handle>/BOT.md` from `render_personal_bot` (if one is already
  at the branch head, nothing is committed and the request's `error` says the handle is
  taken);
- `{PMOS_POLICIES_DIR}/bots/pm-<handle>.yaml` and `pm-<handle>.sandbox.yaml`, the bot's sandbox
  policy and launch spec from `pmos_policy`, so `pmos policy check` has nothing missing.

The message is `Add <handle> to PM OS` with the trailer `Approved-by: <admin> (Slack, request
<id>)`. Since slice U7 there is no `team.yaml` in it: nothing reads one. Before committing,
the content with the new bot in it (a scratch copy of the checkout) must pass `pmos lint`,
since the policies are generated from it. The handle must pass `valid_new_handle` and be
unused (`422 bad_handle` otherwise). Decisions run one at a time, so two admins clicking at
once make one commit; the store also lets only one decision win.

**If the commit doesn't happen** (the admin hasn't linked GitHub, can't push, the branch
keeps moving, GitHub fails), access doesn't wait for it: the user stays active, the request is
`approved` with `commit: null` and an `error` saying why the personal bot is missing, and the
answer is `200`. An admin retries the bot later (slice U5's
`POST /internal/admin/users/{handle}/personal-bot`).

**POL-2, for these policy files:** they are generated unchanged from the already-reviewed
`pm-assistant` role template, and the approving platform owner is their reviewer of record,
named in the `Approved-by` trailer. Any hand change to them still goes by pull request with a
second owner.

**Removing** (`/internal/members/{handle}/remove`) is a **deactivation** since slice U3; nothing
is committed (the answer is `{handle, deactivated: true, commit: null, note}`). See
**Deactivation** below. Refused: `403 not_admin`, `403 self_removal`, `404 not_a_member`,
`409 owns_bots` (someone who still owns a team bot: its owner must change first), `403
protected_not_from_slack` (an Admin or Auditor: host or web only), `429 deactivation_limit`,
`409 already_deactivated`, `409 last_admin`.

**Audit:** every decision and removal is an event with `tool` `members.add` or
`members.remove`, `user` the admin, the caller (`bot` from Slack), outcome `approved`,
`rejected`, `ok` or `denied`, and `output_ref` the commit sha when there is one; the user
store records its own `users.create` / `users.deactivate` events (and `denied` ones with the
refusal code).

### The tool server decides from the user database (slice U3)

Since slice U3 the tool server reads its own users tables (`src/users/access.rs`, `People`)
for every decision it makes; it never reads a `team.yaml`, and since slice U7 onboarding no
longer writes one and no other service reads one either.

- **Who may use PM OS:** an active user holding `pm`, `reviewer` or `admin`. An auditor-only
  person never gets an MCP session, never decides a draft, never links an account. A team
  bot's identity (`bots.*`) is never a user.
- **Fresh for decisions:** session creation, decisions, draft saves, account links, admin
  reads, onboarding and the audience check read the store each time (`UserStore::snapshot`,
  one consistent read). An **open MCP session** is re-checked on every call against a
  directory at most 5 seconds old; a write through this server empties that cache, and a
  deactivation also deletes the person's sessions in its own transaction, so it dies at once
  even when a host command made it.
- **No open directory:** with fake connectors and no seed, nobody may use PM OS; the dev and
  eval stacks seed the in-memory store (`PMOS_USERS_SEED`, which must name an admin). An open
  directory exists for tests only (the `test-support` feature, never in a release binary).
  At start-up the server logs a warning when the user database is empty.
- **The audience check** for a restricted Slack draft: insiders are active users holding
  `pm`, `reviewer` or `admin`, found by their Slack identity; a deactivated or auditor-only
  person is an outsider. With the user database unreachable, nothing is posted
  (`503 audience_unavailable`).
- **The console's single-item read:** an assertion subject who isn't the item's PM reads it
  only when they are an active admin in the database, read for that request.
- **Admin reads** (`app/admin.rs`): the viewer's roles come from the database (a deactivated
  person's kept roles grant nothing); `accounts` lists every user with `status` and `roles`;
  `overview` adds `held_drafts` and `users` (below). A person holding Admin and Auditor under
  the pilot exception has every audit-log read and export recorded with the detail
  `dual_role admin+auditor`; once the exception has lapsed, those reads are `403
  role_conflict` (admin powers stay).
- **The overview's `users`:** counts `active` and `deactivated`; `sign_in_unconfirmed` (joined
  through Slack, Microsoft sign-in name not confirmed); `dual_role`; `pilot_exception`
  (`ends`, `ending_soon` within 14 days, `lapsed`) and `role_conflicts` once lapsed;
  `entra_needs_host`; `identity_conflicts` (`users.identity_conflict` events in the last 7
  days) and, since U5a, `identity_conflict_users` (whose sign-in names they presented);
  `resolve_miss_warning` (clients past `resolve_miss_warning_threshold`, 100, resolve misses in
  an hour, counted in this process since it started); `deactivations_last_hour`,
  `deactivation_limit`, `deactivation_limit_reached`, `deactivation_grace_hours`;
  `bots_without_active_owner`; and, since the review of `b469d5b` (U1), `first_links`: each
  `users.identity_link` the first-link route recorded in the last 7 days, `{handle, at,
  provider: "entra"}`, newest first (`null` when the audit log can't be read back). Handles
  and counts only, never an address, Slack id or `oid`.
  `pmos-web` serves them field by field (`docs/web-api.md`, `GET /api/admin/overview`).

**Account linking** checks the account against the database (`src/oauth.rs`): the person must
be an active user; Slack: the member id must be their Slack identity; **Atlassian and GitHub:
after the first link the account id decides** (the Atlassian `account_id` from `/me`, the
GitHub numeric user id), stored as an identity (`users.identity_link`); a link that signs in
with a different account is refused `409 different_account` (the callback page) and nothing
is stored, until an admin unlinks it (slice U5a: `DELETE
/internal/admin/users/{handle}/identities/{atlassian|github}`, which deletes the linked account
too). Before the first link, the Atlassian email
must equal their `atlassian_email`, and the GitHub login their `github_login` (or, with none,
a verified email equal to their `atlassian_email`). The Atlassian account id is kept as the
credential's `external_id`.

### Deactivation

One transaction (Postgres; in memory, the same steps under one lock), then a 24-hour grace,
then clean-up, each recorded in `role_changes` and the audit log:

| What | At deactivation | After 24 hours, unless reactivated |
| --- | --- | --- |
| Status | `deactivated` | — |
| `admin` | Revoked (`revoke`, detail `on deactivation`); reactivation never restores it (`grant-admin` on the host) | — |
| MCP sessions | Deleted | — |
| Linked accounts | **Suspended** (`credentials.suspended_at`, migration 0017; `suspend_credentials`): the vault never unseals a suspended credential, so no connector call can use it | **Deleted** by the sweeper (`delete_credentials`, `users.deactivate` detail `credentials deleted`) |
| Pending drafts | **Held**: still pending, but a decision is refused because the owner isn't an active user | **Closed** as `rejected` by `system`, ledger reason `owner_deactivated`; nothing is written |

**Reactivation** within the grace restores the accounts (`credentials restored`) and the held
drafts can be decided again; after the grace the accounts and drafts are gone, whether or not
the sweeper ran first. The sweeper runs every 10 minutes beside the draft sweeper and does
nothing if the database is unavailable.

**The cap:** at most `PMOS_USERS_DEACTIVATIONS_PER_HOUR` (1 to 10, default 3) deactivations in
any rolling hour from the web and Slack clients together, counted from `role_changes` under
the users lock. The next is `429 deactivation_limit`, recorded as a `denied` event; a refused
attempt writes no `role_changes` row, so it never counts, and a reactivation deletes nothing,
so it never resets the window. Host commands aren't capped. From the Slack bot client an Admin
or Auditor is never deactivated (`403 protected_not_from_slack`).

### The user database: the store (slice U1)

The source of users and roles is the tool server's own tables (`docs/user-database-spec.md`);
`team.yaml` was retired in slice U7, and only the import below parses one. Slice U1 builds the
store, its database guarantees and the host commands. Since slice U3 the tool server decides
from it (above), and the runtime since slice U4a, Slack since slice U4b, `pmos-web`'s sign-in
since U4c and the console since U4d.
The read API (slice U2)
is below.

**Tables** (migration 0015): `users` (handle, display name, `sign_in_name` for Entra and
`atlassian_email` as separate columns, GitHub login, status), `user_roles` (`admin`, `pm`,
`reviewer`, `auditor`), `user_identities` (Slack, Entra `(tid, oid)`, Atlassian and GitHub
ids; one person per key, one key of each kind per person; Entra links carry
`host_confirmed`), `role_changes` (the authoritative history), `pilot_exceptions` and
`directory_version` (the directory's ETag for U2). Handles are never reused, users are never
deleted, and addresses are unique across every user, active or not.

**The database is the guarantee.** A deferred constraint trigger, `users_invariants()`, runs
at commit for any change to `users`, `user_roles`, `user_identities` or `pilot_exceptions`.
It takes a transaction-scoped advisory lock first, refuses any isolation level but READ
COMMITTED (so the checks read a fresh snapshot after the lock), and then refuses:
- a commit that leaves users but no active admin (`last_admin`), so two concurrent demotions
  can't both commit, whatever the application does;
- an active person holding `admin` and `auditor` unless a pilot exception is open and not past
  `expires_at` (`dual_role`); checked for the people the change touched, so a lapsed exception
  blocks other changes to such a person but never the ones that resolve it (revoking either
  role, deactivating them, `off --revoke-auditor`);
- the `evals` account holding any role but `pm`, or any identity (`evals_account`).

`role_changes` is append-only by its own trigger, which never reads the audit purge's
`pmos.audit_purge` setting: `UPDATE` and `TRUNCATE` are always refused, and `DELETE` only from
inside `purge_role_changes()` (which sets `pmos.role_changes_retention`), only for a row about
a user deactivated more than 90 days ago, never a `*` row. The retention job runs daily with
the audit purge and records its own run (`users.role_changes_purge`, client `system`).

**`UserStore`** (`src/users/`) has a Postgres and an in-memory implementation that behave the
same: every write is planned against the current directory under the same lock, the planned
result is checked against the same invariants (the trigger's, written again in Rust), and the
write, its `role_changes` rows and its audit events go in one transaction. In Postgres the
plan's check only gives a clear refusal first; the trigger is what holds. A refused write
changes nothing and records a `denied` audit event with the refusal's code. The rules for the
later web and Slack routes are already in the planner and tested: admin is granted on the host
only; from Slack, `pm`-only joins with an unconfirmed sign-in name, and never an Admin's or
Auditor's deactivation; no one changes their own roles, status or addresses; deactivation
revokes `admin` and reactivation never restores it; at most 3 deactivations an hour from web
and Slack together, counted from `role_changes` (a refused attempt writes no row, so it never
counts; a reactivation deletes none, so it never resets the window).

**Host commands:** `pmos-toolserver users …`, run by `deploy/pmos-admin users …` in a one-off
container (`docs/runbooks/users.md`):

| Command | Does |
| --- | --- |
| `bootstrap-admin --handle --sign-in-name --atlassian-email --slack-id (--oid \| --no-entra) [--name] [--github]` | The first admin (`pm`, `admin`); exit 3 once any active admin exists |
| `grant-admin <h> (--oid \| --keep-entra)` | The only way to make an admin; sets or confirms the host-confirmed Entra link in the same transaction |
| `revoke-admin <h>` | Break glass; refused for the last admin |
| `set-entra <h> --oid` / `unset-entra <h>` | A host-confirmed Entra link (`tid` from `PMOS_ENTRA_TENANT_ID`) |
| `set-sign-in-name <h> <name>` / `set-atlassian-email <h> <email>` | Setting the sign-in name also confirms it |
| `deactivate <h>` / `reactivate <h>` | Not capped; deactivation revokes `admin` |
| `pilot-exception admin-and-auditor on --until <date> \| off [--revoke-auditor] \| status` | At most 90 days; `off` refused while anyone holds both unless `--revoke-auditor` |
| `import --from <file> [--commit <sha>] [--allow-admins \| --bootstrap] [--apply]` | A dry run unless `--apply`; idempotent; conflicts reported, never overwritten; admins only with `--allow-admins` (confirmed on a terminal) or `--bootstrap` (an empty table only, deactivated rows included); the file's SHA-256 and commit recorded |
| `export --format team-yaml` | The database as a `team.yaml`, for a rollback |
| `list` | Handles, status, roles, flags |

Every command needs `PMOS_HOST_USER` (the OS user; `pmos-admin` passes
`${SUDO_USER:-$(id -un)}`) matching `^[a-z_][a-z0-9_.-]{0,31}$`, and exits 2 without it. Writes
record it in `role_changes.host_user` and the audit event's detail (`host_user <name>; …`,
client `host`). Exit codes: 0 done, 1 failed, 2 usage, 3 refused by a rule. No command prints a
token; `list`, `export` and the import's dry run show addresses, which the host already holds.

**The dev seed:** `PMOS_USERS_SEED=<file>` (the old `team.yaml` shape, loaded by the import's
code as a bootstrap) fills the in-memory store at start-up. It is refused with
`PMOS_DATABASE_URL`, with live connectors, and (slice U7) when the file is inside
`PMOS_CONTENT_ROOT`: users are never read from the content. With fake connectors only, it may give `evals` its fake Slack id. A seed
needs an active admin like any import.

### The user database's read API (slice U2)

Three routes read the user database for the other services (`src/app/directory.rs`). The
runtime polls `directory` since slice U4a (`docs/bots-runtime.md` "Who may ask"); `pmos-web`
calls `resolve` and the first link since slice U4c (sign-in and every request, through
`crates/directory`'s `resolve` client, cached 10 seconds per lookup); the Slack service polls
`directory` with the bot token since slice U4b (`crates/slack/src/people.rs`); the console calls
`resolve` on every request since slice U4d (`docs/console.md`).
Each route has its own allow-list, checked before anything reads the request; any other
token is `403`, none is `401`.

| Route | Tokens | Answers |
| --- | --- | --- |
| `GET /internal/directory` | service (the runtime), bot (Slack) | `{version, users: [...]}`, every user, deactivated ones included. `ETag: "<version>"` (strong; the database's `directory_version`, bumped by every committed change to users, roles, identities or the pilot exception); `If-None-Match` with it (also `W/"…"`, a list, or `*`) answers `304` with no body. `Cache-Control: private, no-cache` |
| `GET /internal/users/resolve?by=entra&tid=&oid=` / `?by=sign_in_name&name=` / `?by=handle&handle=` | web, console | One user, or `404 not_found`. `by=sign_in_name` matches the stored lower-case name (`name` is trimmed and lower-cased). A malformed lookup (unknown `by`, a `tid`/`oid` that isn't a GUID, a name that isn't an address, a handle that can't exist) is `422 invalid` and isn't a miss |
| `POST /internal/users/{handle}/identities/entra` `{id_token, nonce}` | web | The first Entra link (below), from the raw ID token, which this server validates itself: `200 {first_link, user}` (`first_link: false` when the user already had exactly this key: nothing written). `501 not_configured` without `PMOS_ENTRA_TENANT_ID` and `PMOS_ENTRA_CLIENT_ID` |
| `GET /internal/users/entra-link` | web | `{configured, tenant_id, client_id}`: the tenant and application first-link tokens are validated for (`null`s and `false` without them). `pmos-web`'s `/readyz` compares them with its own (`entra_link`) |

**Projections** (fixed in code, tested field by field):

| Client | Fields per user |
| --- | --- |
| Service token (`directory`) | `handle`, `display_name`, `status`, `roles` |
| Bot token (`directory`) | the same and `slack_id` (or `null`) |
| Web and console (`resolve`, and the link's `user`) | `handle`, `display_name`, `status`, `roles`, `entra` (`none`, `linked`, or `needs_host` for an Admin or Auditor whose link the host hasn't confirmed), `sign_in_name_confirmed`, `dual_role` |

No answer ever carries a sign-in name, an Atlassian email, a GitHub login, a `tid`, an `oid`,
or an Atlassian or GitHub id; only the Slack bot gets Slack ids; web and the console can't
list. `roles` are the stored rows: a deactivated user's kept roles grant nothing, so every
caller checks `status` first. Display names are untrusted text (render as plain text).

**No user assertion.** `resolve` and the first link are how a person gets a session, so there
is no signed-in subject to assert (spec §2); the routes answer the same in every assertion
mode and don't read `X-Pmos-Assertion`. What bounds a stolen web or console token instead:
one user per call, no addresses, and two limits. The **miss limit**: each client may get at
most 20 misses in any rolling minute (`404`s from `resolve`, and for `pmos-web` every refusal
of the first-link route, not only its `404`s: review of `b469d5b`, UM2); past that a miss
answers `429 {code: "resolve_misses"}` with `Retry-After`. The **hit limit** (UM2): at most
600 `resolve` answers that found someone, per client, in any rolling minute; past that `429
{code: "resolve_rate"}` with `Retry-After`. 600 is generous on purpose: `pmos-web` and the
console cache each answer 10 seconds per person, so a signed-in person costs at most 6 a
minute, and 600 covers 100 people active at once (the pilot is a few dozen) with room for
sign-ins; a stolen token can confirm at most 600 names, handles or `oid`s a minute. Windows
are per client (web and console apart) and in process. Every miss is
logged with its kind (`entra`, `sign_in_name`, `handle`, `first_link`), never the value; more
than 100 in an hour from one client logs a warning, and the admin overview shows it
(`resolve_miss_warning`, slice U5a).

**The first Entra link** (spec §7 step 2). Since the review of `b469d5b` (U1) the body is
`{id_token, nonce}`, nothing else (anything more, or the old `{tid, oid, sign_in_name}`, is
`422 invalid`): the raw ID token Microsoft issued to `pmos-web` and the `nonce` of that
sign-in. The route **validates the token itself** (`pmos_entra::Verifier`, the one copy of the
rules `pmos-web` applies): RS256, signed by a key in the tenant's JWKS (from the tenant's
OpenID configuration, cached a day, refetched on an unknown `kid` at most once a minute, a
failed fetch not retried for 5 seconds, 4 seconds per fetch), `iss` the tenant's v2.0
issuer, `aud` exactly `PMOS_ENTRA_CLIENT_ID`, `exp`/`nbf`/`iat` within 60 seconds, the
`nonce`, `tid` = `PMOS_ENTRA_TENANT_ID`, `oid` a GUID, `acct` `0`, `idp` this tenant or
absent. A token that fails is `422 invalid_token` (`422 entra_misconfigured` without `acct`,
`403 not_a_member` for a guest or another identity provider); the tenant's keys unreachable is
`503 entra_unavailable` (nothing linked: fail closed). Each token is good for **one** first
link: a SHA-256 of its `(oid, iat)` is kept by the user store until the token expires (in
Postgres, `entra_used_tokens`, migration 0023, so a restart doesn't forget it), and the same
token again is `403 token_used`. The `(tid, oid)` and the sign-in name come only from the
validated token; a token with no usable `preferred_username` is `404 not_found`. Every refusal
(`501 not_configured` and `503 entra_unavailable` included) counts as a miss first: past the
miss limit it is `429 resolve_misses` and not audited; below it, it is audited. A deactivated
user, `evals`, and an Admin or Auditor are refused even when already linked with this key. Then one transaction, under the users lock, re-reads the directory and
checks: the user exists and the token's `preferred_username` (never `email`) is their stored
sign-in name, else `404 not_found`; they
have no Entra identity yet (`409 linked` if a different one; the same key is `200` with
`first_link: false`); no one else holds the key (`409 taken`); they are active (`403
not_active`), not the `evals` account (`403 evals_account`), hold neither `admin` nor
`auditor` (`403 host_only`: the host links those, `set-entra`), and their sign-in name is
confirmed (`403 sign_in_unconfirmed`: a Slack join waits for an admin's confirmation in the
web app). Then it stores `(tid, oid)` (lower-case, `host_confirmed` false, `linked_client`
`link`), a `role_changes` row (`link`, `entra`, by the user through `web`) and the audit event
`users.identity_link` (`entra`), and the version moves. Two links racing for one key: the
unique key `(kind, issuer, subject)` lets exactly one commit, and `(handle, kind)` does the
same for two keys racing for one person; the loser gets `409 taken` or `409 linked`.

**Audited:** a link as `users.identity_link` (`ok`, actor the user, detail `entra`); a refused
one as `users.identity_link` (`denied`, actor `anonymous`, detail `entra; <code>`, the target
handle only when that user exists); a conflict (`linked`, `taken`) as
`users.identity_conflict` (`denied`, detail `entra`). No address, `tid` or `oid` in any event
or log line. `resolve` and `directory` record no audit events (they are reads that run on
every sign-in and every poll).

### The Admin API (slice U5a)

`src/app/users.rs`. The routes behind the web app's Team screen (`pmos-web` mirrors them
under `/api/admin/users…`; the full request and response contract, every field and error, is
`docs/web-api.md` **Admin: users and roles**), plus the console's admin listing. Every rule
is `crate::users::rules`, the same the host commands and the Slack client go through; the
route only parses, gates and answers.

| Route | Body | Does |
| --- | --- | --- |
| `GET /internal/admin/users?user=<viewer>` | | Every user: `{version, view, users, deactivations, pilot_exception, linking}`. `manage-members` (the admin view, with `sign_in_name`, `atlassian_email`, `github_login`) or `view-audit-log` (the auditor view: those three keys left out). Never an `oid`, `tid`, Slack id or account id. Audited `admin.users`. |
| `POST /internal/admin/users?user=<admin>` | `{handle, display_name?, sign_in_name, atlassian_email, github_login?, slack_id?, roles}` | Invite: `201`, `active`, sign-in name confirmed. Never `admin` (`403 host_only`). |
| `PUT /internal/admin/users/{handle}/roles?user=` | `{roles, expected_version}` | The whole set. |
| `PUT /internal/admin/users/{handle}/fields?user=` | `{sign_in_name?, atlassian_email?, github_login?, display_name?, expected_version}` | All or nothing (`Op::SetFields`). |
| `POST /internal/admin/users/{handle}/sign-in-name/confirm?user=` | `{sign_in_name, expected_version?}` | Confirm, or replace and confirm (§4). |
| `POST /internal/admin/users/{handle}/deactivate?user=`, `/reactivate` | none or `{expected_version?}` | §6 **Deactivation** (above); `409 owns_bots` while they own a team bot. |
| `DELETE /internal/admin/users/{handle}/identities/{entra\|atlassian\|github}?user=` | | `Op::Unlink`: an `atlassian` or `github` identity takes its linked account with it (§6: "an admin unlinks first"). |
| `PUT /internal/admin/reviewers/{reviewer}?user=` | `{pms, expected_version?}` | The whole set of PMs assigned (§8; `reviewer_assignments`, migration 0018). |
| `GET /internal/console/members?user=<admin>` | | The console's token only: `{handles: [...]}`, active users holding `pm`, `reviewer` or `admin`, for a viewer who is an active admin here (else `403 forbidden`). Its assertion is checked as on the console's approvals reads. Audited `admin.console_members`. |

**Who may call.** The Admin API is `pmos-web`'s token only: the service (orchestrator),
console and Slack bot tokens get `403`; the Slack bot keeps only its `pm`-only join approval
and capped `remove` (**Members**). The console listing is the console's token only.

**Writes** (every method but `GET`):

1. **Never on the development override.** Unless this server runs in assertion `require`
   (so never with `PMOS_ASSERTION_INSECURE_OFF=1`), a write answers `503
   {"code":"assertion_required"}` before anything else, recorded as a `denied` event.
2. **A signed assertion for the actor, in every mode:** present, valid, bound to the request
   with its body (at most 16 KiB), and its subject the `user=` the call names; else `401
   bad_assertion`, recorded.
3. **The actor is checked in this server's tables:** an active admin (`403 forbidden`),
   never from the assertion's roles.
4. **The rules** (spec §6): nobody changes their own roles, status, addresses, links or
   reviewer assignments (`403 self_change`); `admin` is never granted here (`403
   host_only`); an Admin's or Auditor's sign-in name, Atlassian email, GitHub login and Entra
   link are host-only (`403 host_only`); an Atlassian email or GitHub login changes only
   while no account of that kind is linked (`409 linked`); addresses are unique across
   everyone (`409 taken`); the last admin (`409 last_admin`) and Admin + Auditor outside the
   pilot exception (`409 dual_role`) are refused by the plan and, in Postgres, by the
   `users_invariants()` trigger; the cap (`429 deactivation_limit`, counted from
   `role_changes`: a refused attempt writes no row, a reactivation deletes none, and the host
   is never counted); reactivation never restores `admin`.
5. **`expected_version`** (required on roles and fields, optional elsewhere): once every rule
   above passes, a directory whose version moved since refuses the write `409 stale`
   (`rules::plan_expecting`), before the invariants. The version is the read API's ETag:
   bumped by every committed change to users, roles, identities, the pilot exception and
   (migration 0018) reviewer assignments.
6. **Audited, allowed or refused:** the store writes the `role_changes` rows and the audit
   event in the write's transaction, or a `denied` event with the refusal's code; a refusal
   before the store (a body that doesn't parse, the override, a bad assertion, `owns_bots`)
   is recorded by the route (actor `anonymous` when no assertion was verified). Event details
   are fixed formats only (`users::detail`): field names, `sign_in_name confirmed`, role
   deltas, `<n> pms`, refusal codes; never a value.

Each refusal is `{code, error}`; the status is `refusal_status`: `422 invalid`, `404
not_found`, `403` for `forbidden`, `host_only`, `self_change`, `429 deactivation_limit`, and
`409` for the rest (`exists`, `taken`, `unchanged`, `stale`, `last_admin`, `dual_role`,
`evals_account`, `not_active`, `linked`, `owns_bots`, `not_reviewer`, `not_pm`). A write
answers the target as an admin sees them and the new version: `{version, user, note?}`.

**A lapsed pilot exception** never blocks what resolves it, and never leaves the stack without
a working admin: the plan and the trigger check only the people a write touches, and a
deactivated person never counts as holding both, so revoking either role or deactivating the
person always goes through, and a person holding both keeps their admin powers (their writes
on others go through) until one role is gone; only their audit-log reads are refused (`403
role_conflict`). Tested on both stores (`tests/admin_users.rs`).

**Reviewer assignments** (§8): `reviewer_assignments (reviewer, pm, assigned_by,
assigned_at)`, both handles users, never the same person; changes bump the directory version
under the users lock (migration 0018). The reviewer must be active and hold `reviewer`
(`409 not_active`, `409 not_reviewer`), each PM added active and holding `pm` (`409 not_pm`),
at most 200, unless the set is empty (clearing is always allowed). Kept, inert, when either
loses the role or is deactivated. Recorded `assign`/`unassign` per PM in `role_changes`
(detail `pm <handle>`) and `reviewers.assign` (`<n> pms`) in the audit log. What a reviewer
then reads and recommends: **Reviews** (below).

**Migration 0018** also widens `role_changes.client` to allow `link`: a person's own first
Atlassian or GitHub link (`Op::LinkAccount`, the OAuth callback) records its row with that
client, which 0015's check refused, so in Postgres every such first link failed until now
(the in-memory store has no check, so U3's tests didn't see it).

### Reviews (slice U6a)

`src/reviews.rs` (types, note cleaning, audit formats), `src/service/review.rs` (who may read
and recommend, what they see), `src/app/reviews.rs` (the routes), the store's
`ApprovalRepo::{recommend, recommendations, pending_of}` in memory and Postgres (migration
0019), `tests/reviews.rs` on both stores. **A Reviewer recommends; the draft's owner
confirms** (`docs/user-database-spec.md` §8). `pmos-web` mirrors the routes under
`/api/reviews…`; the full contract is `docs/web-api.md` **Reviews**.

| Route | Body | Does |
| --- | --- | --- |
| `GET /internal/reviews?user=<reviewer>` | | `{items, total, truncated, pms}`: every pending draft of the PMs assigned to the reviewer (and active), newest first, at most `PENDING_CAP` (1,000). Audited `approvals.review_inbox`. |
| `GET /internal/reviews/{id}?user=<reviewer>` | | One such draft, with its sources (metadata only, as `GET /internal/approvals/{id}/sources`). Audited `approvals.review_read`. |
| `POST /internal/reviews/{id}/recommendation?user=<reviewer>` | `{verdict, note?, draft_revision, args_hash}` | Record a recommendation: `{recommendation, draft}`. Audited `approvals.recommend`, detail `verdict <v> revision <n>`. |

**Who may call.** `pmos-web`'s token only (the service, Slack bot and console tokens get
`403`, recorded as `anonymous`). Every call needs `pmos-web`'s signed assertion whose subject
is the `user=` it names, in **every** assertion mode (`401 bad_assertion`), since a reviewer
reads other people's drafts. A recommendation also needs assertion `require` with no
development override (`503 assertion_required`, as the Admin API's writes).

**Who may review what** (from the user database, each call; never the assertion's roles):

- the reviewer is an **active user holding `reviewer`** (`403 forbidden`; holding `admin` is
  not enough, and an admin can't be assigned to themselves anyway);
- the draft is a draft of a PM **assigned to them** (`reviewer_assignments`, migration 0018)
  who may use PM OS (active, holding `pm`, `reviewer` or `admin`): anyone else's, or a missing
  one, is `404 not_found`, the same answer, so ids can't be probed;
- **never their own** (`403 own_draft`): they decide it as its owner;
- **pending** (`409 not_pending`; `409 expired` for a Slack draft past its time, which expires
  as soon as a reviewer reaches it, as a decision would find it).

**What a reviewer sees.** `{id, owner, owner_name, tool, bot, skill, status, created_at,
expires_at, revision, restricted, sources_sensitivity}` always; and only when the draft isn't
restricted, `args`, `args_hash`, `edited`, `edited_at`, `target_url` (built without the
owner's Atlassian site), `target_channel` (for a public channel only: a private channel's
name and a direct message's other person are left to the owner), `my_recommendation` (their own live one, never
another reviewer's) and, on a single read, `sources` and `sources_available`. **Restricted**
means `sources_sensitivity` `restricted` or unrecorded, which since W6 includes a follow-up
built on restricted conversation history (its session starts at that level): metadata only,
and `403 restricted_draft` on a recommendation. No tasks, traces or routines of others.

**A recommendation** is a row in `approval_recommendations` (migration 0019): `verdict`
(`approve` or `changes`), an optional note, and the `draft_revision` and `draft_sha256` (the
item's full `args_hash`) the reviewer read. The store takes the item's row lock (`SELECT …
FOR UPDATE`, the lock a decision and a saved edit take) and refuses, storing nothing, when
the item is missing, the reviewer's own, not pending, or its `revision` or `args_hash` isn't
the one named: `409 {code: "draft_changed", revision, args_hash}` with what it holds now.
One live row per reviewer per draft (a partial unique index): a new one withdraws the old
(`withdrawn_at`) and clears its note. **It never changes the item**: no status, arguments,
ledger row, grant or connector call; `decide` ignores recommendations and still requires
`decided_by == item.user` (a reviewer's decision on someone else's item is `403`).

`approvals.revision` (migration 0019; the B1 fix added none) is 1 as drafted and goes up by
one whenever the item's arguments change to ones with another hash: a saved edit
(`PUT …/draft`) or an edit decision. Saving the same text again leaves it.

**The note** is untrusted plain text: cleaned on write (`reviews::clean_note`: CR LF to LF, a
tab to a space, other control characters and bidirectional overrides dropped, trimmed; empty
is none; over 1,000 characters is `422 invalid`, never cut), checked again by the table
(no control character but the line feed). It is served to `pmos-web` (the reviewer, and the
draft's owner) and the service token only, never to the Slack bot or the console
(`app.rs` `for_client`), and never written to the audit log or a log line. When a Slack
draft's text is removed (it expires, or a day after its decision: **Slack drafts**), the
notes on it are removed in the same transaction (`note_removed_at`).

**The owner's view.** Every item as the approvals routes answer it carries `revision` and
`recommendations`: each live one as `{id, reviewer, reviewer_name, verdict, draft_revision,
created_at, stale, note_removed, note}` (`note` left out for the Slack bot and the console).
`reviewer_name` is the reviewer's display name, or their handle. `stale` is true when the
item's revision or args hash is no longer the one the reviewer read: "given on an earlier
version". If the recommendations can't be read, the field is left out and logged; a read
never fails for it.

**The decision's record.** A decision's audit event adds, after its own detail, one fixed
part per kind of live advice: `after a recommendation to approve, on this version`, `… on an
earlier version`, `after a recommendation to make changes, …` (so `edited before approval;
after a recommendation to approve, on an earlier version`). The admin and auditor views serve
these (`safe_detail`), and `verdict <v> revision <n>` and the refusal codes.

**Audited.** Each listing, read and recommendation is an event as the reviewer (`client`
`web`, `run_id` `reviews`, the item's id and args hash); each refusal `denied` with its code
(`forbidden`, `not_found`, `own_draft`, `restricted_draft`, `not_pending`, `expired`,
`draft_changed`, `bad_request`, `invalid`, `assertion_required`, or `bad_assertion` as
`anonymous`). These are ordinary audit events: the 90-day purge deletes them like any other
(tested on Postgres through the real purge). Recommendation rows go with their item.

### Password sign-in

Email-and-password sign-in with a required authenticator code (`docs/password-sign-in.md`,
where the flows, limits and threats are): `src/password/` (rules and Argon2id, TOTP, codes,
limits, the flows), `src/app/password.rs` (the routes), the store in memory
(`src/users/memory.rs`) and Postgres (`src/users/password_pg.rs`, migration 0020),
`tests/password.rs` on both stores. The tool server owns the password hash, the authenticator
secret (sealed by the vault, below) and the recovery codes; `pmos-web` forwards what a person
typed and acts on the answer. The full browser contract is `docs/web-api.md` **Password
sign-in**.

| Route | Body | Answer |
| --- | --- | --- |
| `POST /internal/password/sign-in` | `{sign_in_name, password, source?}` | `{pending_token, expires_at}`; every refusal `401 sign_in_failed`; `429 too_many_attempts` from the global limit (its reserve only for an address someone signed in from, decided before the name is looked up), or an account whose correct password was given 10 times in 15 minutes |
| `POST /internal/password/second-step` | `{pending_token, code, source?}` or `{pending_token, recovery_code, source?}` | `{user, password_epoch, recovery_code_used, recovery_codes_left}`; `401 code_failed` (`attempts_left`), `401 sign_in_expired`, `403 not_a_member`, `429 too_many_attempts` |
| `POST /internal/password/setup/begin` | `{sign_in_name, setup_code, source?}` | `{setup_token, handle, kind, expires_at}`; `401 setup_failed` (a wrong guess also counts against the person's live code: void at 20), then `403 not_a_member`, `409 sign_in_unconfirmed`, `403 host_only`, `409 evals_account`, `409 setup_code_spent` (5 setups started or 6 passwords set with the code) |
| `POST /internal/password/setup/password` | `{setup_token, password}` | `{next: "authenticator"}`; `422 weak_password` (`reason`), `401 setup_expired` (also past 3 per token), `409 setup_code_spent`, `429 too_many_attempts` (the account's work, or the global limit's open allowance: never the reserve) |
| `POST /internal/password/setup/authenticator` | `{setup_token}` | `{otpauth_uri, secret, account, issuer, algorithm, digits, period}`; `409 wrong_step`, `401 setup_expired` (also past 5 per token) |
| `POST /internal/password/setup/confirm` | `{setup_token, code, source?}` | `{user, password_epoch, kind, recovery_codes}`; `401 code_failed`, `409 wrong_step`, `403 host_only` |
| `POST /internal/password/change?user=<handle>` | `{current_password, code, new_password}` | `{password_epoch}`; `403 reauth_failed`, `422 weak_password`, `409 no_password`, `429 too_many_attempts` (the wait, the account's work, or the open allowance) |
| `POST /internal/password/recovery-codes?user=<handle>` | `{code}` | `{recovery_codes}`; `403 reauth_failed`, `409 no_password`, `429 too_many_attempts` |
| `GET /internal/password/status?user=<handle>` | | `{enabled, password_set, second_step_enrolled, setup_code_pending, recovery_codes_left, needs_host_setup, reset_notice: {by, by_name, at} \| null}` |
| `POST /internal/password/reset-notice/dismiss?user=<handle>` | `{}` | `204` |
| `POST /internal/password/setup-codes?user=<admin>` | `{handle}` | `201 {setup_code, handle, kind, expires_at}`; `403 forbidden`, `403 host_only`, `403 self_change`, `404 not_found`, `409 not_active`, `409 sign_in_unconfirmed`, `409 evals_account` |

- **`pmos-web`'s token only:** the orchestrator's, the console's and the Slack bot's get `403`,
  no token `401`.
- **Only in assertion `require`**, with no development override: `503 assertion_required`
  otherwise. The five routes that name `user=` need `pmos-web`'s assertion for that person,
  bound to the request (`401 bad_assertion`); the six before a session take none (as
  `resolve`, nobody is signed in yet).
- **`source`** is the browser's address as `pmos-web` counts it (an IPv4 address, or an IPv6
  `/64`): the per-account waits are per address (`docs/password-sign-in.md`, "Limits"). It is
  hashed before it is stored and never logged; missing, those attempts share one count.
- **Off without `PMOS_VAULT_KEY`** (`501 not_configured`); with live connectors it is required
  anyway. The host command `pmos-toolserver users password-setup <handle>`
  (`pmos-admin users password-setup`) issues a code for anyone but `evals`, Admins and
  Auditors included, and needs no key.
- **`resolve` for `pmos-web`** carries `password_epoch` (the credential's epoch, `null` without
  one), so a password session ends when a reset or a change moves it. The console's answer
  doesn't. `GET /internal/admin/users` gives each user `password_set`,
  `second_step_enrolled` and `setup_code_pending`.
- Bodies are at most 4 KiB; answers are `no-store`; refusals carry fixed sentences. Nothing is
  logged or audited but fixed words and counts (`password::is_safe_detail`, which the admin
  audit views accept).

### Content sync

Every `PMOS_CONTENT_SYNC_SECS`, and right after a member commit, the tool server runs
`git -C <repo root> pull --ff-only` in the repo holding `PMOS_CONTENT_ROOT` (found with
`git rev-parse --show-toplevel`), with `tokio::process`. If HEAD isn't the commit last loaded,
it reloads like `POST /internal/reindex`, with the same lint gate: content that fails is
pulled but not loaded, and is tried again on the next sync. Failures are logged and never stop
the server. The checkout's own git setup is used (such as a deploy key on the Spark); the tool
server holds no git credentials and never waits on a prompt (`GIT_TERMINAL_PROMPT=0`).
`pmos-slack` and `pmos-bots` read the same checkout, so they see a change as soon as the pull
lands.

### The vault

Tokens are sealed with AES-256-GCM before any store sees them (`src/vault.rs`, table
`credentials`).
- Each ciphertext is bound to its user, provider and field, so a sealed token copied into
  another user's row won't decrypt.
- `PMOS_VAULT_KEY` is a comma-separated list of `key_id:base64key` (32-byte keys). The first key
  seals; every listed key can unseal.
- **To rotate:** put the new key first and keep the old one listed. Rows are re-sealed under the
  new key on their next read, and the old key can be dropped once nothing uses it.
- Tokens never appear in logs, audit events, API responses or `Debug` output (sessions and
  the bot's access print `<redacted>` for theirs).
- Password sign-in's authenticator secrets are sealed with the same keys and format, bound to
  their person and use (`pmos-vault|<handle>|password|totp`), and re-sealed under the newest
  key when next used (`docs/password-sign-in.md`).

## Storage

Every store sits behind an async trait (`crates/toolserver/src/store.rs`). Each has an in-memory
implementation for tests and `PMOS_DEV=1`, and a Postgres one for real use
(`crates/toolserver/src/pg.rs`, schema in `crates/toolserver/migrations/`).

| Store | Postgres table | Notes |
| --- | --- | --- |
| Context index | `context_docs` | Full-text search (title weighted over body), rebuilt from the content repo on start-up and on `POST /internal/reindex`. Embeddings are added once the gateway serves the embedding model. |
| Memory | `memory_notes` | One row per user and key, with the note's `sensitivity` (migration 0009; null for older notes, read as `internal`) |
| Sessions | `sessions` | Only the SHA-256 of each token is stored. Each names a `skill` or a `bot` (a check constraint keeps it to one) |
| Grants | `grants` | Taken with `DELETE … RETURNING` on a row locked `FOR UPDATE SKIP LOCKED`: one grant, one call, even under concurrency |
| Approvals | `approvals` | A decision locks the item (`SELECT … FOR UPDATE`) and changes it only while it is `pending`: exactly one of several concurrent decisions wins. A Slack reply is created under an advisory lock per user and thread. The Slack draft sweep takes items `FOR UPDATE SKIP LOCKED`, leaving one a decision holds to the decision. A saved edit takes the same lock. `drafted_args` and `edited_at` (migration 0012) keep the bot's own arguments and the last save, once a person changed them |
| Feedback ledger | `ledger` | One row per approval item, written in the same transaction as the item on create, decide, complete and expire. `expired` is stored for Slack drafts and worked out on read for the rest |
| Audit | `audit_events` | Append-only: triggers reject `UPDATE`, `DELETE` and `TRUNCATE`. Each event records the caller (`mcp`, `orchestrator`, `bot`, `web` or `console`; migration 0011) and the session's skill or bot, and for a read the titles of its non-Slack sources (`source_titles`, null before migration 0010; see **What an item is shown with**). |
| Credentials | `credentials` | Tokens sealed by the vault; the database never sees plaintext. Providers `atlassian`, `slack`, `github` |
| Memory repos | `memory_repos` | One row per PM: `repo` (`org/name`) and `verified_at` |
| Member requests | `member_requests` | A partial unique index keeps one `pending` request per Slack id; a decision is a conditional `UPDATE … WHERE status = 'pending'`, so exactly one wins |
| Teams | `user_bots` | Migration 0024: one row per person and persona (never `nudge`), `added_at`, `added_via`; at most 16 per person (a deferred constraint trigger). A change takes an advisory lock per person and writes its `team.change` audit event in the same transaction. `sessions.persona`, `approvals.persona` (fixed by a trigger) and `audit_events.persona` are `null` for Nudge and for every row from before. See **Personas and teams** |
| Users | `users`, `user_roles`, `user_identities`, `role_changes`, `pilot_exceptions`, `directory_version` | Migration 0015; see **The user database**. The invariants are a deferred trigger that takes an advisory lock; `role_changes` is append-only with its own retention rule. Audit events of host commands have client `host`. |

- **Migrations** run on start-up.
- **Pruning:** expired sessions and grants are deleted every 10 minutes.
- **Retention of Slack text:** approval items keep a Slack draft's text at most
  `PMOS_SLACK_DRAFT_TTL_HOURS` (24) after creation if undecided, or after the decision; the
  sweeper runs every 10 minutes (see **Slack drafts**). The ledger and the audit log never
  hold it.
- **Shutdown:** SIGTERM (what `docker compose stop` sends) or Ctrl-C stops new requests and lets in-flight ones finish. Give the container a stop grace period of at least 30 seconds.
- **MCP sessions** stay open while idle for up to the session TTL (60 minutes), so a long model step between tool calls doesn't drop the connection.
- **One replica.** OAuth sign-in states, refresh locks, MCP sessions, the lock that runs member decisions one at a time and the memory-repo cache are in process memory, so run a single tool server. Running two needs those moved to Postgres first; this is tracked in the implementation plan.
- **Audit failures:**
  - **Writes fail closed:** they don't run unless their `started` event is recorded.
  - **Reads:** a missing event is logged as `AUDIT EVENT NOT RECORDED` at error level, and the read isn't undone.
- **Database roles (recommended):** run migrations as an owner role, and give the tool server's role only `SELECT, INSERT` on `audit_events`. The triggers already stop the owner from rewriting history; the separate role also stops anyone from dropping them through the app's connection.
- **Tests:** `tests/stores.rs` runs the same contract cases against both backends, including the concurrency cases.
  - Set `PMOS_TEST_DATABASE_URL` to run the Postgres side. CI does.
  - Without it, the Postgres cases are skipped locally and fail in CI.

## Running it

```sh
PMOS_SERVICE_TOKEN=$(openssl rand -hex 32) \
PMOS_CONNECTORS=fake \
PMOS_DATABASE_URL=postgresql://pmos@localhost/pmos \
cargo run -p pmos-toolserver
```

| Variable | Default | Notes |
| --- | --- | --- |
| `PMOS_SERVICE_TOKEN` | required | The orchestrator's token. At least 32 characters |
| `PMOS_BOT_TOKEN` | none | The Slack bot's own token: at least 32 characters and different from the service token |
| `PMOS_CONSOLE_TS_TOKEN` | none | The web console's own read-only token (approvals only): at least 32 characters and different from every other token. The console gets it as `PMOS_TOOLSERVER_TOKEN` |
| `PMOS_BOT_SKILLS` | `docs` | Skills the bot may start sessions for |
| `PMOS_WEB_TOKEN` | none | `pmos-web`'s own token, for the approvals routes and the read-only admin routes. At least 32 characters, different from every other token |
| `PMOS_ASSERTION_MODE` | `require` with `PMOS_WEB_TOKEN` or `PMOS_CONSOLE_TS_TOKEN` set, else `off` | `off`, `verify` or `require`: see **Approvals auth: user assertions**. `verify` and `require` refuse to start without a key for each set token; `off` and `verify` refuse to start with either token set |
| `PMOS_ASSERTION_INSECURE_OFF` | none | `1`: FAKE-DATA DEVELOPMENT STACKS ONLY. Lets the tool server start in `off` or `verify` with a web or console token set; logs `INSECURE` at error level. Refused (the tool server won't start) with `PMOS_CONNECTORS=live`, or with `PMOS_DATABASE_URL` set without `PMOS_DEV=1`; `deploy/compose.yaml` doesn't pass it |
| `PMOS_ASSERTION_KEYS` | none | `web:toolserver:<kid>:<base64>[:<kid>:<base64>],console:toolserver:…` (`docs/user-assertion.md`, Keys). A malformed list, a key under 32 bytes, or another service's pair refuses to start, in every mode |
| `PMOS_CONNECTORS` | required | `live` for real Atlassian and Slack with each PM's own token; `fake` for fixture data (dev, tests, and everything before Gate 1). Either way, only active users in the user database (a seed in dev) may have sessions |
| `PMOS_USERS_DEACTIVATIONS_PER_HOUR` | `3` | How many people the web app and Slack together may deactivate in any rolling hour (1 to 10); past it, `429 deactivation_limit` and host commands only (see **Deactivation**) |
| `PMOS_ENTRA_TENANT_ID`, `PMOS_ENTRA_CLIENT_ID` | none | The Entra tenant and application (GUIDs; the same as `pmos-web`'s `PMOS_WEB_ENTRA_*`): the first-link route validates ID tokens for them itself (see **The user database's read API**). Both, or the tenant alone (the host commands' setting: the route then answers `501 not_configured`); a client id without a tenant refuses to start |
| `PMOS_ENTRA_AUTHORITY` | `https://login.microsoftonline.com` | The authority the tenant's discovery document and keys are fetched from: an origin, `https` unless loopback (tests) |
| `PMOS_USERS_SEED` | unset | Development and eval stacks only (refused with a database or live connectors): a file in the import's shape (a `members:` list), outside `PMOS_CONTENT_ROOT`, loaded into the in-memory user database at start-up; it must name an admin |
| `PMOS_DATABASE_URL` | required | Postgres for every store. `PMOS_DEV=1` without it keeps everything in memory. |
| `PMOS_AUDIT_LOG` | none | Also writes audit events to this JSON-lines file, for shipping off-box |
| `PMOS_CONTENT_ROOT` | `content` | Must pass `pmos lint`, or the server won't start |
| `PMOS_BIND` | `127.0.0.1:8710` | |
| `PMOS_ALLOWED_HOSTS` | `toolserver.pmos.internal,localhost,127.0.0.1` | `Host` values `/mcp` accepts |
| `PMOS_CONTENT_SYNC_SECS` | `300` with live connectors, `0` with fake | How often the content checkout is pulled (see **Content sync**); `0` turns it off, after member commits too |
| `PMOS_LEDGER_EXPIRE_DAYS` | `7` | Days after which a draft still pending is reported as `expired` in the feedback ledger. A whole number, 1 or more |
| `PMOS_SLACK_DRAFT_TTL_HOURS` | `24` | Hours after which an undecided Slack draft expires, and a decided one loses its text after its decision. A whole number from 1 to 24: no Slack text is kept longer |

With `PMOS_CONNECTORS=live`:

| Variable | Default | Notes |
| --- | --- | --- |
| `PMOS_VAULT_KEY` | required | `key_id:base64key[,…]`; generate a key with `openssl rand -base64 32` |
| `PMOS_PUBLIC_URL` | required | `https://` URL PMs' browsers reach; callbacks are `{url}/oauth/{provider}/callback`, registered with each app |
| `PMOS_ATLASSIAN_CLIENT_ID`, `PMOS_ATLASSIAN_CLIENT_SECRET` | required | From the Atlassian developer console (OAuth 2.0 integration) |
| `PMOS_ATLASSIAN_SCOPES` | `read:me`, Jira classic, Confluence granular read/write and search | `offline_access` is always added; keep `read:me`, which the link check needs |
| `PMOS_ATLASSIAN_SITE` | none | Needed only if a grant covers several sites |
| `PMOS_SLACK_CLIENT_ID`, `PMOS_SLACK_CLIENT_SECRET` | required | From the internal Slack app |
| `PMOS_SLACK_SCOPES` | search (`search:read.public`, `.private`, `.im`, `.mpim`), history (`channels:history`, `groups:history`, `im:history`, `mpim:history`), channel info and members for the audience check (`channels:read`, `groups:read`, `mpim:read`, `im:read`) and `chat:write` | Requested as `user_scope` only. A PM linked before the `:read` scopes were added must link Slack again: until then the audience check answers `503 audience_unavailable` ("reconnect Slack") |
| `PMOS_ATLASSIAN_AUTH_URL`, `PMOS_ATLASSIAN_API_URL`, `PMOS_SLACK_URL` | the real hosts | For pointing at mocks. `https://` only; `http://` only to a loopback host (review of `b469d5b`, L10), else the server refuses to start |
| `PMOS_GITHUB_CLIENT_ID`, `PMOS_GITHUB_CLIENT_SECRET` | none | From the GitHub App (see **GitHub**). Set both or neither; without them GitHub linking, `PMOS_MEMORY=github` and approving members are off |
| `PMOS_GITHUB_ORG` | required with GitHub | Your GitHub org's login. Memory repos and the content repo must be in it |
| `PMOS_GITHUB_URL` | `https://api.github.com` | API base, for mocks. `https://`, or `http://` to loopback only (L10) |
| `PMOS_GITHUB_AUTH_URL` | `https://github.com` | `/login/oauth/authorize` and `/login/oauth/access_token`, for mocks. `https://`, or `http://` to loopback only (L10) |
| `PMOS_CONTENT_REPO` | none | `org/name` of the content repo, in `PMOS_GITHUB_ORG`. Required for approving and removing members, and for admin proposals |
| `PMOS_CONTENT_BRANCH` | `main` | The branch member commits go to, and admin proposals are made on |
| `PMOS_CONTENT_DIR` | `content` | Where the content root sits in that repo (`.` for its root, only with `PMOS_PROPOSALS=off`) |
| `PMOS_POLICIES_DIR` | `policies` | Where the sandbox policies sit in that repo, from its root |
| `PMOS_PROPOSALS` | `on` with `PMOS_CONTENT_REPO` | Admin policy and content proposals (**Admin proposals**). `on` refuses to start when the content or policies folder is the repo root or one is inside the other; `off` turns proposals off |
| `PMOS_PROPOSALS_PER_HOUR` | `10` | Proposals per admin per hour, 1 to 100, in memory |
| `PMOS_POLICIES_ROOT` | the checkout's policies folder | The checked-in policy files the proposal re-check compares with |
| `PMOS_MEMORY` | `store` | `store` keeps memory in Postgres (or in memory). `github` puts each PM's memory in their own repo and needs the GitHub settings; it is a start-up error without them |
| `PMOS_DOCS_URL` | required | The `pmos-docs` MCP endpoint, such as `http://docs.pmos.internal:8730/mcp` |
| `PMOS_DOCS_TOKEN` | required | The bearer token `pmos-docs` expects |

`PMOS_DEV=1` (everything in memory) is refused with `PMOS_CONNECTORS=live`, so real tokens and the audit log can't end up in a process that forgets them.

The OAuth `state` signing key is derived from `PMOS_SERVICE_TOKEN`, so rotating the service
token invalidates sign-in links in flight.

After a merge to the content repo, the content sync pulls and reloads within
`PMOS_CONTENT_SYNC_SECS`. To do it at once, or with the sync off, pull the checkout and call
`POST /internal/reindex`:
- If the content fails `pmos lint`, the call returns `422` and the index keeps its last good state.
- Bot and role changes (tools, action settings, read scope) take effect at once.
- Skill and tool-registry changes take effect on the next restart.

## Not yet

- **`pmos-admin ledger`** (the summary, and `--export` as JSON lines) lives with the deployment
  kit; until it does, call `GET /internal/ledger/summary` and `GET /internal/ledger` with the
  service token.
- **First real calls** with real tokens and data, on the Spark after Gate 1. The sandbox network policy (0f) must allow only the tool server and gateway, not these hosts.
- **Embeddings** in the context index, once the gateway (0d) serves the embedding model.
- **Paging** past the first page of results (`nextPageToken`, `cursor`). Skills ask for small result sets for now.
