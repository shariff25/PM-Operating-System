# Bot runtime: interface spec (0e, 0f, 0g)

This is the contract between the pieces built in parallel. Each piece builds and tests against
this spec, with the others mocked. Where this spec and the code disagree, fix the code, or
raise the gap; don't silently diverge. Decisions are in `docs/bot-decisions.md` (D1–D12).

```
Slack (pmos-slack) ──┐                        ┌──► pmos-gateway (models)       [built]
Console (pmos-console)├─► pmos-bots (runtime) ─┤
Evals (pmos-evals) ──┘   tasks, routines,      └──► pmos-toolserver (sessions, approvals) [built; bot sessions added]
                         traces, notifications
                                ▲ claim / events / complete
                         harness worker (TS, one per bot sandbox) ──► gateway + tool server MCP
```

**Ports (local):**
- tool server 8710
- docs 8730
- gateway 8740
- **bots runtime 8750**
- **console 8760**

Every service reads its tokens from env and compares them with `pmos_runtime::token_eq`. It shuts down on `pmos_runtime::shutdown_signal()`.

**As built (2026-11-14), settled while merging the parallel work:**
- **Claim answer:** it also carries `gateway: {model, headers}`, the model role and the gateway headers for the task.
- **Running tasks:** carry `worker` and `lease_until`. A worker call on a task that is no longer running answers `409 {status}`, which tells the worker to stop.
- **Notifications:** each carries `at`; `approval_closed` items carry `outcomes`.
- **No PM needed:** the Slack client sends no `X-Pmos-User` for the shared notification feed or the bot list. Every per-PM call (tasks, routines) still needs it, and is refused without it.
- **Skill or bot:** tool-server sessions, approval items and audit events carry either `skill` or `bot`, never both. For bot sessions `skill` is `null`.
- **One worker token per bot:** `worker-<bot>` in `PMOS_BOTS_TOKENS`. That worker can claim, and report on, only its bot's tasks. Another bot's task answers `404`, and claiming for another bot is `403`. This way a compromised sandbox can't take another bot's tasks or their tool-server sessions.
- **Worker shutdown:** a worker that is told to stop leaves its unfinished task alone. The lease runs out and the runtime retries the task once (D2).

## 1. Bots and roles (built: `pmos_contract::bots`)

- **Bots:** `content/bots/<name>/BOT.md`, linted by `pmos lint`.
- **Roles:** `content/roles/<role>/ROLE.md`.
- **Loading:** call `pmos_contract::lint(root, &LintOptions::new(today))`, refuse to start on errors, and read `report.bots`, `report.roles` and `report.skills`.
- **Tools:** a bot's tools are `bots::role_tools(role, skills)`.
- **Write settings:** each write tool's setting is `bot.action_setting(role, tool)`, one of `approve` or `never` (`auto` is refused by the lint in v1).
- **Seeded:**
  - roles `docs-helper` (skill `docs`, public) and `pm-assistant` (`ask meeting-notes prd-draft`, restricted);
  - the team bot `docs`.
- **Personal bots:** named `pm-<handle>`, role `pm-assistant`, scope `personal`, owner `<handle>`. They are created by pull request for now.

## 2. Tool server: bot sessions (added to `crates/toolserver`)

**New session form:**
```
POST /internal/sessions {"user": "<handle>", "bot": "<bot name>", "run_id": "<task id>", "ttl_minutes"?: n}
```
It is an alternative to `skill`; send one of the two. It needs the service token.

| Session property | Rule |
| --- | --- |
| Allowlist | `role_tools(role, skills)` |
| Data-class ceiling | the role's `data_class` |
| Write with setting `never` | refused, `403`, audited as `denied` |
| Write with setting `approve` | today's grant-or-inbox path |
| `user` | the PM the task is for. For a personal bot it must be the bot's owner (`403` otherwise). For a team bot, any team member. |

**Read identity:**
- **Personal bots** read as `user`, with that PM's linked accounts (unchanged).
- **Team bots** read as the bot's service identity `bots.<name>`: a vault credential linked by a platform owner with `GET /internal/oauth/{provider}/start?user=bots.<name>`.
  - The identity check against `team.yaml` is skipped for `bots.*` users, which aren't PMs.
  - Writes from a team-bot session still run as `user` (the approving PM), never as the bot.

**Read scope for team bots** (decision 26), enforced in the connectors:

| Tool | How the scope is applied |
| --- | --- |
| `jira.search` | JQL becomes `project in (…) AND (<jql>)` |
| `jira.read` | the issue key's project must be listed |
| `confluence.search` | CQL becomes `space in (…) AND (<cql>)` |
| `confluence.read` | the page's space must be listed, checked after the fetch and before anything is returned |
| `slack.search` | results outside the listed channels are dropped |
| `slack.read_thread` | the channel must be listed |

