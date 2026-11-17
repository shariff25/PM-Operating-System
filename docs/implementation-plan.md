# PM OS — implementation plan and phases

## Context
The PM OS PRD defines an internal agent workspace for the product team, running on one DGX Spark: 8 launch skills, a 7-persona reviewer panel, a team
context library, a Slack reply assistant with human approval, every run in an NVIDIA OpenShell sandbox, pilot by
week 7 and full team by week 12. This repo (`shariff25/pm-operating-system`) is empty apart from a README. It becomes
both the team's content repo (the PRD's "files are the source of truth") and the service code.

Decisions already made in this conversation:
- **Backend in Rust; frontend in TypeScript.** The agent harness is also TypeScript, because Deep Agents has no Rust port.
- **Open-weight models only, no frontier API.** Qwen3.6-35B-A3B is the default model and Nemotron 3 Nano 30B-A3B handles extraction and triage, both on vLLM. The bake-off adds Qwen 3.5 122B and gpt-oss-120b as challengers.
- **Harness: LangChain Deep Agents (`deepagents` 1.16.5, MIT, TypeScript) for the team service; Hermes on NemoClaw for personal agents.**
- **Context layer:** markdown in git (`content/`), indexed into Postgres (pgvector + full-text search) on merge, served only through the Rust tool server's MCP tools. Memory also lives in Postgres and is exposed to the harness as `/memories/*.md`. There is no Letta, Mem0, Graphiti or Cognee in v1.
- **Named bots, not features** (decided 2026-11-14, from the Grok Bot lessons; see [`bot-decisions.md`](bot-decisions.md)). PM OS is a small team of named bots, each with a job, memory, its own OpenShell sandbox and a policy. They run always-on on the Spark, read freely, and draft anything outbound for a PM to approve. That doc's decisions D1–D11 are requirements, and they supersede the older bullets below where they differ:
  - one sandbox per bot (not per session);
  - memory as markdown in git (not Postgres);
  - a small PM-facing console (not admin pages only);
  - routines in Phase 1.
- **The Slack bot is the interface** (decided 2026-11-14; reopened 2026-11-16). PMs tag it in any thread, chat with it in Slack's assistant panel, run skills with slash commands, and approve in Slack. The 2026-11-14 line "There is no web workspace" is superseded: on 2026-11-16 the product owner decided to build a full web client (a TypeScript single-page app in the pnpm workspace). See [`web-client.md`](web-client.md) for the decisions, roles, phases (Web 1 to Web 4) and open questions. Platform-owner PMs get a few small admin pages (audit search, sandbox policies, usage) and the `pmos` CLI. See **Interface: the PM OS Slack bot** below.

## Architecture
```
Slack (DM per bot, approval cards) ─┐
Console (roster, routines,          ├─► bot runtime (Rust: durable task queue, scheduler, routines,
  approvals, audit/trace)           │      status events, sandbox launcher)
CLI pmos (Rust) ────────────────────┘         └─► one long-lived OpenShell sandbox per bot
                                                    └─ harness (TS, deepagents): job, skills, memory files
                                                       ├─ MCP ─► toolserver (Rust): connectors, context, approvals, audit ─► Postgres
                                                       └─ HTTP ─► gateway (Rust): one OpenAI-compatible endpoint ─► vLLM
```
(Before 2026-11-14 this was one sandbox per session behind an orchestrator; see `bot-decisions.md`.)
The sandbox network policy allows only the tool server and the gateway. All writes outside PM OS become approval items.

## Repo layout
```
content/   master/AGENTS.md, master/people/<handle>/AGENTS.md, context/<domain>/*.md,
           skills/<name>/SKILL.md, reviewers/*.md, templates/*.md, tools.yaml
policies/  team-session.yaml, personal-agent.yaml, scheduled-job.yaml
crates/    contract (types + lint), cli (pmos), toolserver, gateway, orchestrator,
           slack (the bot), policy-check
harness/   TS deepagents runner
admin/     a few server-rendered admin pages, served by the orchestrator (Phase 2)
evals/     golden tasks per skill, bake-off, load test
docs/      implementation-plan.md, contract.md
```
Rust: Cargo workspace using axum, sqlx, rmcp, reqwest, clap, serde and chrono; the Slack bot speaks Socket Mode directly with tokio-tungstenite. Each crate is checked for maintenance before it is added.
TypeScript: pnpm workspace with `harness/`. No frontend app.

## Interface: the PM OS Slack bot

