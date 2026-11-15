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
| External content is data, not instructions | Every Jira, Confluence and Slack item comes back wrapped as `{"untrusted": true, "source": …, "content": …}` | Untrusted content |
| Nothing is written outside PM OS without approval | A write runs only with a matching grant or an inbox approval by its own user; otherwise it returns `pending_approval` and does nothing | OUT-4 |
| Everything is audited | One event per call and per decision: user, skill, run, tool, outcome, sources read, argument hash, output reference. Never message bodies | ADM-2, SLK-8 |
| Arguments match the tool's schema | Required keys, no unknown keys, types and enums checked before any handler runs | — |
| A bad draft gets one correction | A write tool refused twice for invalid arguments in a session is refused for the rest of it, without reaching the connector (see **Invalid drafts**) | — |
| Every draft is labelled | Each approval item has a row in the feedback ledger: what became of it, never its text (see **Feedback ledger**) | — |
| A Slack post reaches only who it may | Before a Slack write runs, its channel's members are checked against `team.yaml` and the draft's sources: restricted sources never reach outsiders, and a Slack Connect channel needs a second confirmation (see **Slack drafts**) | SLK-6 |
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

## Endpoints

| Endpoint | Auth | Purpose |
| --- | --- | --- |
| `GET /healthz` | none | Liveness: the process answers |
| `GET /readyz` | none | Readiness: `200` when storage answers within a second and every connector's own dependency (the docs server) answers; `503` otherwise, naming the check that failed |
| `POST /mcp` (Streamable HTTP) | `Bearer <session token>` | MCP for the harness: `tools/list`, `tools/call` |
| `POST /internal/sessions` `{user, skill, run_id, ttl_minutes?}` | service or bot token | Create a session; returns `token` and the session's tools. `403` if the user isn't in `team.yaml` (live connectors), or the bot asks for a skill outside `PMOS_BOT_SKILLS`. |
| `POST /internal/sessions` `{user, bot, run_id, ttl_minutes?}` | service token | Create a bot session (see **Bot sessions**). Send `skill` or `bot`, never both (`400`). `404` for an unknown bot; `403` if `user` may not use it. |
| `DELETE /internal/session` + `X-Pmos-Session` | service or bot token | Revoke a session (sandbox ended) |
| `POST /internal/call` + `X-Pmos-Session` `{tool, args}` | service or bot token | One tool call for the session, with the same rules as MCP: allowlist, argument checks, approvals for writes, audit (which records the caller). |
| `POST /internal/grants` `{session_token, tool, args}` | service token | Record an approval given in the harness |
| `GET /internal/approvals?user=&status=` | service token | `{items: […]}`: a PM's inbox, newest first. `status` is `pending`, `executing`, `approved`, `rejected`, `failed` or `expired` (`400` for anything else) |
| `GET /internal/approvals?user=` | bot token | The App Home Inbox: `{approvals: […], total}`, only `pending` items whatever `status` says, oldest first, at most 50. Each item reads as `GET /internal/approvals/{id}` returns it; `total` counts every pending item. `403` if `user` isn't a team member (live connectors) or is a team bot's identity; `400` without `user` |
| `GET /internal/approvals/{id}` | service or bot token | One item: `id`, `user`, `skill` or `bot`, `run_id`, `tool`, `args`, `args_hash`, `status`, `created_at`, and when set `decided_at`, `decided_by`, `result`, `sources_sensitivity` and `redacted_at` |
| `POST /internal/approvals/{id}/decision` `{decided_by, decision, args?, reason?, confirm_external?}` | service or bot token | `approve`, `edit` (with `args`) or `reject`. `decided_by` must be the item's user (`403`), and in `team.yaml` when it is enforced (`403`). `reason` is optional and only with `reject` (`422` with anything else, or for an unknown value): `wrong_facts`, `wrong_target`, `tone`, `incomplete`, `should_not_draft` or `other`. The audit records which token decided, and the reason in `detail` (`reason: tone`). For a Slack write, `confirm_external: true` confirms posting to a Slack Connect channel. Slack answers, each `{code, error, …}`: `409 audience_blocked` (with `outsiders`, `members`, `external`), `409 needs_external_confirm` (with `members`, `external`), `503 audience_unavailable`, and `409 expired`; see **Slack drafts**. |
| `GET /internal/ledger?since=&until=&bot=&outcome=&min_edit=&limit=` | service or bot token | `{rows: […]}`, newest first. See **Feedback ledger**. `422` for a bad parameter |
| `GET /internal/ledger/summary?since=&bot=` | service token | Per bot (or skill) and tool: counts by outcome, label coverage, median edit distance, rejection reasons. `422` for a bad `since` |
| `POST /internal/reindex` | service token | Rebuild the context index from the content checkout and re-read `team.yaml`, the bots and their roles; `422` if it fails `pmos lint` |
| `GET /internal/oauth/{atlassian\|slack\|github}/start?user=` | service or bot token | The URL to send a PM to so they can link their account. The bot token gets URLs for team members only (`403` otherwise). `user=bots.<name>` (service token) gives the URL for a team bot's own account (`404` unless `<name>` is a team bot); GitHub links PMs only. |
| `GET /oauth/{atlassian\|slack\|github}/callback` | none (the PM's browser) | Finishes linking; shows a short page |
| `GET /internal/credentials?user=` | service token | A PM's linked accounts, without tokens |
| `DELETE /internal/credentials/{user}/{provider}` | service or bot token | Unlink (the PM disconnects, or a platform owner revokes): `204`, or `404` if nothing was linked. The bot token may unlink team members' accounts only (`403` otherwise, never a team bot's own), for the App Home's **Disconnect**. Audited as `credentials.unlink` with the client, `output_ref` the provider |
| `POST /internal/memory/repo` `{user, repo}` | service or bot token | Check `repo` (`org/name`) with the PM's GitHub token and record it as their memory repo: `200 {repo, verified_at}`. `422 {error, code}` with `not_in_org`, `not_private`, `archived`, `no_push`, `not_found` or `bad_request`; `409 github_not_linked`; `504 github_unavailable` if GitHub doesn't answer within 1.8 s (it runs inside a Slack modal); `404 github_off` without GitHub |
| `GET /internal/memory/repo?user=` | service or bot token | `{repo, verified_at}`, or `404` |
| `POST /internal/members/requests` `{slack_id, email, name}` | service or bot token | A request to join: `201 {request}`, or `200 {request}` with the one already pending for that Slack id. `409 already_member`, `422 bad_request`, `429 too_many_pending` (past 20) |
| `GET /internal/members/requests?status=` | service or bot token | `{requests: […]}`, oldest first; `status` is `pending`, `approved`, `rejected` or `failed` |
| `POST /internal/members/requests/{id}/decision` `{decision, decided_by, handle?, roles?}` | service or bot token | `approve` or `reject`, by an `admin` in `team.yaml` (`403 not_admin`). See **Members**. `200 {request}`; `409 already_decided` with `request`; `422 bad_handle`; `409 github_not_linked`, `no_push` or `not_configured` (still pending); `502 commit_failed` with `request` (now `failed`) |
| `POST /internal/members/{handle}/remove` `{decided_by}` | service or bot token | Admin only; not oneself (`403 self_removal`). `200 {handle, commit}`, `404 not_a_member`, `409 owns_bots` if they still own a team bot |
| `GET /internal/onboarding?user=` or `?slack_id=` | service or bot token | A PM's setup checklist: `member`, `handle`, `admin`, `linked` (`atlassian`, `slack`, `github`), `memory_repo`, `personal_bot` (in the loaded content), and for a non-member by Slack id, `pending_request` |

