# The bot runtime: `pmos-bots`

`pmos-bots` is the server side of PM OS's named bots (0e). It holds every task a PM gives a
bot, from the Slack message to the result, and it survives restarts (D2). Its jobs:
- **Tasks:** a durable queue that harness workers claim, with leases, progress events and a
  full trace.
- **Approvals:** a result with outbound writes waits until the PM has decided every item.
- **Notifications:** one ordered feed that the Slack app long-polls for milestones and results.
- **Routines:** plain-language schedules (D8), parsed by the model and checked in code.
- **Traces:** kept 30 days, with Slack text removed after 24 hours (D6, SLK-8).

The interface is §3 of `docs/bot-runtime-spec.md`; this page is how to run it.

```
pmos-slack ──┐                         ┌──► pmos-toolserver  /internal/sessions, /internal/approvals/{id}
pmos-console ├─► pmos-bots :8750 ──────┤
pmos-evals ──┘    Postgres (bots_*)    └──► pmos-gateway     /v1/chat/completions (pmos/extraction)
                       ▲
                       └── harness worker (one per bot sandbox): claim, heartbeat, events, trace, complete
```

## Running it

**Development** (memory store, nothing survives a restart):
```sh
PMOS_DEV=1 \
PMOS_BOTS_TOKENS="slack=$(openssl rand -hex 24),worker=$(openssl rand -hex 24)" \
PMOS_TOOLSERVER_URL=http://127.0.0.1:8710 PMOS_SERVICE_TOKEN=… \
PMOS_GATEWAY_URL=http://127.0.0.1:8740/v1 PMOS_GATEWAY_TOKEN=… \
cargo run -p pmos-bots
```

**On the Spark:** set `PMOS_BOTS_DATABASE_URL`. Migrations run at start-up and are tracked in
`_pmos_bots_migrations`, so the runtime can share a database with the tool server and the
docs server.

**At start-up, the runtime:**
1. Lints `PMOS_CONTENT_ROOT` with the same lint CI runs (`pmos_contract::lint`), and refuses
   to start on any error. Warnings are counted in the log; `pmos lint` lists them.
2. Loads the bots, their roles and skills, and `tools.yaml`. Later changes are picked up
   without a restart (see **Content changes**).
3. Starts polling the tool server's user directory (see **Who may ask**). Until the first
   answer nobody may ask (`503`) and `/readyz` says `directory: down`.
4. Recovers tasks the last process left `working` (see **Durability**), and runs once any
   routine that came due while it was down.

## Content changes

Every 30 seconds the runtime checks a fingerprint of `tools.yaml`, `bots/**`, `roles/**`,
`skills/**` and `personas/**` under `PMOS_CONTENT_ROOT` (paths, sizes and modification times). A `team.yaml`
is neither watched nor read (slice U7); if one is left in the content, the lint reports it to
the runtime as a warning only (`team-file-retired`), so it never stops a load. When it changes:
- **The tree passes the lint:** it is loaded again, and the new catalog replaces the old one
  in one step. A new bot shows up in `GET /v1/bots` and can take tasks.
- **The tree fails the lint:** the catalog already loaded stays, and the errors are logged
  once (`content changed but fails the lint; keeping the old catalog`). The next change is
  checked again.
- **Tasks already claimed** keep the bot, role and tools they were claimed with, even if
  their bot changes or goes. Tasks still queued, new tasks and routines use the new catalog,
  so a queued task whose bot is gone fails at its claim with "the bot `x` no longer exists".

The content root is the tool server's checkout, which it pulls every `PMOS_CONTENT_SYNC_SECS`
and right after a member is approved (`docs/onboarding-spec.md` §3), so a new member's bot is
listed within about half a minute of the pull.

## A new member's worker (operations, for now)