PMs already live in Slack, so PM OS works there like a teammate you can tag, much like
chat-native bots on other platforms. Unlike those bots, it never posts in public by itself:
an answer is first shown only to the person who asked, and it is posted, as that PM, only
when they click.

### How PMs use it
| Entry point | What happens | Slack API |
| --- | --- | --- |
| **Tag it in a thread:** `@PM OS does Acme Vault support SAML SSO?` | Reads the thread and searches the context library, Jira, Confluence and Slack as the asker. The thread gets one line only the asker sees; the cited draft arrives **in the asker's DM** on the reply card: **Post as you**, **Edit**, **Snooze**, **Reject** (decided 2026-11-15: [spec](slack-mentions-and-home-spec.md)). | `app_mention` event, `chat.postEphemeral` |
| **Assistant panel** (Slack's AI-app side panel, next to any conversation) | A private chat with PM OS: questions, skills, follow-ups. Shows "Searching Jira…" status and streams answers. Suggests skills as one-tap prompts. | `assistant_thread_started`, `assistant.threads.setStatus`, `assistant.threads.setSuggestedPrompts`, `chat.startStream` |
| **DM the bot** | Same as the panel, for clients without it. | `message.im` |
| **Slash commands:** `/prd-draft`, `/meeting-notes`, `/review`, `/ask` … | Runs the skill. Long jobs reply in the thread when done (SUR-1), with a link to the Confluence draft. | slash commands |
| **App Home tabs:** Inbox, Skills, Accounts | **Inbox:** pending approvals, approved or discarded in a batch (SLK-10). **Skills:** the catalog (SKL-3). **Accounts:** link or unlink Jira, Confluence and Slack. | `app_home_opened`, `views.publish` |
| **Reply assistant** (SLK-1 to SLK-12) | Questions aimed at the PM get a drafted reply in a DM review card: Approve, Edit, Snooze, Reject. | Events API, Block Kit |

### Rules the bot keeps
- **Nothing is posted without a click.** **Post to thread** or **Approve** is the approval (OUT-4, SLK-5).
  - The orchestrator records the decision as a tool-server grant, and the post goes out with the PM's own Slack user token, marked "Drafted with PM OS" (SLK-7).
  - There is no auto-send setting.
- **The audience check runs on Post** (SLK-6). A draft citing Restricted sources is blocked in channels where some members lack access. External (Slack Connect) channels need a second confirmation.
- **Only PM OS users can invoke it.**
  - The Slack user is mapped to a PM OS user at SSO onboarding, and reads use that PM's linked accounts.
  - Anyone else who tags it gets a private note saying who to ask. Readers (PMM, SEs, engineering leads) come in Phase 3.
- **The bot token is only for plumbing:** receiving events, private replies, review cards, modals and App Home. It never reads channels for answers and never posts visible messages.
- **Ephemeral replies don't last** (they're gone on reload), so every draft is also an Inbox item until it is posted or discarded.
- **Connection:** Socket Mode, so the Spark needs no inbound connection from Slack. The OAuth callback only needs to be reachable from PMs' browsers on the internal network.

### Where each web-workspace requirement now lives
Reopened 2026-11-16: the web workspace is being built as the web client ([`web-client.md`](web-client.md)). The rows below are the record of what the Slack-side design covers, and the web client's own phases decide how the WEB and SUR rows are built.

| PRD requirement | Now |
| --- | --- |
| WEB-1 chat home | Assistant panel and DM |
| WEB-2 approval cards inline | Ephemeral answer with buttons; DM review cards; Inbox tab |
| WEB-3 plain request suggests a skill | Suggested prompts and skill buttons on the answer |
| WEB-4 chats per user, deletable, logged | Assistant threads are per user. `/pmos forget` deletes PM OS's own copy; Slack's copy follows the workspace retention policy. Every turn is still audited (ADM-2). |
| SUR-3 web workspace | Reopened 2026-11-16 (was "replaced by the bot, App Home and the admin pages"). Being built as the web client, Web 1 to Web 4, in [`web-client.md`](web-client.md). |
| SLK-10 inbox view | App Home Inbox tab |
| SKL-3 catalog | App Home Skills tab; publishing stays by pull request (SKL-4) |
| REV-3 accept or dismiss reviewer comments | Buttons on each `/review` finding |
| MOD-2 personal agent | `/pmos agent start`, `stop` and `status` |
| ADM-2 audit search, ADM-4 usage, POL-1 to 6 policies | Admin pages (read and search), `pmos audit` and `pmos policy` CLI. Policy changes are edited in git and reviewed by a second platform-owner PM, so there is no form editor (a change from POL-2). |

