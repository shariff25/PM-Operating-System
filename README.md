# PM OS

PM OS is the product team's agent workspace. It turns recurring PM work into one-command skills
(`/prd-draft`, `/meeting-notes`, `/review` and others), all drawing on one shared context
library and a panel of reviewer agents. It runs on the team's DGX Spark with open-weight models
only. Every run is in an NVIDIA OpenShell sandbox, and nothing is written to Jira, Confluence or
Slack without the PM's approval.

- Plan and phases: [`docs/implementation-plan.md`](docs/implementation-plan.md)
- File formats: [`docs/contract.md`](docs/contract.md)

## Repo layout

| Path | What it is | Language |
| --- | --- | --- |
| `content/` | Skills, context library, master context, reviewer rubrics, templates, tool registry | Markdown, YAML |
| `crates/contract` | The content contract and its lint | Rust |
| `crates/cli` | `pmos`, the command line for skill authors | Rust |
| `crates/gateway` | `pmos-gateway`: one OpenAI-compatible endpoint in front of the local models; roles, fair queueing, usage log ([docs](docs/gateway.md)) | Rust |
| `crates/runtime` | Small pieces every service shares: constant-time token checks, SIGTERM-aware shutdown | Rust |
| `crates/toolserver` | The only path from a sandbox to context and systems: MCP, allowlists, approvals, audit ([docs](docs/toolserver.md)) | Rust |
| `crates/docs` | `pmos-docs`: MCP server over the public Acme support docs ([docs](docs/docs-server.md)) | Rust |
| `crates/slack` | `pmos-slack`: the Slack bot: DMs to bots, progress, approval cards, routines, onboarding, `/docs` ([docs](docs/docs-server.md)) | Rust |
| `crates/bots` | `pmos-bots`: the bot runtime: durable tasks, routines, traces, notifications ([docs](docs/bots-runtime.md)) | Rust |
| `crates/console` | `pmos-console`: the four-screen console: bots, routines, approvals, activity ([docs](docs/console.md)) | Rust |
| `crates/policy` | `pmos policy`: one OpenShell sandbox policy per bot, checked in CI ([docs](docs/sandbox-policies.md)) | Rust |
| `crates/evals` | `pmos-evals`: benchmarks per task category, failure tags, load test ([docs](docs/evals.md)) | Rust |
| `content/bots`, `content/roles` | The named bots and the roles they're made from | Markdown |
| `policies/` | Generated sandbox policies, provider profiles and the red-team kit | YAML |
| `evals/` | Benchmark categories, tasks and weekly reports | YAML, Markdown |
| `harness/` | The agent on LangChain Deep Agents, and the worker that runs it in each bot's sandbox ([docs](harness/README.md)) | TypeScript |

Phase 0 is built. What's left needs the DGX Spark: vLLM serving the models, OpenShell, the
Slack app, and the first live runs (see the plan's **Status**).

## Develop

Requirements: Rust (stable), Node 22 and pnpm 10.

```sh
cargo test                                  # contract and CLI
cargo run -p pmos -- lint --root content    # check the content tree
pnpm install
pnpm typecheck && pnpm test                 # harness
```

## Contributing content

Change anything in `content/` by pull request. CI runs `pmos lint`. Context files need an
owner, a `last-reviewed` date and a sensitivity label, and the file's owner reviews the change.
To add a skill, copy one from `content/skills/`, rename the folder and its `name`, and declare
only the tools it needs.
