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
2. Loads the bots, their roles and skills, `tools.yaml` and `team.yaml`. A change to any of
   them needs a restart.
3. Recovers tasks the last process left `working` (see **Durability**), and runs once any
   routine that came due while it was down.

## Configuration
| Variable | Default | Notes |
| --- | --- | --- |
| `PMOS_BOTS_TOKENS` | required | `name=token,…` with names `slack`, `worker`, `console`, `evals`. Each token is at least 32 characters, and all are distinct. |
| `PMOS_BOTS_DATABASE_URL` | required | Postgres. Not needed with `PMOS_DEV=1`, which keeps everything in memory. |
| `PMOS_DEV` | off | `1`: the memory store |
| `PMOS_BOTS_BIND` | `127.0.0.1:8750` | |
| `PMOS_CONTENT_ROOT` | `content` | Bots, roles, skills, `tools.yaml`, `team.yaml` |
| `PMOS_TOOLSERVER_URL` | required | The tool server's origin, such as `http://127.0.0.1:8710` |
| `PMOS_SERVICE_TOKEN` | required | The tool server's orchestrator token: it creates bot sessions and reads approvals |
| `PMOS_GATEWAY_URL` | required | The gateway's OpenAI base, ending in `/v1` (as the harness uses it) |
| `PMOS_GATEWAY_TOKEN` | required | This service's client token in the gateway's `PMOS_GATEWAY_TOKENS` |

Tokens and the database URL never appear in `Debug` output or the log.

## Who may call what
Every `/v1` call sends `Authorization: Bearer <token>`; the token's client name decides what
it may do. Anything else is `403`.

| Client | May |
| --- | --- |
| `slack` | create tasks (source `slack` or `cli`), read its PM's own tasks and their events, the notification feed, and its PM's routines (parse, create, list, change, delete) |
| `worker` | claim, heartbeat, events, trace, complete, fail |
| `console` | read everything, including traces; pause, resume and delete routines; cancel tasks |
| `evals` | create tasks with `source: "eval"`, and read eval tasks and their events |

- **The Slack client acts for one PM per call**, named in `X-Pmos-User` (a PM OS handle). The
  call is `400` without it. Another PM's task or routine answers `404`, the same as one that
  doesn't exist.
- **Who may use a bot:** a personal bot serves its owner only; a team bot serves the members
  of `team.yaml`. A task or routine for anyone else is `403`, with the reason.
- **The console** may only switch `enabled` on a routine. Its schedule and prompt are the
  owner's to change, in Slack.

## Tasks
```
queued ──claim──► working ──complete──► done
   ▲                 │  └──complete with approvals──► needs_approval ──all decided──► done
   └──lease lapsed───┤
     (first time)    ├──fail, or a second lapse──► failed
                     └──cancel (also from queued)──► cancelled
```
- **Priority** comes from the source: `slack` and `cli` are `interactive`, routines are
  `scheduled`, evals are `personal`. Claims take interactive work first, then the oldest.
- **Claim** (`POST /v1/worker/claim {worker, bots?, wait_ms?}`): long-polls up to `wait_ms`
  (at most 25 seconds; none if absent). It answers `204` if nothing came in time. The
  runtime then creates the task's tool-server session:
  `POST /internal/sessions {user, bot, run_id: <task id>}`.
  - **Answer:** `{task, bot, role, tools, session_token, lease_until, gateway}`. `gateway` is
    `{model: "pmos/<role model>", headers: {X-Pmos-User, X-Pmos-Run, X-Pmos-Priority}}`,
    ready for the harness's gateway client.
  - **Tool server refuses the session** (for example, the PM may not use the bot): the
    task fails with that reason, and the next task is tried.
  - **Tool server unavailable:** the task goes back to the queue without using up an
    attempt, and the claim is `503`.
  - **Bot gone after a restart:** the task fails with "the bot `x` no longer exists".
- **Worker calls** (heartbeat, events, trace, complete, fail):
  - Each may carry `"worker": "<name>"`; if it does, that worker must hold the claim.
  - A task that is no longer `working` (cancelled, recovered, finished) answers `409` with
    `{"status": …}`. That is the worker's signal to stop.
  - Trace entries are accepted for any claimed task, even after it finished.
- **Events** (`status`, `checklist`, `milestone`): all are kept, and each milestone is also a
  notification.

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
- **Lost items:** an id the tool server no longer knows counts as closed ("no longer in the
  approval queue"), so a task can't wait forever.
- **Outages:** if the tool server can't answer, the check is simply tried again on the next
  tick.

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
- **`GET /readyz`:** `200` when all three checks pass, `503` otherwise, with
  `{"ready", "checks": {storage, toolserver, gateway}}`:
  - **storage:** the store answers within 2 seconds;
  - **toolserver:** the tool server's `/readyz` is `200`;
  - **gateway:** `GET {PMOS_GATEWAY_URL}/models` is `200` with this service's token.
- **SIGTERM** (or Ctrl-C): long-polls return at once (`204`, or an empty page), the
  background loops stop, and in-flight requests finish before the process exits. A task a
  worker was running keeps its lease and is recovered by the next process if the worker
  doesn't finish it.

## Storage
| Table | Holds |
| --- | --- |
| `bots_tasks` | Tasks, with their lease (`worker`, `lease_until`) |
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

Postgres cases need `PMOS_TEST_DATABASE_URL`; each test makes its own schema. Without it
they are skipped locally, and they fail in CI.