## Phases

### Phase 0: Foundation (weeks 1–3) → Gate 1
No real tokens or data are used before Gate 1. Work packages are in build order; each is one PR.

1. **0a. Contract and lint.** First PR, detailed below.
2. **0b. Seed content.**
   - Team `AGENTS.md`.
   - Context folders, each with a README and a placeholder file. Owners and facts are left as TODO; no company or product facts are invented.
   - The 7 reviewer rubrics, built from the PRD appendix.
   - PRD, POC plan and release-notes templates.
   - Skills `/ask`, `/meeting-notes` and `/prd-draft`.
3. **0c. Tool server (Rust, `rmcp` + axum).**
   - Context tools: `context.search` and `context.read`, with a sensitivity check on every read.
   - Indexer that runs on merge.
   - Read connectors for Jira, the doc tool and Slack, using the user's own OAuth token (OUT-2). Built behind a trait, with fakes first.
   - Write tools that create approval items instead of writing (OUT-4).
   - Per-skill tool allowlist.
   - Untrusted-content tagging.
   - Audit log (ADM-2).
4. **0d. Gateway (Rust).**
   - OpenAI-compatible `/v1/chat/completions` and `/v1/embeddings`.
   - Model pinned per skill; local vLLM routes only (ADM-3).
   - Priority queue: interactive work, then scheduled jobs, then personal agents, with a per-user cap (MOD-4).
   - Egress log, which provides the "Restricted data sent off the Spark" metric.
5. **0e. Harness and orchestrator.**
   - The harness wires `createDeepAgent` to the gateway, tool-server MCP tools, skills and memory.
   - The orchestrator queues runs, starts one OpenShell sandbox per session, and turns approval pauses into inbox items.
   - Also covers citation checking (OUT-5) and doc-tool output (OUT-1).
6. **0f. Sandbox policies.**
   - The three `policies/*.yaml` files.
   - `policy-check` crate (POL-3): only `*.pmos.internal` hosts, no direct Slack, Jira or doc hosts, read-only content repo.
   - A red-team checklist.
7. **0g. Evals.**
   - At least 20 golden tasks per P0 skill, including retrieval checks.
   - A CI regression gate.
   - A model and harness bake-off runner.
   - A load test for 5 concurrent sessions. It reports latency; latency is not a priority and doesn't gate (decided 2026-11-14).
   - A measured time baseline for the core artifacts.

**What runs where:** steps 0a–0c and 0f–0g are built and tested here against fakes and a stub model. vLLM, the OpenShell runtime, real OAuth and the baseline need the Spark.

**Gate 1:** evals pass, every load-test task finishes (latency is reported, not gated), and the sandbox red-team passes.

### Bots: what changes in the build order (from `bot-decisions.md`)
- **0d gateway:** add the workaround registry, with a flag and a "delete when" test for each workaround (D11).
- **0e becomes the bot runtime**, replacing the per-session orchestrator:
  - bot definitions in `content/bots/<name>/BOT.md`: name, job, team or personal scope, owner, skills, data class, per-action `auto`/`approve`/`never` settings (v1: all `approve`), and, for team bots, the Jira projects, Confluence spaces and Slack channels its owner chose for it to read, enforced by the tool server; checked by `pmos lint`;
  - a durable task queue in Postgres that resumes or fails with a reason after a reboot (D2);
  - task status events and a checklist, posted as one Slack message per milestone (D6);
  - a 30-day trace store (D6);
  - one long-lived sandbox per bot (D3);
  - **role templates** in `content/roles/<role>/` (D12): each one sets a single role's job text, default skills, data class, approval settings and sandbox policy, and every bot is created from one;
  - **one role per bot, no coordinator bot in v1** (D12): a bot may read other bots' outputs in the same scope, nothing more. The runtime counts how often PMs chain bots by hand, and a Chief of Staff bot is considered once 3 PMs do it.
- **0f sandbox policies:**
  - each bot's policy is generated from its role template and kept in git (D3);
  - network is denied by default;
  - a test shows bot A can't read bot B's files or tokens;
  - blocked calls are logged with their destination and the policy that blocked them.
- **0g evals become D10 benchmarks:**
  - a fixed set of real tasks per category, rerun weekly and on every model or harness change;
  - each failure tagged to its bottleneck: model, tool, policy, memory or harness;
  - a category launches only after passing its bar two weeks in a row.