When a platform owner approves someone in Slack, the tool server adds them to the user
database and commits their `bots/pm-<handle>/BOT.md` and its policy, and `pm-<handle>` appears in `GET /v1/bots` without a
restart. Nothing claims its tasks yet: worker tokens are per bot and live in config, and a
bot's sandbox is started by hand. Until that is automated, a platform owner does this on the
Spark for each new member (the bot is `pm-<handle>`, as `personal_bot_name` makes it):

1. **Policies.** Pull the content repo, then run `cargo run -q -p pmos -- policy generate`
   and `cargo run -q -p pmos -- policy check`. Commit `policies/bots/pm-<handle>.yaml` and
   `.sandbox.yaml`. A new bot is a network change, so it needs a second platform owner's
   review (POL-2, `policies/README.md`). Until it is committed, `pmos policy check` reports
   the bot as `policy-missing`.
2. **Tokens.** Make two new tokens (`openssl rand -hex 24`), never reused from another bot:
   - add `worker-pm-<handle>=<token>` to `PMOS_BOTS_TOKENS`;
   - add `bot-pm-<handle>=<token>` to the gateway's `PMOS_GATEWAY_TOKENS`.
   Restart `pmos-bots` and `pmos-gateway` to load them. Tasks are durable, so a restart loses
   nothing (see **Durability**); a task already queued for the new bot simply waits.
3. **Providers.** Create the bot's two OpenShell providers from those tokens:
   `pmos-pm-<handle>-worker` (profile `pmos-worker`) and `pmos-pm-<handle>-gateway` (profile
   `pmos-gateway`), as in **Credentials** in `docs/sandbox-policies.md`.
4. **Sandbox.** Create `pmos-pm-<handle>` from `policies/bots/pm-<handle>.sandbox.yaml` and
   start its worker, as in **Creating a bot's sandbox** in `docs/sandbox-policies.md`. Then run
   `policies/redteam/isolation.sh`.
5. **Check.** The member DMs PM OS `setup`: step 5 shows their bot once it is in the roster.
   A first DM to it should get "Working on it" within a few seconds; if tasks stay queued, the
   worker isn't claiming (check its token and the runtime log for `401`).

Removing a member (`remove <handle>` in Slack) deletes their `BOT.md`, so the bot leaves the
catalog at the next check. Then stop and delete their sandbox, remove both tokens and
providers, regenerate the policies (the old files are `policy-orphan`), and restart
`pmos-bots` and `pmos-gateway`.

### After a catalog change adds skills: recreate each personal sandbox once

A persona (Chief, Sprout, Echo, or a later one) brings skill folders the personal role
doesn't have. The pull request that adds it regenerates every `policies/bots/pm-*.yaml` and
`.sandbox.yaml` (uploads and read-only paths only; the network policy is byte-identical,
`docs/bot-marketplace-spec.md` §4). OpenShell applies uploads only when a sandbox starts, so
after that pull request merges a platform owner does this **once per personal bot**:

1. Pull the content repo on the Spark; `cargo run -q -p pmos -- policy check` is green.
2. For each `pm-<handle>`: stop its worker, delete the sandbox `pmos-pm-<handle>`, and create
   it again from `policies/bots/pm-<handle>.sandbox.yaml` with the same two providers, as in
   **Creating a bot's sandbox** in `docs/sandbox-policies.md`. Tokens and providers are
   unchanged; nothing in `pmos-bots` or the gateway restarts.
3. Run `policies/redteam/isolation.sh`.
4. Check: ask the new bot once as that PM (after adding it to their team). Until the sandbox is
   recreated, a task for a persona whose skill isn't in it fails with "This bot's skills aren't
   installed in your sandbox yet. A platform owner needs to restart it; Nudge can help
   meanwhile." It never runs with fewer skills or as Nudge.

Offering or withdrawing a persona (`offered:`) changes no policy and needs no recreate.

### Deploy order: the tool server first

