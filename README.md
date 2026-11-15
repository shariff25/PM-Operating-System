# PM OS

PM OS is the product team's agent workspace. It turns recurring PM work into one-command skills
(`/prd-draft`, `/meeting-notes`, `/review` and others), all drawing on one shared context
library and a panel of reviewer agents. It runs on the team's DGX Spark with open-weight models
only. Every run is in an NVIDIA OpenShell sandbox, and nothing is written to Jira, Confluence or
Slack without the PM's approval.

- Plan and phases: [`docs/implementation-plan.md`](docs/implementation-plan.md)
- File formats: [`docs/contract.md`](docs/contract.md)
- Working with it as a PM: [`docs/pm-craft.md`](docs/pm-craft.md); at Staff or Principal level: [`docs/levels.md`](docs/levels.md)

## What it does

PM OS works to a senior-PM bar. It asks who has the problem before it drafts a solution. Every
document weighs real alternatives, opens with one recommendation, names a measurable outcome
and its riskiest assumption, and is checked against a written bar
(`content/playbooks/bar.md`) before the PM sees it. Sample sizes and test results come from
tools, not the model's arithmetic.

| Track | Skills |
| --- | --- |
| Everyday | `ask`, `docs`, `meeting-notes`, `slack-reply` |
| Product sense | `product-critique`, `problem-framing` |
| Strategy and decisions | `strategy-doc`, `decision-doc`, `prioritize`, `pre-mortem` |
| Metrics and experiments | `metrics-tree`, `experiment-design`, `experiment-readout` |
| Spec and review | `prd-draft`, `review` (a panel of 11 reviewers, up to 4 a run, read-only) |
| Customer and market | `call-synth`, `competitive-brief`, `opportunity-sizing`, `poc-plan`, `partner-brief` |
| AI products | `ai-feature-spec`, `eval-plan` |
| Delivery and launch | `release-notes`, `status-update`, `launch-checklist`, `jira-draft`, `advisory-draft` (drafts only; security and legal approve first) |
| Leading across teams | `portfolio-review`, `roadmap-tradeoffs`, `okr-draft`, `business-review`, `alignment-plan`, `coach-feedback`, `bets-register` |
| Company bets | `vision-doc`, `investment-case`, `pricing-proposal`, `platform-strategy`, `exec-brief` |

**It works at your level.** Each PM sets `senior` (the default), `staff` or `principal`
(`level staff` in a DM), which changes the scope, horizon and audience of what PM OS writes.
Staff and Principal PMs also get a rollup of Jira issues across teams, cash-flow numbers for
business cases, and a register of bets with a monthly review
([`docs/levels.md`](docs/levels.md)).

What each skill is for, with an example ask, is in [`docs/pm-craft.md`](docs/pm-craft.md). The
tracks are each skill's `track` field, and the Slack App Home lists skills by them.

In Slack, tag `@PM OS` in a thread to get a draft reply in your DM
([how](docs/slack-reply.md)), and open the app's **Home** for your waiting drafts, the skills
and your connected accounts ([how](docs/slack-home.md)).

## Repo layout

| Path | What it is | Language |
| --- | --- | --- |
| `content/` | Skills, playbooks, context library, master context, reviewer rubrics, templates, tool registry | Markdown, YAML |
| `crates/contract` | The content contract and its lint | Rust |
| `crates/cli` | `pmos`, the command line for skill authors | Rust |
| `crates/gateway` | `pmos-gateway`: one OpenAI-compatible endpoint in front of the local models; roles, fair queueing, usage log ([docs](docs/gateway.md)) | Rust |
| `crates/runtime` | Small pieces every service shares: constant-time token checks, SIGTERM-aware shutdown | Rust |
| `crates/toolserver` | The only path from a sandbox to context and systems: MCP, allowlists, approvals, audit ([docs](docs/toolserver.md)) | Rust |
| `crates/docs` | `pmos-docs`: MCP server over your company's public docs site ([docs](docs/docs-server.md)) | Rust |
| `crates/slack` | `pmos-slack`: the Slack bot: DMs to bots, progress, approval cards with reject reasons, tagging in threads, the App Home, the triage digest, routines, onboarding, `/docs` ([docs](docs/docs-server.md)) | Rust |
| `crates/bots` | `pmos-bots`: the bot runtime: durable tasks, routines, traces, notifications ([docs](docs/bots-runtime.md)) | Rust |
| `crates/console` | `pmos-console`: the four-screen console: bots, routines, approvals, activity ([docs](docs/console.md)) | Rust |
| `crates/policy` | `pmos policy`: one OpenShell sandbox policy per bot, checked in CI ([docs](docs/sandbox-policies.md)) | Rust |
| `crates/evals` | `pmos-evals`: benchmarks per task category, failure tags, load test ([docs](docs/evals.md)) | Rust |
| `content/bots`, `content/roles` | The named bots and the roles they're made from | Markdown |
| `policies/` | Generated sandbox policies, provider profiles and the red-team kit | YAML |
| `evals/` | Benchmark categories, tasks and weekly reports | YAML, Markdown |
| `harness/` | The agent on LangChain Deep Agents, and the worker that runs it in each bot's sandbox ([docs](harness/README.md)) | TypeScript |
| `deploy/` | Running PM OS on the Spark: one Compose stack, images, `setup.sh` and `pmos-admin` ([runbooks](docs/runbooks/README.md)) | Docker, Bash |

Phase 0 is built. What's left needs the DGX Spark: vLLM serving the models, OpenShell, the
Slack app, and the first live runs (see the plan's **Status**).

## Make it your team's

PM OS isn't tied to one company. To set it up for yours:
1. Fill in the `TODO(owner)` markers in `content/master/AGENTS.md`: the company, products,
   audiences and voice.
2. Copy `content/context/products/example-product.md` once per product, and add your product
   names to `content/context/glossary/terms.md`.
3. Point the Docs bot at your public docs site with `PMOS_DOCS_BASE_URL` in `deploy/.env`,
   and optionally a products file ([docs](docs/docs-server.md#pointing-the-docs-bot-at-your-docs)).
4. Write your own `docs-qa` benchmark questions against your docs (`docs/evals.md`).
5. Make the bar your own: edit `content/playbooks/bar.md`, and replace the exemplars in
   `content/context/exemplars/` with your team's best documents
   ([docs](docs/pm-craft.md#make-the-bar-your-own)).

## Run it on the Spark

```sh
deploy/setup.sh --content /srv/pmos/content-repo --clone <content repo>
deploy/pmos-admin build && deploy/pmos-admin up && deploy/pmos-admin status
```

The full install, up to Gate 1, and day-2 operations are in [docs/runbooks](docs/runbooks/README.md).

## Develop

Requirements: Rust (stable), Node 22 and pnpm 10.

```sh
cargo test                                  # contract and CLI
cargo run -p pmos -- lint --root content    # check the content tree
pnpm install
pnpm typecheck && pnpm test                 # harness
```

To try PM OS end to end before the Spark is ready, run it against a test Slack workspace with
a Claude model behind the gateway (testing only): [docs/testing-with-claude.md](docs/testing-with-claude.md).

## Contributing content

Change anything in `content/` by pull request. CI runs `pmos lint`. Context files need an
owner, a `last-reviewed` date and a sensitivity label, and the file's owner reviews the change.
To add a skill, copy one from `content/skills/`, rename the folder and its `name`, and declare
only the tools it needs.
