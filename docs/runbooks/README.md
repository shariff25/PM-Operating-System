# Runbooks: running PM OS on the Spark

PM OS runs on one DGX Spark as one Docker Compose stack (`deploy/compose.yaml`), plus one
OpenShell sandbox per bot. These runbooks are for the platform owners, PMs listed as `admin` in
`team.yaml`. They assume you're comfortable in a terminal; nothing more.

Everything goes through one command, `deploy/pmos-admin`. Run it with no arguments to list its
commands.

| Runbook | When |
| --- | --- |
| [install.md](install.md) | Once: setting up the Spark, up to Gate 1 |
| [daily.md](daily.md) | Every day, and when someone says "PM OS is slow or down" |
| [backup-restore.md](backup-restore.md) | Backups (weekly at least), and putting one back |
| [upgrade.md](upgrade.md) | Deploying a new version of PM OS, and registering the Spark CI runner |
| [quality.md](quality.md) | For each bot's quality owner: the triage digest, trace reviews, case cards, judges and the eval gate |
| [model-swap.md](model-swap.md) | Changing which model serves a role (D11) |
| [bots-and-members.md](bots-and-members.md) | A new member's bot, pausing a bot, revoking someone's tokens |
| [incidents.md](incidents.md) | Something went wrong: a bot misbehaves, a token leaks, a service won't start |

## The pieces

| Service | Port (on the Spark) | What it does |
| --- | --- | --- |
| `postgres` | none | One database per service |
| `docs` | none | The Fortanix support docs index |
| `toolserver` | 127.0.0.1:8710 | The only way bots reach Jira, Confluence, Slack, context and memory. Also handles approvals, audit and the OAuth callback. |
| `gateway` | 127.0.0.1:8740 | The local models, with fair queueing between users |
| `bots` | 127.0.0.1:8750 | Tasks, routines, traces and notifications |
| `slack` | none | The PM OS Slack app (Socket Mode, outbound only) |
| `console` | 127.0.0.1:8760 | The small web console (optional) |
| `vllm-default`, `vllm-extraction` | none | The two model servers on the GPU |
| one sandbox per bot | n/a | That bot's harness worker, under its own OpenShell policy |

**Where things live:**

| Path | What |
| --- | --- |
| `/opt/pmos` | This repo: code and `deploy/` |
| `deploy/.env` | The settings and secrets. Keep a copy in the team password manager. |
| `deploy/live.env` | Atlassian, Slack OAuth and GitHub app settings (Gate 1) |
| `/srv/pmos/content-repo` | The content checkout; the tool server pulls it |
| `/srv/pmos/state` | The audit log, the gateway usage log and Slack state |
| `deploy/backups/` | Backups |