- **Memory moves to markdown files in git** (D5): team memory in a git repo set in config (default `pm-os-memory`), committed by the tool server on each bot write; personal memory in each PM's own private repo in your GitHub org (decided 2026-11-14; `PMOS_MEMORY=github`); a source and date on every entry; and the context mount filtered by data class.
- **Phase 1:**
  - The first named bots: a team **Docs** bot, which takes over `/docs`, and personal PM bots.
  - Routines in plain language (D8), moved up from Phase 2.
  - The console's four screens (D1).
  - Per-action approval settings, PM edits saved as feedback, and a prompt-injection test (D7).
- **Every feature PR opens with its "PM OS can now ___" line**, and the changelog collects them (D9).

### Phase 1: Pilot (weeks 4–7), 2–3 champion PMs → Gate 2
- SSO and the three roles: PM, skill author, admin (ADM-1). Slack users are mapped to PM OS users at onboarding.
- **The Slack bot** (`crates/slack`, Socket Mode), the pilot's interface:
  - tag-to-ask, with the draft delivered in the PM's DM on the reply card (**Post as you**). (Built.)
  - the assistant panel with streaming and status;
  - DMs and slash commands, with long jobs replying in thread (SUR-1);
  - the App Home: Inbox, Skills and Accounts. (Built.)
- Account linking from Slack: the Accounts tab and first-use prompts start the OAuth flows the tool server already has.
- The `pmos` CLI running the same skills (SUR-2).
- The remaining launch skills: `/review` (parallel sub-agents, one critique grouped by severity), `/call-synth`, `/competitive-brief`, `/release-notes` and `/status-update`. (Built.)
- The Slack reply assistant, SLK-1 to SLK-8: triage, grounded drafts, an approval card, an audience check, and a 24-hour expiry that deletes stored Slack content.
- Read connectors for call transcripts and CRM (OUT-3).

**Gate 2:** champions use it weekly, with no guardrail incident.

### Phase 2: Team launch (weeks 8–12) → Gate 3
- **The bot, the rest of the way:**
  - the App Home Skills tab and suggested-skill prompts (WEB-3, SKL-3);
  - reviewer accept and dismiss buttons (REV-3);
  - `/pmos forget`;
  - personal-agent commands (MOD-2).
- **Admin pages for platform-owner PMs**, server-rendered by the orchestrator behind SSO. There is no frontend build. They cover:
  - audit search with CSV export (ADM-2);
  - usage: weekly active PMs, runs per skill, queue time, reviewer acceptance (ADM-4);
  - sandbox policies and the running sandboxes, read-only, plus recreate and revoke actions (POL-1, POL-4 to 6).
- `pmos audit` and `pmos policy` CLI commands.
- Skill catalog and skill publishing by pull request (SKL-3/4), plus `/new-skill`.
- Memory (MEM-1/2), scheduled jobs and digests (SKL-5, SLK-9).
- Personal-agent blueprint on Hermes/NemoClaw, with contract tests (MOD-2 to 6).
- Team-launch skills: `/poc-plan`, `/partner-brief`, `/advisory-draft`, `/jira-draft` and `/launch-checklist`. (Built.)

**Gate 3:** at least 80% of PMs active weekly, and the success metrics are on track.

### Phase 3: Expand (week 13 on)
- Readers: PMM, SEs and engineering leads can tag the bot for context-library answers, read-only.
- The web client, now decided (2026-11-16) rather than waiting for the pilot. Its phases are Web 1 to Web 4 in [`web-client.md`](web-client.md), not these phases.
- More connectors.
- Context-update pull requests proposed from calls (CTX-5).
- Skill chaining (SKL-6).
- A second Spark if p90 queue time passes 2 minutes.
- Optionally Graphiti, for temporal facts.

## First PR: 0a contract and lint, plus the workspaces
Done in the first pull request. The formats are specified in `docs/contract.md`.

1. `docs/implementation-plan.md` holds this plan. `docs/contract.md` holds the file formats below.
2. **Cargo workspace.** `crates/contract` is a library; `crates/cli` builds the `pmos` binary with a `lint` subcommand.
   - `pmos lint [--root content] [--today YYYY-MM-DD] [--format text|json]` exits non-zero on errors.