Deploy a tool server that knows personas (slice B1 or later) **before** a runtime that sends
them. The runtime requires the tool server's `201` to echo the persona it asked for
(`persona.key`, and the same role); a tool server that ignores the field answers with a Nudge
session, so the runtime revokes that session at once (`DELETE /internal/session`; if that
fails, it lapses at its TTL, unused), fails the task
with "PM OS couldn't start Chief for this request: the tool server didn't run it as Chief. A
platform owner needs to check that the tool server and the bot runtime run the same
version; Nudge can help meanwhile.", and never runs a persona's instructions on a session not
opened for it. Nudge tasks send no `persona` field, so they work with either version.

## Configuration
| Variable | Default | Notes |
| --- | --- | --- |
| `PMOS_BOTS_TOKENS` | required | `name=token,…` with names `slack`, `web`, `worker-<bot>` (one per bot sandbox: it claims and reports only that bot's tasks), `console` and `evals`. A plain `worker` serves every bot and is for development only. Each token is at least 32 characters, and all are distinct. |
| `PMOS_BOTS_DATABASE_URL` | required | Postgres. Not needed with `PMOS_DEV=1`, which keeps everything in memory. |
| `PMOS_DEV` | off | `1`: the memory store |
| `PMOS_BOTS_BIND` | `127.0.0.1:8750` | |
| `PMOS_CONTENT_ROOT` | `content` | Bots, roles, skills, `tools.yaml` |
| `PMOS_TOOLSERVER_URL` | required | The tool server's origin, such as `http://127.0.0.1:8710` |
| `PMOS_SERVICE_TOKEN` | required | The tool server's orchestrator token: it creates bot sessions, reads approvals and reads the user directory (`GET /internal/directory`) |
| `PMOS_GATEWAY_URL` | required | The gateway's OpenAI base, ending in `/v1` (as the harness uses it) |
| `PMOS_GATEWAY_TOKEN` | required | This service's client token in the gateway's `PMOS_GATEWAY_TOKENS` |
| `PMOS_EVALS_AS` | `evals` | Comma-separated handles the `evals` client may request tasks as; any other is `403`. Eval tasks may not name a Slack target (`400`) and send no notifications |

Tokens and the database URL never appear in `Debug` output or the log.

## Who may call what
Every `/v1` call sends `Authorization: Bearer <token>`; the token's client name decides what
it may do. Anything else is `403`.

| Client | May |
| --- | --- |
| `slack` | create tasks (source `slack` or `cli`), read its PM's own tasks and their events, the notification feed, and its PM's routines (parse, create, list, change, delete) |
| `web` | the web app's backend: what `slack` may do for its PM except the notification feed. It creates tasks with source `web` only, and reads its PM's own tasks and events, cancels them, manages their routines, reads the bot list, and reads and sets their member settings. A `web` task has no Slack thread and writes no notifications |
| `worker` | claim, heartbeat, events, trace, complete, fail |
| `console` | read everything, including traces and drafts; pause, resume and delete routines; cancel tasks |
| `evals` | create tasks with `source: "eval"`, and read eval tasks, their events and their drafts |

- **The Slack and web clients act for one PM per call**, named in `X-Pmos-User` (a PM OS handle). The
  call is `400` without it. Another PM's task or routine answers `404`, the same as one that
  doesn't exist.
- **Who may use a bot:** someone who may ask (below); then a personal bot serves its owner
  only and a team bot serves everyone who may ask. A task or routine for anyone else is
  `403`, with the reason.
- **The console** may only switch `enabled` on a routine. Its schedule, prompt and persona are
  the owner's to change.
- **`GET /v1/personas`** (`web`, `slack`, `console`, `evals`; others `403`): the offered
  personas, see **Personas** below.

## Personas (marketplace bots)

`docs/bot-marketplace-spec.md` §6. A persona (Chief, Sprout, Echo) is a role a PM's personal
bot runs as, chosen per ask by the PM. It is a field the client sets from the PM's own choice;
the model never sets it. `null` (and every row from before migration `0007_personas.sql`) is
Nudge, the always-on generalist; `nudge` is never stored.

| Route | Field | Rule |
| --- | --- | --- |
| `GET /v1/personas` | | `200 {personas: [{key, display_name, title, role, mascot, always_on, preview, summary, examples, can_do, reads, drafts, needs: [{source, label, without, met}]}]}`: offered only, Nudge first, then by `order`. From the runtime's catalog (`content/personas/*/PERSONA.md` through the lint). `preview: true` is a bot offered before its benchmark passes ("Preview: not yet benchmarked"). |
| `POST /v1/tasks` | `persona?` | `web`, `slack`, `evals` only (others `403`). Not a key shape: `400 {code: "bad_request"}`. Not in the catalog or not offered: `400 {code: "unknown_persona"}`. The bot not a personal bot of role `pm-assistant`: `400 {code: "persona_needs_personal_bot"}`. `nudge` is stored as `null`. The team is **not** checked here. Left out in a thread: the persona of the thread's latest task **asked by the same requester of the same bot** is inherited (`null` included); naming `nudge` does not inherit. Another person's task in the thread passes nothing on, and neither does a routine's run (routines start no thread), so a reply to a routine's message (Chief's daily brief, say) is Nudge's unless it names a bot. |
| `GET /v1/tasks` | `?persona=<key>` | Only that persona's tasks; `persona=nudge` is Nudge's. `400` if not a key shape. |
| Task, notification | `persona` | Every task carries `persona` (`null` is Nudge). A notification carries it only when not Nudge. The console's metadata view keeps it. |
| `POST /v1/routines` | `draft.persona?` | As for tasks (`400` codes above); `nudge` or absent is Nudge. A key other than `nudge`: `web` only (`slack` `403`), as for `PATCH` below. |
| `PATCH /v1/routines/{id}` | `persona: key \| "nudge" \| null` | The owner only. `null` or `nudge` (back to Nudge): `slack` or `web`. Any other key: **`web` only** (`slack` `403`); the console `403` for any `persona`. Checked against the catalog as above. |

**Who decides the team for a routine's persona** (slice R2, the reviewer's finding on this
`PATCH`). The team lives in the tool server, which lets only `pmos-web` read it, with the
person's signed assertion; the runtime has no way to read it. So: (1) only the `web` client may
set a routine to run as a bot other than Nudge, and `pmos-web` reads the owner's team fresh
before it does (`PUT /api/me/brief`, `docs/web-api.md`) and again after, moving the routine
back to Nudge if the bot left in between; (2) the Slack bot, which can't read a team, may only
move a routine back to Nudge (moving to Nudge widens nothing: Nudge is on every team); (3) the
tool server decides again at **every run** (below): a run refused with `persona_not_on_team` or
`persona_unavailable` pauses the routine. A wrong persona therefore never runs with a bot's
role the owner doesn't have; at worst the routine pauses with the reason in the owner's DM.

**At claim** the runtime checks the persona against its catalog again (withdrawn or deleted:
the task fails with "Chief isn't offered in PM OS any more. Ask Nudge instead."), then asks
`POST /internal/sessions {user, bot, run_id, history_sensitivity?, persona}` (the field only
for a persona). The tool server decides the team:
- **`201`** must echo `persona: {key, display_name, title, role}` with the asked key and the
  catalog's role, else the task fails (**Deploy order**, above). The claim then carries
  `role` = the persona's role (skills, model class, instructions), `gateway.model` =
  `pmos/<that role's model>`, `tools` = the session's tools (registry names), and
  `persona: {key, display_name, title}`. A Nudge claim has no `persona` field and is as before.
- **`403 {code, error}`** with `persona_not_on_team`, `persona_unavailable` or
  `persona_needs_personal_bot`: the task fails with the tool server's `error` sentence ("Chief
  isn't on your team any more. Add it on the Bots page in PM OS, or ask Nudge."). For a
  routine's run with either of the first two codes, the routine is paused (disabled, no next
  run) and the task's error and DM read "Your routine is paused: <sentence>."
