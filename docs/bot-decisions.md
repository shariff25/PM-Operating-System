# PM OS bots: key decisions

PM OS v1 is a small team of **named AI bots** for the product team. They run always-on on
the DGX Spark, read our tools freely, and draft anything outbound for a PM to approve. These
decisions adapt how SpaceXAI built Grok Bot (about seven weeks) to our constraints. Source:
the product lessons doc shared on 2026-11-14.

**How to use this doc:**
- Every decision is a requirement, and its **Done when** is the definition of done.
- If a decision blocks the build or conflicts with another, stop and raise it; don't work around it.
- The PRD says *what* to build; this doc says *how to decide*. Where they conflict, flag it rather than pick one.

**Status:**
- **Missing:** the unship list. D12 arrived separately on 2026-11-14.
- **Settled:** the conflicts with earlier decisions, and the follow-up questions, were settled on 2026-11-14 (see **Resolved conflicts**).

## Where PM OS differs from Grok Bot

| Grok Bot | PM OS | Why |
| --- | --- | --- |
| Always-on cloud computers | Always-on service on the DGX Spark | On-prem, one box |
| One cloud computer per bot | One OpenShell sandbox per bot | OpenShell gives each sandbox its own policy and credentials |
| Screen-level computer use first | APIs and MCP first, browser second | PM tools have APIs; an open model on one box is less reliable at clicking pixels |
| "Set it and forget it" | Draft, then a PM approves anything outbound | It speaks for PMs in Slack |
| Frontier model | Open model, swappable | Constraint |

## The decisions, and where we stand

| # | Decision | Done when | Today |
| --- | --- | --- | --- |
| D1 | **Bots, not features.** Two surfaces: Slack (a DM per bot, approval cards) and a small web console (bot roster, routines, approval queue, audit log). Every capability is reached by asking a bot. | A PM creates a bot, gives it a job and gets work back without leaving Slack. No console screen exists for a single feature. | **Gap.** We have skills and `/docs`, but no bots yet. Slash commands stay, as shortcuts that address a bot (`/docs` asks the Docs bot). |
| D2 | **Always-on, server-side.** Bots are long-lived services on the Spark. Tasks, memory, routines and drafts live server-side. | An 8:00 routine runs with every laptop closed. After a reboot, in-flight tasks resume or fail with a stated reason; nothing is lost silently. | **Partly.** Approvals already survive restarts: interrupted writes become "outcome unknown". Still missing: a durable task queue, a scheduler, and resume on reboot. |
| D3 | **One sandbox per bot.** Its own filesystem, policy and credentials. Outbound network denied by default, policy YAML in git, network changes through D7 approval. | A test shows bot A can't read bot B's files or tokens. A blocked call is logged with its destination and the policy that blocked it. | **Change:** the plan had one sandbox per *session*; it becomes one per *bot*. Credentials follow the hybrid model (see **Resolved conflicts**). |
| D4 | **APIs and MCP first**, a headless browser as fallback, no pixel clicking in v1. Read tools and write tools kept separate. | Every integration splits read from write. No write tool runs without the D7 check. | **Met.** Every tool is `read`, `internal` or `write`; writes run only with a grant or an approval, enforced in the tool server. Each system can be its own MCP server behind the tool server, as `pmos-docs` is. |
| D5 | **Named bots with a job, memory and two scopes.** Team memory is shared; personal memory is owned by one PM and private. Memory is markdown, with a source and date on every entry. The team context library is mounted read-only. | A team bot can't read a personal bot's memory. A PM can open and edit every memory file their bot uses. | **Change:** memory moves from Postgres to markdown files in git. The context mount is filtered by the bot's data class, so the review fix that keeps restricted files out of sandboxes still holds. |
| D6 | **Show progress, hide the machinery, log everything.** Each task emits a status and a short checklist. The full trace goes to an audit store, viewable per task. | A task's Slack thread gets at most one message per milestone. Any task's full trace can be pulled from the console. | **Gap.** The audit log has metadata only. Add task status events and a trace store kept 30 days. |
| D7 | **Draft, then approve, for anything outbound.** Each action type is `auto`, `approve` or `never`; v1 ships every outbound action as `approve`. Edits are saved to the bot's memory as feedback. Enforced in code. | No outbound write happens without an approval record, even with a prompt injection planted in a Slack message. Every approve, edit and reject is logged. | **Mostly met.** Writes are already enforced in code and fail closed if the audit log is down. Still missing: per-bot action settings, saving edits as feedback, and a prompt-injection test. |
| D8 | **Routines in plain language.** "Every weekday at 8am, summarize #launch…" becomes a structured routine: schedule, scope, output and approval setting. It is read back for a one-tap confirm. | A PM creates, changes and stops a routine entirely in Slack. | **Gap** (was Phase 2 "scheduled jobs"; moves to Phase 1). |
| D9 | **The launch-post test.** A feature ships only if "PM OS can now ___" is something a PM cares about. Finished features that fail it are cut. | Every shipped capability has a "can now" line in the changelog. | **Adopt now:** PR template and `CHANGELOG.md`. |
| D10 | **Finishing the job beats 90%.** A task category launches only when bots finish it end to end. Real tasks are rerun weekly and on every model or harness change, and each failure is tagged to its bottleneck. | Each launched category meets its pass bar two weeks in a row. | **Planned** as 0g evals; reshaped into per-category benchmarks with failure tags. |
| D11 | **Every workaround has an expiry date.** Scaffolding for model weakness sits behind a flag with a "delete when" test. The model sits behind one OpenAI-compatible endpoint. | Switching models is a config change plus one benchmark. | **Partly:** the harness already calls model roles (`pmos/default`, `pmos/extraction`) through the gateway. Add a workaround registry when the gateway lands (0d). |
| D12 | **One role per bot; a Chief of Staff only when it emerges.** v1 ships single-role bots, with no coordinator bot that delegates to others. We build one once several PMs (proposed: 3) set it up by hand. A bot may read other bots' outputs in the same scope, nothing more. Count how often PMs chain bots by hand. | Role templates exist. *(The line may have been cut off; confirm whether it continues.)* | **Gap.** Role templates are new. They also feed D3: each bot's sandbox policy is generated from its role template. **Not a conflict:** `/review`'s parallel reviewer sub-agents stay inside one bot, so they aren't a coordinator. Skill chaining (SKL-6) was already Phase 3. |