3. **Contract formats.**
   - **SKILL.md** follows the Agent Skills spec: `name` (lowercase and hyphens, matching the directory) and `description` (at most 1,024 characters). Deep Agents' loader enforces the same rules (`libs/deepagents/src/skills/loader.ts` upstream).
     - The PM OS fields sit under `metadata`, all as strings: `owner`, `version`, `model`, `data-class`, `tools` (space-separated), `surfaces` and `ships-in`.
   - **`content/tools.yaml`** is the registry of tools: `name`, `system`, and `kind: read|internal|write`. Only `write` tools (outside PM OS) need approval.
     - Lint errors: a skill tool missing from the registry, a bad data class, or a duplicate skill name.
   - **Context files** need front matter with `owner`, `last-reviewed` (a date) and `sensitivity` (`restricted`, `internal` or `public`). The domain comes from the folder, one of:
     - products, personas, competitors, partners, stakeholders, style, glossary, decisions, exemplars.
     - Lint: a missing field is an error; more than 90 days since review is a stale warning (CTX-2).
   - **Reviewers** need `name`, `lens` and `flags` in front matter; the body is the rubric. **Templates** need `owner` and `version`.
4. **Harness skeleton (TypeScript, pnpm).**
   - `harness/src/registry.ts` loads `tools.yaml`. `buildInterruptOn()` maps every write tool to approve, edit or reject.
   - `harness/src/agent.ts` builds `createPmosAgent({gatewayUrl, model, contentRoot, user, tools})`:
     - `ChatOpenAI` pointed at the gateway's `baseURL`.
     - `CompositeBackend(StateBackend, {"/content/": FilesystemBackend({rootDir, virtualMode:true})})`.
     - `permissions: [{operations:["write"], paths:["/content/**"], mode:"deny"}]`.
     - `skills: ["/content/skills/"]`.
     - `memory: ["/content/master/AGENTS.md", "/content/master/people/<user>/AGENTS.md"]`.
5. **Seed content** (0b) is included, so the lint has real files to pass.
6. **CI** in `.github/workflows/ci.yml`: `cargo fmt --check`, `clippy -D warnings`, `cargo test`, `pmos lint content`, and `pnpm -C harness typecheck test`.

## Verification
- `cargo test`: unit tests for each lint rule, plus integration tests in `crates/contract/tests/lint.rs` that build a valid tree, break one thing and check the lint catches it.
- `cargo run -p pmos -- lint --root content` passes on the seed content. Changing a date makes it report stale.
- `pnpm -C harness test`: `buildInterruptOn` covers every write tool and nothing else. A smoke test builds the agent with a fake chat model (`@langchain/core/utils/testing`), loads the 3 skills, and rejects a write under `/content/`.
- CI is green on the pull request.

