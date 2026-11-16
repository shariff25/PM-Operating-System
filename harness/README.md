# PM OS harness

The agent side of PM OS, on LangChain Deep Agents (TypeScript):
- `createPmosAgent` (`src/agent.ts`) builds an agent with the gateway model, the content
  repo's skills and master context mounted read-only, and the tool server's tools.
- The **worker** (`src/worker.ts`) is the long-running process in each bot's sandbox that
  runs that bot's tasks (docs/bot-runtime-spec.md §4).

```sh
pnpm -C harness typecheck
pnpm -C harness test
pnpm -C harness build      # tsc to harness/dist
pnpm -C harness worker     # node dist/main.js, configured from the environment
```

## The worker

One worker per bot sandbox, serving one bot. It loops:
1. **Claim:** `POST {PMOS_BOTS_URL}/v1/worker/claim {worker, bots: [PMOS_WORKER_BOT], wait_ms}`
   with `Authorization: Bearer PMOS_WORKER_TOKEN`. A `204` means nothing is queued, so it
   claims again; a `200` carries `{task, bot, role, tools, session_token, lease_until}`.
2. **Run:** it builds a fresh agent for the task:
   - **Model:** the gateway at `PMOS_GATEWAY_URL`, model `pmos/<role's model>`, with headers
     `X-Pmos-User` (the PM who asked), `X-Pmos-Run` (the task id) and `X-Pmos-Priority`.
     The client sets `maxRetries: 0`: the gateway retries `429`, `503` and connection failures
     (3 tries in a 20 s budget, `docs/gateway.md`), and a second layer would multiply them. A
     final `429`/`503` fails the task as "still busy after several tries" when the gateway's
     `X-Pmos-Upstream-Attempts` header shows it retried, and as "busy right now" otherwise.
   - **Tools:** the tool server's MCP tools for the claimed `session_token` (sent as
     `Authorization: Bearer`, never in a URL), through the official MCP TypeScript SDK client
     (`@modelcontextprotocol/sdk`), wrapped as LangChain tools.
   - **Skills:** only the role's skills are mounted under `/content/skills/`.
   - **System context:** the role's instructions, then the bot's job.
   - **Master context:** the team `AGENTS.md`; a personal bot also gets its owner's own.
   - **Writes:** no harness pause. Writes reach the tool server, which answers
     `{"status": "pending_approval", "approval_id": …}` and waits for the PM's Slack card.
     The worker collects those ids.
   - **Step limit:** at most `PMOS_WORKER_MAX_STEPS` graph steps.
3. **Progress (D6):** sent to `/v1/worker/tasks/{id}/events` as the run goes:
   - a `status` event when it starts;
   - the milestone "Working on it", then one milestone per distinct tool family the first
     time it is used, each sent once:

     | Tools | Milestone |
     | --- | --- |
     | `docs_*` | Searching the support docs |
     | `jira_*` | Reading Jira (a write: Drafting in Jira) |
     | `confluence_*` | Reading Confluence (a write: Drafting in Confluence) |
     | `slack_*` | Reading Slack (a write: Drafting a Slack message) |
     | `context_*` | Checking team context |
     | `memory_*` | Checking its notes (a write: Updating its notes) |

   - a `checklist` whenever it changes: the agent's own todo list once it writes one
     (`write_todos`), else the milestones so far, all marked done at the end.
4. **Trace:** every prompt (once per distinct system prompt), model reply, tool call and
   tool result goes to `/v1/worker/tasks/{id}/trace`, in order. A refusal from the tool
   server is also traced as a `policy_denial`.
5. **Heartbeat:** `/v1/worker/tasks/{id}/heartbeat` every 30 seconds while the task runs. A
   `404`, `409` or `410` means the task was cancelled or its lease went elsewhere: the worker
   stops the run and reports nothing more.
6. **Finish:** `complete {result, approvals}` with the final answer as markdown, or
   `fail {reason}` with a reason a PM can read (no stack traces). If the call fails, it is
   retried a few times; after that the lease expires and the runtime retries or fails the
   task (D2). One task's error never stops the loop.

**Stopping:** on SIGTERM or SIGINT the worker stops claiming. The current task has
`PMOS_WORKER_STOP_GRACE_MS` to finish. If it doesn't, the worker reports nothing and exits:
the task's lease runs out and the runtime retries it once, or fails it with a stated reason
if it was already retried (D2).

### Configuration

| Variable | Default | Notes |
| --- | --- | --- |
| `PMOS_BOTS_URL` | required | The bot runtime, e.g. `http://127.0.0.1:8750` |
| `PMOS_WORKER_TOKEN` | required | This bot's own runtime token, `worker-<bot>` in `PMOS_BOTS_TOKENS`: it can claim and report only this bot's tasks |
| `PMOS_WORKER_BOT` | required | The one bot this worker serves, e.g. `docs` |
| `PMOS_GATEWAY_URL` | required | The gateway, e.g. `http://127.0.0.1:8740/v1`; `/v1` is added if missing |
| `PMOS_GATEWAY_TOKEN` | placeholder | The gateway client token. In a sandbox, OpenShell may inject it at the network boundary instead. |
| `PMOS_TOOLSERVER_MCP_URL` | required | The tool server's MCP endpoint, e.g. `http://127.0.0.1:8710/mcp` |
| `PMOS_CONTENT_ROOT` | `content` next to `harness/` | The content repo checkout: skills, master context, `tools.yaml` |
| `PMOS_WORKER_ID` | `<hostname>-<pid>` | Sent as `worker` on claim |
| `PMOS_WORKER_WAIT_MS` | `25000` | Claim long-poll; the runtime caps it at 25,000 |
| `PMOS_WORKER_STOP_GRACE_MS` | `10000` | How long a running task may finish after SIGTERM |
| `PMOS_WORKER_MAX_STEPS` | `200` | Graph steps per task before it fails with "too many steps" |

Logs are JSON lines on stderr. They never include tokens or task text.

### Tests

`test/worker.test.ts` runs the worker against in-process fakes (`test/fakes.ts`): the bot
runtime (node `http`), a real MCP SDK server with a read tool, a write tool that answers
`pending_approval` and a refused tool, and an OpenAI-compatible gateway. The model is the
`ScriptedModel`, except in the gateway test, which checks the headers the real
`gatewayModel` sends.