## Resolved conflicts (decided 2026-11-14)

1. **Whose credentials bots use (D3 versus "each PM's own token"): hybrid.**
   - **Team bots** read with their own read-only service accounts, scoped to team Jira projects, Confluence spaces and Slack channels.
   - **Personal bots** read with their owner's linked accounts, so they see exactly what that PM sees.
   - **Anything outbound** goes out with the token of the PM who approved it, marked "Drafted with PM OS" (SLK-7). A bot never posts as itself.
   - **The audience check (SLK-6) still applies.** A team bot's read scope must not exceed what every member of the team can see, so it can't become a way around permissions.
2. **Traces (D6 versus metadata-only audit and SLK-8): a separate trace store kept 30 days.**
   - It holds prompts, tool calls and policy denials per task.
   - Slack message text is removed after 24 hours (SLK-8).
   - The audit log stays append-only and metadata-only.
3. **Memory (D5): markdown files in git.**
   - **Team memory** is in git, and PMs edit it by pull request or commit.
   - **Personal memory** is in a separate private store, readable only by its owner and their bots.
   - Every entry carries its source and date.
4. **Console (D1): a small server-rendered console** with four screens only: bot roster, routines, approval queue and audit or trace. It replaces the planned platform-owner-only admin pages. Slack stays the main surface, and approvals still happen in Slack cards.

5. **Team bots' read scope: the PM chooses (decided 2026-11-14).**
   - A team bot's owner picks the exact Jira projects, Confluence spaces and Slack channels it may read. The list is part of the bot's definition, shown in the console and changed by asking the bot.
   - The bot's read-only service accounts are limited to that list, and the tool server refuses reads outside it, whatever the account itself could see.
   - Widening the list goes through D7 approval, like any policy change.
6. **Team memory repo: `pm-os-memory` by default, configurable (decided 2026-11-14).**
   - Team memory lives in a git repo, by default `pm-os-memory`, set in config so a platform owner can point PM OS at another repo.
   - The tool server commits each bot write there, with the bot as author and the source in the message.
   - PMs edit by pull request or direct commit; git history is the review trail.
   - The content repo keeps its review-by-pull-request rule.
7. **Slash commands stay as shortcuts (decided 2026-11-14).** `/docs` asks the Docs bot; a command is never a feature of its own (D1).

## Open questions

- **The unship list** is still to come.
- **D12's "Done when"** may be cut off after "role templates exist". Is it complete?
- **What counts as "chaining by hand"** (D12's metric). The proposal: a PM passes one bot's output to another bot within an hour, either by forwarding the message or by citing the task. It is counted in the trace store and shown in the console.
- **Personal memory store.** The proposal: a private git repository per PM on the Spark, never pushed to the shared remote, and backed up with the rest of the Spark.