## Settled after plan review
- **Systems of record are Jira and Confluence.** Confluence is the doc tool, so drafts land as Confluence pages (OUT-1).
  - The tool server has `jira.*` and `confluence.*` connectors (Atlassian Cloud REST, using each user's own OAuth 2.0 (3LO) token).
  - `content/tools.yaml` in PR 1 registers, for example, `jira.search`/`jira.read` (read), `jira.create_issue` (write), `confluence.search`/`confluence.read` (read), and `confluence.create_draft`/`confluence.update_page` (write).
- **Everything uses the user's own token.** Connectors act as the signed-in user, and Slack replies post under the PM's own Slack user token, not as the bot (SLK-7).
  - The bot token only receives events and sends the PM the review cards in DM.
  - The posted reply still carries the "drafted with PM OS" marker.
  - User tokens are stored encrypted in the tool server and attached only to that user's sandbox (OpenShell credential placeholders).

- **Atlassian Cloud is confirmed.** Connectors use Atlassian Cloud REST with per-user OAuth 2.0 (3LO), through `api.atlassian.com/ex/{jira|confluence}/{cloudId}`.
- **Models are local and open-weight only.** The gateway has no outbound model route at all.
- **The platform owners are product managers**, not a dedicated engineer.
  - Platform-owner rights go to named PMs in the `admin` role.
  - Sandbox-policy changes still need a second platform-owner PM who isn't the author (POL-2).
  - Operations must stay PM-runnable:
    - one `docker compose` stack on the Spark;
    - `pmos admin` commands for health, restart, model swap and token revocation;
    - day-2 runbooks in `docs/runbooks/` written for a technical PM;
    - upgrades only through the eval gate.
  - Gate 1 adds "a platform-owner PM can restore from backup by following the runbook" (`docs/runbooks/backup-restore.md`).

## Status
| Package | State | Notes |
| --- | --- | --- |
| 0a Contract and lint | Done | `crates/contract`, `crates/cli`, CI |
| 0b Seed content | Done | 3 skills, 7 reviewers, 3 templates, context skeleton |
| 0c Tool server | Done (fakes) | `crates/toolserver`; see `docs/toolserver.md` |
| 0c-2a Postgres stores | Done | Every store in Postgres behind async traits; same contract tests on both backends; CI runs Postgres |
| 0c-2b Real Atlassian and Slack connectors, encrypted token store | Done | Each PM's own token; AES-256-GCM vault; OAuth 3LO and Slack v2; tested against local mock APIs; first real call after Gate 1 |
| Public docs + `/docs` in Slack | Done | `crates/docs` (`pmos-docs` MCP server over your docs site, `PMOS_DOCS_BASE_URL`), docs connector and `docs` skill in the tool server, `crates/slack` (Socket Mode, `/docs`), `content/team.yaml`; see `docs/docs-server.md` |
| Staff-engineer review fixes | Done | Four reviews (security, correctness, scale and operations, design and tests), checked against the code. The 8 blockers and the pilot list are fixed; see **Review follow-ups** for what's left |
| 0d Gateway | Done | `crates/gateway`: role aliases to local vLLM only, fair queue (interactive, scheduled, personal; per-person cap), usage log, workaround registry (D11), stub model for CI; see `docs/gateway.md` |
| 0e Bots | Built (mocks and stub model) | `content/bots`, `content/roles` and their lint; tool-server bot sessions (allowlist per role, `never` actions, team-bot read scope with its own account); `crates/bots` runtime (durable tasks with leases, routines, 30-day traces, notifications, one worker token per bot); `harness` worker; Slack DMs, progress, approval cards and routines; `crates/console`. Checked end to end with the real binaries and the stub model. See `docs/bot-runtime-spec.md` |
| 0f Sandbox policies | Built; verify on the Spark | `crates/policy`, `pmos policy generate/check` in CI, `policies/bots/docs.yaml`, red-team kit. Format checked against OpenShell's repo; items marked "verify on the Spark" in `docs/sandbox-policies.md` |
| 0g Evals | Built; first live run on the Spark | `crates/evals`, `evals/categories/docs-qa` (20 real questions), failure tags, weekly reports, load test. Latency is reported, not gated (decided 2026-11-14) |
| Onboarding and GitHub memory | Built (this PR); first live run on the Spark | New members ask in Slack, a platform owner approves, PM OS commits `team.yaml` and their personal bot; guided setup; each PM's memory in a private repo in your GitHub org ([spec](onboarding-spec.md)) |
| Spark deployment kit | Built (this PR); first run on the Spark | `deploy/`: one Compose stack (Postgres, docs, tool server, gateway, bots, Slack, console, two vLLM servers), the service and worker images, `setup.sh`, `pmos-admin` (status, logs, backup and restore, reindex, revoke, bot sandboxes, smoke test, benchmarks), and runbooks in `docs/runbooks/`. CI builds the images, starts the stack and runs a task end to end. |
| Improvement loop, phases 0–1 (tooling) | Built (this PR); runs on the Spark | From the deep research ([report](research/self-improvement-loops.md), [spec](improvement-loop-spec.md)). **Measurement:** a feedback ledger of every draft's outcome, one-tap reject reasons, a daily triage digest for each bot's quality owner. **Benchmarks:** 3 runs per task, pass^3 and error bars, flips and a gate, should-decline cases, checks on drafts, second-level failure tags, calibrated judges, seed draft categories. **Running them:** an eval stack, and an eval gate on PRs (self-hosted Spark runner). **Later, each after its gate:** shadow and canary releases, the improver bot (phase 2), training (phase 3). |
| Slack reply assistant (SLK-1 to SLK-8) | Built (this PR); first live run on the Spark | `reply assistant on` starts a routine on the PM's personal bot that finds questions to them (with their own token), drafts grounded replies with the `slack-reply` skill, one per thread, and shows each on a card (Post as you, Edit, Snooze, Reject). The audience check blocks restricted drafts where outsiders would see them and asks again for Slack Connect. Drafts expire after 24 hours and their text is deleted ([how it works](slack-reply.md), [spec](slack-reply-spec.md)). |
| Tagging in channels and the App Home | Built (this PR); first live run on the Spark | **Tagging:** `@PM OS` in a thread starts the PM's personal bot on that thread, read with the PM's own token; the draft arrives in their DM on the reply card, one per tagged message (`slack.post_reply` gains `message_ts`). **App Home:** an Inbox to tick drafts and approve or reject them together, Skills grouped by each skill's new `track`, and Accounts to connect or disconnect. The tool server lets the Slack app list a member's pending approvals and unlink a member's account. The Slack app needs updating and reinstalling ([how it works](slack-home.md), [spec](slack-mentions-and-home-spec.md)) |
| Claude test in a test Slack workspace (testing only) | Built; **deferred until the test workspace is ready** (2026-11-15) | Every service on one machine with Claude behind the gateway, fake Jira, Confluence and Slack, and your public docs site, if one is set. Waiting on a test Slack workspace and app, plus four environment variables: `PMOS_SLACK_BOT_TOKEN`, `PMOS_SLACK_APP_TOKEN`, `PMOS_TEST_SLACK_ID` and `PMOS_GATEWAY_ANTHROPIC_KEY` ([guide](testing-with-claude.md)) |
| PM craft (senior-PM bar) | Built (this PR); the bar is measured on the Spark | PM OS works to a written bar: problem first, alternatives, a recommendation, a measurable outcome with a guardrail, the riskiest assumption. **Content:** six playbooks, 23 skills on one pattern (ask, draft from a template and an exemplar, check against the bar, propose a Confluence draft), 18 templates, five exemplars, four new reviewers (`senior-pm`, `data-scientist`, `design-ux`, `ai-safety`), and an AI-product track. **Code:** the review panel as read-only sub-agents (up to 4 a run), `stats.sample_size` and `stats.compare` in the tool server, the `reasoning` model role, playbook lint. **Benchmarks:** `evals/categories/pm-craft` (23 seed tasks, structure checks) and three advisory judges. Whether the local models meet the bar is decided by those benchmarks on the real stack; the `reasoning` role can be pointed at a larger model if they don't ([how PMs use it](pm-craft.md), [spec](pm-craft-spec.md)). |
| Staff and Principal PMs | Built (this PR); measured on the Spark | A level each PM sets (`senior`, `staff`, `principal`; `level staff` in a DM) that changes the scope, horizon and audience the bot works at. **Content:** 12 skills in two tracks (Leading across teams, Company bets), 10 templates, 3 playbooks, 3 reviewers (`cfo`, `cto`, `ceo`) and 4 exemplars. **Code:** the level in the bot runtime (a settings store, two routes, `level` in the claim), `jira.rollup` across teams, `stats.cashflow` for business cases, and the bets register (`bets.list`, `bets.read`, `bets.record`) with a monthly review routine. **Benchmarks:** `evals/categories/staff-craft` and `principal-craft` (8 seed tasks each) and two advisory judges. Whether the local models work at this level is decided by those benchmarks on the real stack ([how PMs use it](levels.md), [spec](staff-principal-spec.md)). |
| **Gate 1** | Needs the Spark | vLLM with the chosen models; OpenShell; the Slack app and the GitHub App; the first platform owners in `team.yaml` (everyone else onboards through the bot); the red-team run; two passing benchmark weeks; the load test (every task finishes) |

0c notes:
- Connectors share one `Connector` trait (`read` and `write`, both keyed by action) instead of one trait per system. The real Jira, Confluence and Slack connectors each implement it.
- Writes that the PM approved in the harness run through single-use grants, so the PM is never asked twice.

## Review follow-ups
From the staff-engineer review (2026-11-14). Fixed before any real token: every blocker and
the pilot list. Still open, roughly in priority order:

**Security and code-practices review (2026-11-15).** Fixed in batch 1
([spec](security-fixes-spec.md)), before the Spark's CI runner is registered or any real PM
token is used:
- ~~S1: the eval gate ran a pull request's own `pmos-admin` beside the Spark's secrets.~~ Fixed:
  `pull_request_target`, the Spark's installed script, the PR checked out as data.
- ~~S2: the Slack bot's token could grant admin, redirect a draft by editing it, read any PM's
  approvals by id, and see approval ids in the ledger.~~ Fixed.
- ~~S3: the benchmark token could run tasks as any PM and reach Slack.~~ Fixed:
  `PMOS_EVALS_AS`, no Slack target, no notifications.
- ~~S4: the console held the tool server's service token.~~ Fixed: its own read-only token.
- ~~S5: the console believed any `X-Forwarded-User`, and a form with no `Origin` or
  `Referer`.~~ Fixed: `PMOS_CONSOLE_PROXY_SECRET`, and both headers required.
- ~~S6: notes and bets didn't carry the sensitivity of what they were built from.~~ Fixed.
- ~~S7: a Confluence update could overwrite a newer version of the page.~~ Fixed.
- ~~S10–S14, S17 and S24: untrusted bets output, `bet-` memory keys, the sample-size
  overflow, memory tools for team bots, write-argument limits, tokens in debug output,
  `deploy/*.env` in the build context, internal detail in write errors.~~ Fixed.

Still open from that review:
- **S8:** the gateway trusts the caller's user and priority headers (`X-Pmos-User`,
  `X-Pmos-Priority`), so one bot token can spend another PM's share or jump the queue.