- A routine whose persona is withdrawn is paused at its next due run ("Your routine is
  paused: its bot Chief isn't offered in PM OS any more.").

**History:** earlier turns are those with the same bot, requester, thread **and persona**, so
Nudge never sees Chief's turns in a thread and the other way round.

**The daily brief under Chief** (slice R2). The brief is the owner's routine whose prompt starts
with `[daily-brief]`; `pmos-web` sets its `persona` when the PM switches "Chief writes my daily
brief" (Chief's role lists the `daily-brief` skill). Nothing else about it changes: the run is a
routine run as for any persona, its notification carries `persona`, and the Slack service
attributes the DM and sends the brief's block to `pmos-web` with `persona`, so Today's panels
fill and say who wrote it. The Slack service keeps the persona when the PM moves the brief to a
new time (`PATCH {schedule, enabled: true}` on the same routine) and runs `brief now` as it.
Removing Chief from the team moves the brief back to Nudge whatever the PM chose for Chief's
other routines.

**The worker** (`harness/src/worker.ts`) stages the persona role's skills. A skill folder
missing from the sandbox (the window before **recreate each personal sandbox once**) fails the
task with the plain sentence above. The system prompt's first line is "You are Chief, the Chief
of Staff on <handle>'s PM OS team, running in pm-<handle>." after checking the display name and
title are plain one-line text (else the task fails; the key is never put in the prompt). The
PM's own `BOT.md` body is kept, after the persona's instructions, as limits that still apply,
and a "What comes first" section says `content/master/AGENTS.md` and that body take precedence
over the persona's "You are …" line and instructions: restrictions accumulate. The tool server
enforces the tools, writes and data class whatever the prompt says.

## Who may ask

Who exists, their status and their roles come from the PM OS user database
(`docs/user-database-spec.md` §2, slice U4a); there is no team file (slice U7). The runtime polls the
tool server's `GET /internal/directory` every 10 seconds with its orchestrator token and
`If-None-Match` (a `304` confirms the snapshot it holds), through the shared
`pmos-directory` crate. A failed poll keeps the last snapshot **and its age**, so an outage
only narrows what anyone may do.

**May ask:** an active user holding `pm`, `reviewer` or `admin` (the `ask-bot` permission).
An Auditor-only person can't ask, and a deactivated person's kept roles grant nothing.

| Decision | Snapshot at most | Older, or none yet |
| --- | --- | --- |
| Queue an ask (`POST /v1/tasks`, every client) | 5 minutes (the task still needs a tool server session, which decides afresh) | `503` |
| Parse or create a routine; set a PM's level | 30 seconds | `503` |
| A routine falls due: run it, or pause it with the reason if its owner may no longer ask | 5 minutes | This run is skipped (the routine stays on, its next run is scheduled) and logged |
| Pause a deactivated (or no longer allowed) owner's routines, with the "paused" note | 30 seconds; on each new snapshot and every 30-second tick | Waits for a fresh one |
| The console's admin check (another PM's task, events, trace, drafts, cancel, routines) | 30 seconds | Not an admin: the viewer's own tasks and routines only |