**Who holds which token:**
- **Service token** (`PMOS_SERVICE_TOKEN`): the orchestrator, the bot runtime and platform-owner tools. Every route, and the only token that starts bot sessions.
- **Bot token** (`PMOS_BOT_TOKEN`): the Slack bot. Only sessions for the skills in `PMOS_BOT_SKILLS` and calls within them, plus reading and deciding one approval item (`GET /internal/approvals/{id}` and its `decision`), for Slack approval cards, the onboarding routes, `GET /internal/ledger` for the daily triage digest, and for the App Home (`docs/slack-home.md`) a team member's pending items (`GET /internal/approvals?user=`) and unlinking a team member's account (`DELETE /internal/credentials/{user}/{provider}`). A session started for another skill, or any bot session, looks to the bot like one that doesn't exist, and every other route (the ledger summary among them) returns `403`.
- **Session token:** each sandbox gets only its own.

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
  text. Every field may go to the Slack bot.
- **`expired`** is stored for a Slack draft that expired (see **Slack drafts**), with no
  `decided_at`. For every other tool it is worked out on read: a row still `pending` and
  never decided is reported as `expired` once it is older than `PMOS_LEDGER_EXPIRE_DAYS`
  (default 7). An approved write still running stays `pending` until its outcome is stored.
- **`edited`** means the decision changed the item's arguments (their hash differs). An
  `edit` decision that sends back the same arguments is `approved`.
