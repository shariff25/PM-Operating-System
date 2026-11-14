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
| `crates/runtime` | Small pieces every service shares: constant-time token checks, SIGTERM-aware shutdown | Rust |
| `crates/toolserver` | The only path from a sandbox to context and systems: MCP, allowlists, approvals, audit ([docs](docs/toolserver.md)) | Rust |
| `crates/docs` | `pmos-docs`: MCP server over the public Fortanix support docs ([docs](docs/docs-server.md)) | Rust |
| `crates/slack` | `pmos-slack`: the Slack bot; today `/docs` ([docs](docs/docs-server.md)) | Rust |
| `harness/` | Team-service agent on LangChain Deep Agents | TypeScript |

Still to come, per the plan: the gateway, orchestrator and policy checker (Rust), the rest
of the Slack bot (mentions, assistant panel, approvals), sandbox policies, evals and a few
admin pages.

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