`/readyz` reports `directory: ok` only with a snapshot at most 30 seconds old.

## Tasks
```
queued ──claim──► working ──complete──► done
   ▲                 │  └──complete with approvals──► needs_approval ──all decided──► done
   └──lease lapsed───┤
     (first time)    ├──fail, or a second lapse──► failed
                     └──cancel (also from queued)──► cancelled
```
- **Priority** comes from the source: `slack`, `web` and `cli` are `interactive`, routines are
  `scheduled`, evals are `personal`. Claims take interactive work first, then the oldest.
- **Claim** (`POST /v1/worker/claim {worker, bots?, wait_ms?}`): long-polls up to `wait_ms`
  (at most 25 seconds; none if absent). It answers `204` if nothing came in time. The
  runtime then creates the task's tool-server session:
  `POST /internal/sessions {user, bot, run_id: <task id>}`.
  - **Answer:** `{task, bot, role, tools, session_token, lease_until, gateway, level,
    history, history_sensitivity, persona?}` (`persona` and the session's `persona` field
    only for a persona task: see **Personas**). `gateway` is
    `{model: "pmos/<role model>", headers: {X-Pmos-User, X-Pmos-Run, X-Pmos-Priority}}`,
    ready for the harness's gateway client. `history` is the conversation's earlier turns
    (below), `[]` for a first message.
  - **Tool server refuses the session** (for example, the PM may not use the bot): the
    task fails with that reason, and the next task is tried.
  - **Tool server unavailable:** the task goes back to the queue without using up an
    attempt, and the claim is `503`.
  - **Bot gone after a restart or a content change:** the task fails with "the bot `x` no
    longer exists".