- **The edit distance** is the normalized character-level Levenshtein distance between the
  original and the final text, divided by the longer one's length. The text is the item's
  string arguments, at any depth, in key order, joined with newlines. Each side is cut to its
  first 20,000 characters, and a shared prefix and suffix are skipped first, so small edits to
  long drafts stay cheap.
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
- Jira, Confluence, Slack and memory reads: `internal`;
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
   any member isn't in `team.yaml` (by Slack id), or the channel is shared outside the
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
(`audience_blocked: 2 not in team.yaml, external: false`, `needs_external_confirm`); an
unavailable check as `error` (`audience_unavailable: …`). Rejecting is never checked. A write
approved in the harness with a grant doesn't go through this check.

The members count against `team.yaml` as loaded, enforced or not: with fake connectors and an
empty `team.yaml`, everyone is an outsider.

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
| `user` | the PM the task is for. A personal bot serves only its owner (`403` otherwise); a team bot, any team member (`team.yaml`, when enforced). Never a `bots.*` identity. |

Sessions, approval items and audit events record either `skill` or `bot`, never both; a bot
session's `skill` is `null`.

**Who a bot reads and writes as.**
- **Personal bots** read as their owner, with the owner's linked accounts and no scope.
- **Team bots** read Jira, Confluence and Slack as the bot's own service identity,
  `bots.<name>`. A platform owner links its read-only accounts with
  `GET /internal/oauth/{provider}/start?user=bots.<name>` (service token), then opens the
  URL signed in as the bot's account. The `team.yaml` identity check is skipped only for
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
| `bets.list` | read | `status?` | `{"bets": [{key, title, status, review_by, updated_at}]}`, soonest review first |
| `bets.read` | read | `key` | `{"bet": {…}}`; an unknown key is not found |
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
  Reading a bet raises the session to `internal`.
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
3. The tool server exchanges the code and checks the account belongs to that PM in
   `team.yaml`: the Slack user must be the member's `slack-id`, and the Atlassian account's
   email (from `/me`, scope `read:me`) must be the member's `email`. For GitHub, see
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
  If someone saved a newer version first, the update fails with "page changed since PM OS
  read it". MCP marks `confluence_update_page` as destructive, because it replaces the body.
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
3. If the member has `github` in `team.yaml`, the login must equal it (ignoring case).
   Otherwise one **verified** address from `GET /user/emails` must equal their `email`.
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
3. **Permissions:** Repository: Contents read and write, Metadata read. Account: Email
   addresses read. Organization: Members read. Nothing else.