- **S9:** a team bot's Jira rollup shows links and epics outside its scope.
- **S15:** a refused token refresh can delete a newly re-linked account (the delete isn't
  conditioned on the row the refresh read).
- **S16:** routines have no minimum interval or per-owner cap.
- **S18:** the worker lives at `/opt/pmos`, but the sandbox policy allows `/app`.
- **S19:** a task's approval ids aren't checked against its session, so a worker can attach
  another run's approvals to its task.
- **S20:** the remote test upstream's signature cache isn't keyed by caller.
- **S21:** the local model servers have no API key on the shared network.
- **S22:** a worker call without its worker id isn't checked against the claim.
- **S23:** GitHub Actions are pinned to tags, not commit SHAs.
- **Code practices:**
  - P1: no request ids, traces or metrics across services;
  - P2: the tool server crate's size (one crate does MCP, connectors, OAuth, onboarding,
    memory and the ledger);
  - P3: loose error bodies on `/internal`, and three copies of the store error type;
  - P4: duplicated plumbing across services (config, bearer auth, Postgres setup);
  - P5: lock-poison panics (`.lock().unwrap()` / `expect` on the request path);
  - P6: no inbound request timeouts;
  - P7: file writes on the request path (audit file, Slack state);
  - P8: no TypeScript or shell lint in CI;
  - P9: Docker images aren't reproducible (unpinned base images and packages);
  - P10: `pmos-admin` has only a dry-parse test, and `deploy/.env` is parsed two ways (sourced
    by the shell, read by Compose);
  - P11: no task deadline in the worker;
  - P12: no CI job timeouts, and a compiler mismatch between CI and the images;
  - no LICENSE file.