- **Worker calls** (heartbeat, events, trace, complete, fail):
  - Each may carry `"worker": "<name>"`; if it does, that worker must hold the claim.
  - A task that is no longer `working` (cancelled, recovered, finished) answers `409` with
    `{"status": …}`. That is the worker's signal to stop.
  - Trace entries are accepted for any claimed task, even after it finished.
- **Events** (`status`, `checklist`, `milestone`): all are kept, and each milestone is also a
  notification.

## Conversations
A follow-up in a thread is claimed with the thread's earlier turns, so "customer" in reply to
"Who is the user?" reads as an answer, not a new request.
- **The thread:** a Slack task's `slack {channel, thread_ts}`, or a `conversation` id the web
  (or Slack) client names on `POST /v1/tasks` (1 to 128 of `A-Z a-z 0-9 . _ : -`; `403` from
  any other client, `400` if malformed). The id is only a label: it is matched within the
  same requester and bot, so naming another PM's id finds nothing. Only interactive tasks
  (`slack`, `web`, `cli`) have a thread; routines always start fresh, and so does an eval task unless it names a `conversation` (a benchmark thread).
- **The turns:** earlier tasks with the same bot, the same `requested_by`, the same
  thread and the same persona (`null` equals `null`), created before this one and no longer `queued` or `working`. Each is
  `{text, result?, error?, status, at}`, oldest first, in the claim's `history`.
- **Bounds:** the newest 6 turns; each `text`, `result` and `error` cut to 4,000 bytes with
  `… [truncated]` after; at most 16,000 bytes in all, the oldest dropped first.
- **Retention:** only turns asked in the last 24 hours, the window Slack text is kept in
  traces (D6), and never one whose text, result or error carries the redaction marker
  (`slack content removed after 24 h`). A history read that fails gives `[]` rather than
  stranding the task.