4. **Where it can be installed:** only on this account. Install it on your org, on
   all repositories or on the repos PM OS uses: the content repo and the PMs' memory repos.
   With "only select repositories", add each new memory repo to the installation, or its PM
   gets `not_found`.
5. Generate a client secret. Set `PMOS_GITHUB_CLIENT_ID` (the App's Client ID, not its App
   ID) and `PMOS_GITHUB_CLIENT_SECRET`, `PMOS_GITHUB_ORG`, and `PMOS_CONTENT_REPO` for
   approving members. The App's private key isn't needed: PM OS never acts as the App.
6. Each platform owner who approves members links GitHub (`setup` in a Slack DM) and needs
   push access to the content repo. If `main` is protected, allow these commits (the
   platform owners, or the App) to push to it.

### Personal memory in GitHub

With `PMOS_MEMORY=github`, `memory_read` and `memory_update` use the memory repo of the
session's user, a private repo of their own in the org (`src/github_memory.rs`). The
record of which repo (`memory_repos`) is set with `POST /internal/memory/repo` (the Slack
`setup` checklist does this).
- **Layout:** one file per note, `notes/<key>.md`, on the repo's default branch, with front
  matter: `updated:` (RFC 3339) and `source:` (the bot or skill and the run id, such as
  `"pm-ana, task 0d3f…"`). The rest of the file is the note.
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

Someone not in `team.yaml` asks in Slack; a platform owner (`admin` in `team.yaml`) approves
or rejects there (`src/onboarding.rs`, `docs/onboarding-spec.md`). Requests are stored in
`member_requests`; at most one is pending per Slack id, and at most 20 in all. The proposed
handle comes from `propose_handle`, avoiding `team.yaml` and pending requests.

**Approving** commits, in **one commit** on `PMOS_CONTENT_BRANCH` with the approving admin's
own GitHub token:
- `{PMOS_CONTENT_DIR}/team.yaml`: the file at the branch head plus the new member
  (`handle`, `slack-id`, `email`, `roles`, default `[pm]`), written with `render_team_yaml`;
- `{PMOS_CONTENT_DIR}/bots/pm-<handle>/BOT.md` from `render_personal_bot`;
- `{PMOS_POLICIES_DIR}/bots/pm-<handle>.yaml` and `pm-<handle>.sandbox.yaml`, the bot's sandbox
  policy and launch spec from `pmos_policy`, so `pmos policy check` has nothing missing.

The message is `Add <handle> to PM OS` with the trailer `Approved-by: <admin> (Slack, request
<id>)`. Before committing, the new `team.yaml` must parse and pass the team lint rules, and
the content with the new member in it (a scratch copy of the checkout) must pass `pmos lint`,
since the policies are generated from it. The handle must pass `valid_new_handle` and be
unused at the branch head (`422 bad_handle` otherwise). Decisions run one at a time, so two
admins clicking at once make one commit; the store also lets only one decision win.

**POL-2, for these policy files:** they are generated unchanged from the already-reviewed
`pm-assistant` role template, and the approving platform owner is their reviewer of record,
named in the `Approved-by` trailer. Any hand change to them still goes by pull request with a
second owner.

After the commit, the in-memory team includes the new member at once (so the Slack welcome
checklist works before any pull), and the content checkout is synced (below). A commit that
fails for any other reason marks the request `failed` with a readable `error` (`502`).

**Removing** (`/internal/members/{handle}/remove`) commits `team.yaml` without the member and
deletes their `BOT.md` and its two policy files, in one commit. Then every linked account is
unlinked and their memory-repo record deleted; the repo itself is theirs and is left alone.
The in-memory team drops them at once. Someone who still owns a team bot can't be removed
(`409 owns_bots`): the bot's owner must change first, or the content would fail the lint.