- An empty list means no access to that system.
- An out-of-scope read is `403`, audited as `denied`.

**Bot token decides approvals:** `PMOS_BOT_TOKEN` may now also call:
- `GET /internal/approvals/{id}`;
- `POST /internal/approvals/{id}/decision`, used by Slack approval cards.

`decided_by` must be a `team.yaml` member. The Slack bot maps the clicking Slack user to their handle; the tool server checks membership and audits `client=bot`.

## 3. Runtime: `pmos-bots` (`crates/bots`)

Postgres, with its own migrations table `_pmos_bots_migrations`. `PMOS_DEV=1` keeps it in memory, and the same contract tests run against both stores.

### Auth
`PMOS_BOTS_TOKENS=name=token,…`, each token at least 32 characters. The client name sets what it may do:

| Client | May |
| --- | --- |
| `slack` | create tasks (as `X-Pmos-User`), read their own tasks, notifications, and routines for `X-Pmos-User` |
| `worker` | claim, heartbeat, events, trace, complete, fail |
| `console` | read everything; pause, resume and delete routines; cancel tasks |
| `evals` | create tasks with `source: "eval"`, and read them |

### Task
```json
{"id": "tsk_<32 hex>", "bot": "docs", "requested_by": "ana", "source": "slack|routine|cli|eval",
 "priority": "interactive|scheduled|personal", "text": "What ports does DSM need?",
 "status": "queued|working|needs_approval|done|failed|cancelled",
 "checklist": [{"item": "Search the docs", "done": true}], "result": "markdown or null",
 "error": "stated reason or null", "approvals": ["apr_…"],
 "slack": {"channel": "D0…", "thread_ts": "1731…"} , "routine_id": "rtn_… or null",
 "attempts": 1, "created_at": "…", "updated_at": "…"}
```
- **Priority:** `interactive` for Slack and the CLI, `scheduled` for routines, `personal` for evals and background work. It is passed to the gateway as `X-Pmos-Priority`.
- **Status flow:** `queued`, then `working`, then `done` or `failed`, or `needs_approval` when the result has pending approvals. A queued or working task can become `cancelled`.
- **Leaving `needs_approval`:** the runtime polls `GET /internal/approvals/{id}` every 30 seconds with the service token. When none is still `pending` or `executing`, the task becomes `done`, and the notification says what was approved or rejected.
- **Durability (D2):**
  - A claim takes a lease (`lease_until`, 120 seconds), extended by heartbeat.
  - On start-up and every 30 seconds, a `working` task with an expired lease goes back to `queued` if `attempts < 2`, with the event "interrupted by a restart; retrying".
  - Otherwise it becomes `failed`, with the reason "interrupted twice by restarts; ask again".
  - Nothing is lost silently.

### API
| Method and path | Client | Notes |
| --- | --- | --- |
| `GET /v1/bots` | all | `[{name, role, scope, owner, description}]` |
| `GET /v1/bots/{name}` | all | the bot, its role, its tools, and each write tool's setting |
| `POST /v1/tasks` `{bot, requested_by, text, source, slack?}` | slack, evals | `201` with the Task. `403` if `requested_by` may not use the bot: personal bots serve their owner only, and team bots serve team members. |
| `GET /v1/tasks/{id}`, `GET /v1/tasks?bot=&requested_by=&status=&limit=` | slack (own), console, evals | |
| `POST /v1/tasks/{id}/cancel` | slack (own), console | |
| `GET /v1/tasks/{id}/events` | slack (own), console, evals | `[{at, kind, text}]` |
| `GET /v1/tasks/{id}/trace` | console | `[{at, kind, body}]` |
| `POST /v1/worker/claim` `{worker, bots?: [..], wait_ms?}` | worker | Long-polls up to `wait_ms` (at most 25,000). Returns `200` with `{task, bot, role, tools, session_token, lease_until}`, or `204` if nothing is queued. The runtime creates the tool-server session (§2) for the task. |
| `POST /v1/worker/tasks/{id}/heartbeat` | worker | extends the lease |
| `POST /v1/worker/tasks/{id}/events` `{kind: "status"\|"checklist"\|"milestone", text?, checklist?}` | worker | Milestones are what Slack shows. The runtime keeps them all and notifies each one. |
| `POST /v1/worker/tasks/{id}/trace` `{entries: [{kind: "prompt"\|"model_reply"\|"tool_call"\|"tool_result"\|"policy_denial", body}]}` | worker | to the trace store |
| `POST /v1/worker/tasks/{id}/complete` `{result, approvals: [..]}` | worker | `needs_approval` if `approvals` is non-empty, else `done` |
| `POST /v1/worker/tasks/{id}/fail` `{reason}` | worker | |
| `GET /v1/notifications?after=<cursor>&wait_ms=` | slack | Long-polls. Returns `{cursor, items: [{id, task_id, bot, requested_by, slack, kind: "milestone"\|"done"\|"failed"\|"needs_approval"\|"approval_closed", text, approvals?}]}` in order. |
| `POST /v1/routines/parse` `{bot, owner, text, tz}` | slack | Returns `{draft, summary}`, or `422` with `{question}` when the request is unclear. |
| `POST /v1/routines` `{draft}` | slack | `201` with the Routine |
| `GET /v1/routines?owner=` | slack (own), console | |
| `PATCH /v1/routines/{id}` `{enabled?, schedule?, prompt?}` | slack (own), console | |
| `DELETE /v1/routines/{id}` | slack (own), console | |
| `GET /healthz`, `GET /readyz` | none | readiness covers storage plus the tool server and the gateway |