- **Storage:** `bots_tasks.conversation` (the client's id) and `bots_tasks.thread_key`
  (`c:<conversation>` or `s:<channel>:<thread_ts>`, interactive tasks only), indexed by bot,
  requester, key and persona (migrations `0004_conversations.sql`, `0007_personas.sql`).

## Durability (D2)
- **Leases:** a claim holds the task for 120 seconds, and each heartbeat extends that. The
  harness heartbeats every 30 seconds.
- **Recovery:** on start-up and every 30 seconds after, a `working` task whose lease has
  lapsed is handled:
  - **First lapse** (`attempts < 2`): it goes back to `queued`, with the event "interrupted
    by a restart; retrying".
  - **Second lapse:** it becomes `failed`, with the reason "interrupted twice by restarts;
    ask again", and a `failed` notification.
- **Races:** a heartbeat that lands first wins, and the task is left alone. Every change to
  a task checks its state and writes the change, its event and its notification in one
  transaction.
- **Many workers:** in Postgres, claims use `FOR UPDATE SKIP LOCKED`, so each queued task
  goes to exactly one worker however many poll at once.

## Approvals
`complete {result, approvals: ["apr_…"]}` with any ids makes the task `needs_approval`, and
sends a `needs_approval` notification with the ids.
- **Polling:** every 30 seconds the runtime reads each id with
  `GET /internal/approvals/{id}`, using the service token.
- **Closing:** when none is `pending` or `executing`, the task becomes `done` and an
  `approval_closed` notification says what happened, such as "jira.create_issue was approved
  by ana; slack.send_message was rejected by ana." Its `outcomes` lists each item's `tool`,
  `status` and `decided_by`.
- **Closed states:** `approved`, `rejected`, `failed`, and `expired`: a Slack draft nobody
  decided within `PMOS_SLACK_DRAFT_TTL_HOURS` (24 hours; `docs/toolserver.md`, **Slack
  drafts**). An expired item reads "slack.post_reply expired undecided; nothing was sent",
  with no `decided_by`.
- **Lost items:** an id the tool server no longer knows counts as closed ("no longer in the
  approval queue"), so a task can't wait forever.
- **Outages:** if the tool server can't answer, the check is simply tried again on the next
  tick.

**Drafts** (`docs/improvement-loop-spec.md` §3). `GET /v1/tasks/{id}/drafts` returns
`{drafts: [{id, tool, args, status}]}`, one per approval id the worker reported, in that order.
- **Who may call it:** the `console` (any task) and `evals` (eval tasks only; another task is
  `404`). Slack and workers get `403`.
- **Where it comes from:** the runtime reads each item from the tool server
  (`GET /internal/approvals/{id}`) with its own service token, so the evals client never holds
  a tool-server token. `args` are the item's arguments as they stand (the edited ones once a
  PM edited them); `status` is the item's (`pending`, `executing`, `approved`, `rejected`,
  `failed`, `expired`). A Slack draft's text reads `"[expired]"` once it expired, or a day
  after its decision.
- **Lost items** read `{"id": …, "tool": null, "args": null, "status": "missing"}`.
- **Outages:** if the tool server can't answer, the call is `503`, never a partial list.
- A task that drafted nothing, or hasn't finished, has `{"drafts": []}`.

## Notifications
`GET /v1/notifications?after=<cursor>&wait_ms=<ms>` (Slack only) returns
`{cursor, items}`, oldest first, at most 100 a page.
- **The cursor:** keep the returned one and send it back as `after`. With no `after`, the
  feed starts from the oldest notification kept.
- **Waiting:** an empty page long-polls up to `wait_ms` (at most 25 seconds).
- **Each item:** `{id, task_id, bot, requested_by, slack, kind, text, approvals?, outcomes?, at}`.
  `kind` is one of `milestone`, `done`, `failed`, `needs_approval` or `approval_closed`.
  Routine tasks have `slack: null`; reply to the owner's DM.
- **Order:** numbers are allocated under a row lock held until commit, so they become
  visible in order. A cursor never skips an item written by a slower transaction.
- **Kept:** 30 days, like traces.

