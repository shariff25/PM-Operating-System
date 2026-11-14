# PM OS — implementation plan and phases

## Context
The PRD ("PM OS for the Fortanix Product Team", claude.ai artifact FwKYdeMgiLGd3rhC1Mc1UU) defines an internal agent
workspace for the product team, running on one DGX Spark: 8 launch skills, a 7-persona reviewer panel, a team
context library, a Slack reply assistant with human approval, every run in an NVIDIA OpenShell sandbox, pilot by
week 7 and full team by week 12. This repo (`shariff25/pm-operating-system`) is empty apart from a README. It becomes
both the team's content repo (the PRD's "files are the source of truth") and the service code.

Decisions already made in this conversation:
- **Backend in Rust; frontend in TypeScript.** The agent harness is also TypeScript, because Deep Agents has no Rust port.
- **Open-weight models only, no frontier API.** Qwen3.6-35B-A3B is the default model and Nemotron 3 Nano 30B-A3B handles extraction and triage, both on vLLM. The bake-off adds Qwen 3.5 122B and gpt-oss-120b as challengers.
- **Harness: LangChain Deep Agents (`deepagents` 1.16.5, MIT, TypeScript) for the team service; Hermes on NemoClaw for personal agents.**
- **Context layer:** markdown in git (`content/`), indexed into Postgres (pgvector + full-text search) on merge, served only through the Rust tool server's MCP tools. Memory also lives in Postgres and is exposed to the harness as `/memories/*.md`. There is no Letta, Mem0, Graphiti or Cognee in v1.

## Architecture
```
Slack app (Rust) ─┐
Web (TS, Ph2) ────┼─► orchestrator (Rust: queue, sandbox launcher) ─► OpenShell sandbox per session
CLI pmos (Rust) ──┘                                                     └─ harness (TS, deepagents)
                                                                           ├─ MCP ─► toolserver (Rust): connectors, context, memory, approvals, audit ─► Postgres
                                                                           └─ HTTP ─► gateway (Rust): open-weight routing, priority queue ─► vLLM
```
The sandbox network policy allows only the tool server and the gateway. All writes outside PM OS become approval items.

## Repo layout
```
content/   master/AGENTS.md, master/people/<handle>/AGENTS.md, context/<domain>/*.md,
           skills/<name>/SKILL.md, reviewers/*.md, templates/*.md, tools.yaml
policies/  team-session.yaml, personal-agent.yaml, scheduled-job.yaml
crates/    contract (types + lint), cli (pmos), toolserver, gateway, orchestrator, slack, policy-check
harness/   TS deepagents runner
web/       TS React + Vite (Phase 2)
evals/     golden tasks per skill, bake-off, load test
docs/      implementation-plan.md, contract.md
```
Rust: Cargo workspace using axum, sqlx, rmcp, clap, serde, chrono and slack-morphism. Each crate is checked for maintenance before it is added.
TypeScript: pnpm workspace with `harness/` now and `web/` in Phase 2.

## Phases

### Phase 0: Foundation (weeks 1–3) → Gate 1
No real tokens or data are used before Gate 1. Work packages are in build order; each is one PR.

1. **0a. Contract and lint.** First PR, detailed below.
2. **0b. Seed content.**
   - Team `AGENTS.md`.
   - Context folders, each with a README and a placeholder file. Owners and facts are left as TODO; no Fortanix facts are invented.
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
   - A load test for 5 concurrent sessions, measured against the PRD latency targets.
   - A measured time baseline for the core artifacts.

**What runs where:** steps 0a–0c and 0f–0g are built and tested here against fakes and a stub model. vLLM, the OpenShell runtime, real OAuth and the baseline need the Spark.

**Gate 1:** evals pass, the load test meets targets, and the sandbox red-team passes.

### Phase 1: Pilot (weeks 4–7), 2–3 champion PMs → Gate 2
- SSO and the three roles: PM, skill author, admin (ADM-1).
- The Slack bot, with DMs, slash commands and replies in thread (SUR-1).
- The `pmos` CLI running the same skills (SUR-2).
- The remaining launch skills: `/review` (parallel sub-agents, one critique grouped by severity), `/call-synth`, `/competitive-brief`, `/release-notes` and `/status-update`.
- The Slack reply assistant, SLK-1 to SLK-8: triage, grounded drafts, an approval card, an audience check, and a 24-hour expiry that deletes stored Slack content.
- Read connectors for call transcripts and CRM (OUT-3).

**Gate 2:** champions use it weekly, with no guardrail incident.

### Phase 2: Team launch (weeks 8–12) → Gate 3
- Web workspace in TypeScript with React and Vite: chat home, inbox, reviews, skills, scheduled jobs, policy view and audit log (WEB-1 to 4, SUR-3).
- API types generated from the Rust OpenAPI spec.
- Skill catalog and skill publishing by pull request (SKL-3/4), plus `/new-skill`.
- Memory (MEM-1/2), scheduled jobs and digests (SKL-5, SLK-9).
- Reviewer accept and dismiss (REV-3).
- Policy portal (POL-1 to 6) and usage dashboard (ADM-4).
- Personal-agent blueprint on Hermes/NemoClaw, with contract tests (MOD-2 to 6).
- Team-launch skills: `/poc-plan`, `/partner-brief`, `/advisory-draft`, `/jira-draft` and `/launch-checklist`.

**Gate 3:** at least 80% of PMs active weekly, and the success metrics are on track.

### Phase 3: Expand (week 13 on)
- Readers: PMM, SEs and engineering leads.
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

## Open questions (not blocking PR 1)
- Who the platform owner is. This blocks Gate 1.
- Whether to confirm Atlassian Cloud (as assumed) or Data Center. This changes the auth flow for the connectors.
- Updating the PRD text that still mentions a frontier API, as suggestions in the doc.