**Audit:** every decision and removal is an event with `tool` `members.add` or
`members.remove`, `user` the admin, the caller (`bot` from Slack), outcome `approved`,
`rejected`, `failed`, `ok` or `denied`, and `output_ref` the commit sha.

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
- Tokens never appear in logs, audit events, API responses or `Debug` output.

## Storage

Every store sits behind an async trait (`crates/toolserver/src/store.rs`). Each has an in-memory
implementation for tests and `PMOS_DEV=1`, and a Postgres one for real use
(`crates/toolserver/src/pg.rs`, schema in `crates/toolserver/migrations/`).

| Store | Postgres table | Notes |
| --- | --- | --- |
| Context index | `context_docs` | Full-text search (title weighted over body), rebuilt from the content repo on start-up and on `POST /internal/reindex`. Embeddings are added once the gateway serves the embedding model. |
| Memory | `memory_notes` | One row per user and key |
| Sessions | `sessions` | Only the SHA-256 of each token is stored. Each names a `skill` or a `bot` (a check constraint keeps it to one) |
| Grants | `grants` | Taken with `DELETE … RETURNING` on a row locked `FOR UPDATE SKIP LOCKED`: one grant, one call, even under concurrency |
| Approvals | `approvals` | A decision locks the item (`SELECT … FOR UPDATE`) and changes it only while it is `pending`: exactly one of several concurrent decisions wins. A Slack reply is created under an advisory lock per user and thread. The Slack draft sweep takes items `FOR UPDATE SKIP LOCKED`, leaving one a decision holds to the decision |
| Feedback ledger | `ledger` | One row per approval item, written in the same transaction as the item on create, decide, complete and expire. `expired` is stored for Slack drafts and worked out on read for the rest |
| Audit | `audit_events` | Append-only: triggers reject `UPDATE`, `DELETE` and `TRUNCATE`. Each event records the caller (`mcp`, `orchestrator` or `bot`) and the session's skill or bot. |
| Credentials | `credentials` | Tokens sealed by the vault; the database never sees plaintext. Providers `atlassian`, `slack`, `github` |
| Memory repos | `memory_repos` | One row per PM: `repo` (`org/name`) and `verified_at` |
| Member requests | `member_requests` | A partial unique index keeps one `pending` request per Slack id; a decision is a conditional `UPDATE … WHERE status = 'pending'`, so exactly one wins |

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
| `PMOS_BOT_SKILLS` | `docs` | Skills the bot may start sessions for |
| `PMOS_CONNECTORS` | required | `live` for real Atlassian and Slack with each PM's own token, and only people in `team.yaml` may have sessions; `fake` for fixture data (dev, tests, and everything before Gate 1), with `team.yaml` not enforced |
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
| `PMOS_ATLASSIAN_AUTH_URL`, `PMOS_ATLASSIAN_API_URL`, `PMOS_SLACK_URL` | the real hosts | For pointing at mocks |
| `PMOS_GITHUB_CLIENT_ID`, `PMOS_GITHUB_CLIENT_SECRET` | none | From the GitHub App (see **GitHub**). Set both or neither; without them GitHub linking, `PMOS_MEMORY=github` and approving members are off |
| `PMOS_GITHUB_ORG` | required with GitHub | Your GitHub org's login. Memory repos and the content repo must be in it |
| `PMOS_GITHUB_URL` | `https://api.github.com` | API base, for mocks |
| `PMOS_GITHUB_AUTH_URL` | `https://github.com` | `/login/oauth/authorize` and `/login/oauth/access_token`, for mocks |
| `PMOS_CONTENT_REPO` | none | `org/name` of the content repo, in `PMOS_GITHUB_ORG`. Required for approving and removing members |
| `PMOS_CONTENT_BRANCH` | `main` | |
| `PMOS_CONTENT_DIR` | `content` | Where the content root sits in that repo (`.` for its root) |
| `PMOS_POLICIES_DIR` | `policies` | Where the sandbox policies sit in that repo, from its root |
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