## Routines (D8)
- **Parse** (`POST /v1/routines/parse {bot, owner, text, tz}`): asks the gateway
  (`pmos/extraction`, JSON mode, as the PM, interactive) for a routine. Every answer is
  checked in code:
  - the cron has exactly five fields and parses (no `@daily`, no seconds);
  - the time zone is an IANA name; the request's `tz` is the default;
  - the schedule ever runs (`0 8 30 2 *` doesn't);
  - the bot and owner come from the request, never from the model, and the owner must be
    allowed to use the bot;
  - `approval` is `approve` and `output` is `{"kind": "dm"}`, whatever the model said.

  The answer is `{draft, summary}`, where `summary` reads like "Every weekday at 8:00
  London time, Docs bot will summarize overnight #launch." An unclear request, a reply
  that isn't JSON, or one that fails a check is `422 {question}`, with a question Slack can
  ask the PM. A request with a bad `tz` is `400`; a gateway that is down is `503`.
- **Create** (`POST /v1/routines {draft}`): the draft is checked again. Any `approval` other
  than `approve` is `400`.
- **Running:** every 30 seconds, each enabled routine whose `next_run_at` has passed is
  queued as a task (`source: "routine"`, priority `scheduled`, as its owner). Its next run
  is then computed from now. So a routine that missed several slots while the runtime was
  down runs once on start, not once per slot. Each run is claimed against the routine's
  `next_run_at`, so two ticks can't queue it twice.
- **Pause and resume:** pausing clears `next_run_at`. Resuming or rescheduling counts from
  now, so slots that passed while paused don't run.
- **Routines that can no longer run:** if the owner may no longer use the bot, or the bot is
  gone, the routine is paused. A failed task tells the owner why ("Your routine is paused:
  …").
- **Time zones:** schedules follow the zone's own clock, so 8:00 London stays 8:00 London
  across clock changes.

## Traces (D6)
`POST /v1/worker/tasks/{id}/trace {entries: [{kind, body}]}`, with at most 500 entries a
call. The console reads them with `GET /v1/tasks/{id}/trace`.
- **Kinds:** `prompt`, `model_reply`, `tool_call`, `tool_result`, `policy_denial`.
- **Slack text:** an hourly job (also run at start-up) replaces the body of every
  `tool_result` older than 24 hours whose tool is `slack.*` with
  `{"redacted": "slack content removed after 24 h"}`. The tool is read from the body's
  `tool` field (or `name`), in registry (`slack.read_thread`) or harness
  (`slack_read_thread`) spelling. The worker must put it there.
- **Kept:** 30 days, then deleted by the same job.
- **Separate from audit:** the tool server's audit log is untouched; it stays metadata-only
  and append-only.

## Health and shutdown
- **`GET /healthz`:** the process is up.
- **`GET /readyz`:** `200` when all four checks pass, `503` otherwise, with
  `{"ready", "checks": {storage, toolserver, gateway, directory}}`:
  - **storage:** the store answers within 2 seconds;
  - **toolserver:** the tool server's `/readyz` is `200`;
  - **gateway:** `GET {PMOS_GATEWAY_URL}/models` is `200` with this service's token;
  - **directory:** the user directory snapshot is at most 30 seconds old (**Who may ask**).
- **SIGTERM** (or Ctrl-C): long-polls return at once (`204`, or an empty page), the
  background loops stop, and in-flight requests finish before the process exits. A task a
  worker was running keeps its lease and is recovered by the next process if the worker
  doesn't finish it.

## Storage
| Table | Holds |
| --- | --- |
| `bots_tasks` | Tasks, with their lease (`worker`, `lease_until`) and thread (`conversation`, `thread_key`) |
| `bots_task_events` | Each task's events, in order |
| `bots_traces` | Trace entries, with the tool each is about and whether it was redacted |
| `bots_notifications` | The Slack feed, numbered from `bots_counters` |
| `bots_routines` | Routines, with `next_run_at` and `last_run_at` |

## Testing
`cargo test -p pmos-bots` runs:
- **Store contract tests** against memory and Postgres, including two workers racing for
  one task and concurrent notifications;
- **API tests** with a mock tool server and gateway: the lifecycle, approvals, long-polls,
  routine parsing with valid and invalid model replies, and the auth matrix;
- **Clock-driven tests:** lease recovery (both branches), the scheduler (due, missed once,
  paused, can no longer run), and redaction and retention.
- **Catalog tests:** the seeded content loads, a tree with lint errors refuses to start, a
  content change adds a bot to `GET /v1/bots` while a claimed task keeps its bot, and a
  change that fails the lint keeps the old catalog.

Postgres cases need `PMOS_TEST_DATABASE_URL`; each test makes its own schema. Without it
they are skipped locally, and they fail in CI.