### Routines (D8)
```json
{"id": "rtn_…", "bot": "pm-ana", "owner": "ana", "enabled": true,
 "schedule": {"cron": "0 8 * * 1-5", "tz": "Europe/London"},
 "prompt": "Summarize overnight #launch and draft replies to anything addressed to me",
 "output": {"kind": "dm"}, "approval": "approve", "next_run_at": "…", "last_run_at": null}
```
- **Parsing:** `/v1/routines/parse` asks the gateway (`pmos/extraction`) for JSON in this shape, then validates it in code:
  - the cron has 5 fields;
  - the time zone is an IANA name;
  - the bot exists and the owner may use it;
  - `approval` is always `approve` in v1.

  The `summary` is a plain sentence ("Every weekday at 8:00 London time, Docs bot will …") that Slack reads back for a one-tap confirm.
- **Running:** a scheduler wakes every 30 seconds and enqueues due routines as tasks (`source: "routine"`, priority `scheduled`). A run missed during downtime runs once on start, not once per missed slot.

### Traces (D6, decision 22)
- **What's stored:** prompts, model replies, tool calls and results, and policy denials, per task.
- **Kept:** 30 days.
- **Slack text:** removed after 24 hours by an hourly job. Any entry whose body contains Slack message text (`tool_result` of `slack.*`) has its body replaced with `{"redacted": "slack content removed after 24 h"}`.
- **Separate from the audit log:** the tool server's audit log stays metadata-only and append-only.

## 4. Harness worker (`harness/src/worker.ts`)
- **Claim:** one long-running process per bot sandbox. It calls `POST /v1/worker/claim {worker, bots: [<its bot>]}`, with `PMOS_BOTS_URL`, `PMOS_WORKER_TOKEN` and `PMOS_WORKER_BOT`.
- **Agent:** builds `createPmosAgent` with:
  - `gatewayModel` at `PMOS_GATEWAY_URL` / `PMOS_GATEWAY_TOKEN`, with the model role from the bot's role and headers `X-Pmos-User`, `X-Pmos-Run` = task id and `X-Pmos-Priority`;
  - the tool server's MCP tools for the claimed `session_token`;
  - only the role's skills;
  - the role's instructions plus the bot's job as system context.
- **Writes:** no harness interrupt. Writes go to the tool server, which returns `pending_approval` items; the worker collects their `approval_id`s.
- **Progress:** checklist and milestones as the run goes. At least "Working on it" and one milestone per distinct tool family used ("Searching the support docs", "Reading Jira", "Drafting in Confluence"), each sent once. Every model turn and tool call goes to the trace endpoint.
- **Heartbeat:** every 30 seconds while running.
- **Finish:** `complete {result, approvals}`, or `fail {reason}` on an error, with a reason a PM can read.

## 5. Slack (`crates/slack`)
- **DM routing:** one Slack app, `PM OS`, with a DM per PM. A message's bot is chosen this way:
  1. a `<bot>:` prefix (`docs: what ports does DSM need?`);
  2. else the bot already working in that thread;
  3. else the PM's personal bot `pm-<handle>` if it exists;
  4. else `docs`.

  The bot posts its replies under its own name (`chat:write.customize`), so each bot reads as its own teammate in the thread. *Deviation to confirm:* D1 says a DM per bot. A Slack app per bot would meet that literally, at the cost of a Slack app per bot.
- **Tasks:** a message becomes `POST /v1/tasks` (as the PM, mapped through `team.yaml`) with `slack: {channel, thread_ts}`. Replies go in that thread.
  - **Notifications:** a single long-poll loop on `/v1/notifications`. Each milestone is one message, edited in place as the run goes, so a thread gets at most one message per milestone (D6).
  - **Result:** the final answer is a new message.