**Before a second replica or Phase 3:**
- Move these to Postgres:
  - OAuth used states, as a table with a unique key;
  - refresh locks, as `pg_advisory_xact_lock`;
  - MCP sessions, by sticky routing or stateless MCP.
- Elect one docs-sync leader.

**Security hardening:**
- TLS (or mTLS) between services, and the public OAuth callback on its own listener, separate from `/internal` and `/mcp`. *Until then, the install runbook's reverse proxy exposes only `/oauth/*`; every service port is published on 127.0.0.1 only.*
- Untrusted-content wrapping:
  - wrap connector error text too;
  - give memory notes provenance, a cap, and approval when written from runs that read untrusted content.
- Slack posts:
  - escape `<!channel>`, mentions and link syntax;
  - show the raw text on approval cards;
  - set `unfurl_links=false`.
- Pin the Slack workspace or enterprise id in the bot.
- Vault:
  - authenticate `key_id` and the metadata fields in the AAD;
  - zeroize secrets in memory;
  - add a `pmos admin vault reseal` command for rows that are never read.
- Call Slack `auth.revoke` and end the user's sessions on unlink.

**Operations:**
- A request id per call (the run id), passed downstream, and `duration_ms` plus an error class in the audit.
- Audit retention: monthly partitions, and rotation for the JSON-lines file.
- Per-user concurrency and per-session call budgets on the tool server.
- An approval inbox with paging, expiry for pending items, and purging of stored draft bodies after N days (SLK-8).
- Paging (`offset`) for truncated Jira, Confluence and docs reads.

**Code structure:**
- Typed error codes on `/internal`, with request and response types in a shared `pmos-api` crate.
- Check at start-up that each connector handles every action registered for it.
- One product list, shared by `pmos-docs`, the tool server schema and `/docs`.
- Move the remaining shared setup into `pmos-runtime`: Postgres pool, bearer middleware, test fixtures.

**Duplicate writes:** beyond the "outcome unknown" message:
- record write attempts by argument hash;
- search Jira for the run-id marker before creating.

**CI:**
- `cargo deny` and `cargo audit`;
- one MSRV across crates;
- reuse the build between jobs;
- a TypeScript lint.

**Tests:**
- split the large Slack scenario test;
- clean up Postgres test schemas when a test panics.

## Open questions
- None blocking.
- Before building the bot (Phase 1): does your Slack plan allow the AI-app assistant panel? If not, the DM gives the same experience.
- The PRD still mentions a frontier API and an engineer as platform owner; suggest edits to the doc.