- **Approval cards (D7):** for each `needs_approval` item, a card with the tool, a summary of the arguments, and buttons **Approve**, **Edit** and **Reject**.
  - **Edit** opens a modal with the editable fields (summary, body, title, text).
  - A decision calls the tool server's decision endpoint with the bot token; `decided_by` is the clicker's handle.
  - The card updates to "Approved by Ana", with the link that was written.
  - Only the item's own PM can decide; anyone else gets a private note.
- **Routines in Slack (D8):**
  - **Create:** a DM such as "every weekday at 8am …" (or `routine: …`) is parsed, then read back with **Create** and **Cancel** buttons.
  - **List:** "my routines" lists them, with **Pause**, **Resume** and **Delete** buttons.
- **`/docs`** stays as it is: a fast search shortcut that asks the Docs bot's search skill.
- **Manifest:** add `message.im` events, interactivity, and the bot scopes `chat:write`, `chat:write.customize` and `im:history`.

## 6. Console (`crates/console`, `pmos-console`)
**Four screens, server-rendered (no frontend build), read-mostly:**
1. **Bots:** the roster, each bot's role, scope, owner, read scope and action settings.
2. **Routines:** each with its next run; **Pause**, **Resume** and **Delete**.
3. **Approvals:** the queue from the tool server, read-only, with a link to Slack to decide.
4. **Activity:** recent tasks; a task opens its events, its full trace (D6) and its audit events.

**Auth:**
- behind the Spark's SSO proxy: `X-Forwarded-User` is trusted only with `PMOS_CONSOLE_TRUST_PROXY=1`;
- else `PMOS_CONSOLE_DEV_USER` for development.

Admins (role `admin` in `team.yaml`) see everything; a PM sees their own bots, routines and tasks plus the team bots.

## 7. Sandbox policies (0f): `crates/policy` and `pmos policy`
- **Per bot:** `policies/bots/<bot>.yaml` is generated from the bot's role (the D3 role template) by `pmos policy generate`.
- **Checked in CI:** `pmos policy check` verifies the files are current and every rule holds:
  - **Network:** egress denied by default, allowing only:
    - `toolserver.pmos.internal:8710` (MCP on `/mcp`, the role's tools only);
    - `gateway.pmos.internal:8740` (`POST /v1/chat/completions`);
    - `bots.pmos.internal:8750`, the worker routes only: `POST /v1/worker/claim` and `POST /v1/worker/tasks/*/{heartbeat,events,trace,complete,fail}`;
  - **Filesystem:** the bot's own workspace is writable; `skills/` and its master context are read-only; nothing else is mounted;
  - **Credentials:** only the bot's own, per sandbox:
    - its gateway client token (provider `pmos-<bot>-gateway`; one `bot-<bot>` entry per bot in `PMOS_GATEWAY_TOKENS`);
    - its runtime worker token (provider `pmos-<bot>-worker`; one `worker-<bot>` entry per bot in `PMOS_BOTS_TOKENS`, which claims only that bot's tasks);
    - the tool-server session token, which comes in each claim response (v1 decision, 2026-11-14). It lives for one task, is scoped to the bot, and the tool server refuses anything outside the bot's role. The tool-server endpoint binds no provider. Rotating a per-bot session provider instead is a recorded future option (`docs/sandbox-policies.md`);
  - **Processes:** no privilege escalation.
- **Changes:** a network-policy change is a reviewed pull request by a second platform-owner PM (POL-2).
- **Format:** NVIDIA OpenShell's policy format, verified against its docs where reachable, else marked "verify on the Spark".
- **Isolation tests:**
  - bot A can't read bot B's files or tokens;
  - a blocked call is logged with its destination and the policy that blocked it.

  They are written as a script for the Spark, plus generator unit tests here.

## 8. Benchmarks (0g): `evals/` and `pmos-evals`
- **Categories:** `evals/categories/<name>/category.yaml` holds `{name, bot, pass_bar}`.
  - Tasks are `tasks/*.yaml` with `{id, ask, expect: {must_cite?: [url], must_include?: [text], must_not_include?: [text], approvals?: n}}`.
  - The first category is `docs-qa`: 20 real questions answered by support.fortanix.com pages.
- **Runner:** `pmos-evals run --category docs-qa` creates tasks through the runtime API (client `evals`), waits for them, and grades each one.
- **Failure tags:** every failure is tagged `model`, `tool`, `policy`, `memory` or `harness`, by rules over the task's events and trace (D10).
- **Report:** written as markdown under `evals/reports/<date>-<category>.md`. A category passes when it meets its `pass_bar` two weeks in a row.
- **Load test:** `pmos-evals load --tasks 5` sends N concurrent tasks and reports p50 and p90 queue and total time against the PRD latency targets.
- **CI:** runs the runner against the stub gateway and a mocked runtime, to check the grading itself.
