# Testing PM OS with Claude and a test Slack workspace

**Testing only (decided 2026-11-14).** Before the Spark's local models are up, PM OS can be
tried end to end with a Claude model behind the gateway, in a Slack workspace made for testing.
Prompts to Claude leave the machine. So this setup uses only:
- a test Slack workspace;
- fake Jira, Confluence and Slack fixtures;
- the public Acme support docs.

Never use it with real PM tokens or Acme data. The production rule (local open-weight
models only) is unchanged: the gateway refuses the Claude upstream unless
`PMOS_GATEWAY_CLOUD_TEST=1` is set.

## What you can test

| Try | What happens |
| --- | --- |
| `docs: what ports does DSM need?` | The Docs bot searches the real support docs and answers with links |
| Any other DM | Your personal bot `pm-<you>` answers from the fake Jira, Confluence and Slack fixtures and the team context |
| A request to draft a Jira issue or a Slack reply | An **Approve / Edit / Reject** card. Approving writes to the fixtures only. |
| `every weekday at 9am, summarize the launch issues` | A routine is parsed and confirmed |
| `setup` | The onboarding checklist. Its Connect links can't complete here, because the OAuth callback needs a public URL. |
| `/docs rotate a key` | The `/docs` shortcut |
| `requests`, `remove <handle>` | Platform-owner commands. You are an admin in the test `team.yaml`. |

**Not in this test:** real Jira, Confluence or Slack reads, account linking, GitHub memory and
the approval commit. Those need the Spark setup (`docs/toolserver.md`).

## 1. Create the test Slack workspace and app

1. **Create the workspace.** Go to <https://slack.com/get-started#/createnew> and create a free
   workspace, such as "PM OS test".
2. **Create the app from the manifest.**
   - Open <https://api.slack.com/apps>, choose **Create New App**, then **From a manifest**.
   - Pick the test workspace and paste `docs/slack-app-manifest.yaml`.
   - Create the app and install it to the workspace.
3. **Copy the two tokens**, keeping them out of chat and out of the repo:
   - **Bot token** (`xoxb-…`): under **OAuth & Permissions**.
   - **App-level token** (`xapp-…`): under **Basic Information**, then **App-Level Tokens**.
     Generate one with the `connections:write` scope.
4. **Find your member id** (`U…`): in Slack, open your profile, then **⋯**, then **Copy member
   ID**.

## 2. Give the stack its keys

The stack reads these settings. None of them goes in a file in the repo.

| Variable | What it is |
| --- | --- |
| `PMOS_SLACK_BOT_TOKEN` | The bot token (`xoxb-…`) |
| `PMOS_SLACK_APP_TOKEN` | The app-level token (`xapp-…`) |
| `PMOS_TEST_SLACK_ID` | Your member id (`U…`); not secret |
| `PMOS_TEST_HANDLE` | Optional: your handle in the test (default `tester`) |
| `PMOS_GATEWAY_ANTHROPIC_KEY` | A Claude API key from the Claude Console (<https://platform.claude.com>). Use a key in a workspace with a spend limit set for testing. |

**In a Claude Code cloud session:** add the variables in the environment's settings (the
environment menu in the session's title bar, then **Edit**, then environment variables). They
reach a session when it starts or resumes. The network must also allow `slack.com`,
`wss-primary.slack.com` and `wss-backup.slack.com`; `api.anthropic.com` is always reachable.

**On your own machine:** export them in your shell. You need Rust, Node 22, pnpm and openssl.

## 3. Run it

```sh
scripts/claude-test-stack.sh
```

**What the script does:**
- builds everything;
- copies `content/` to `state/claude-test/content` and adds you there as `pm` and `admin`, with
  your bot `pm-<handle>` (`pmos member add`);
- starts the services with fresh tokens between them;
- prints what to try.

**Logs** are in `state/claude-test/logs`. The gateway's usage log
(`state/claude-test/usage.jsonl`) records each Claude call: who made it, the role and the
tokens, never the text. **Ctrl-C** stops everything.

## The model

`config/gateway.claude-test.yaml` maps both roles to `claude-opus-5-5`:
- **`pmos/default`** at effort `medium`;
- **`pmos/extraction`**, which parses routines, at effort `low`.

The gateway translates the callers' OpenAI-style requests to the Claude Messages API
(`crates/gateway/src/anthropic.rs`). Sampling parameters are dropped, server-side refusal
fallbacks are on, and a refusal reaches the bot as `finish_reason: "content_filter"`.

**Cost:** Claude Opus 5.5 costs $4 per million input tokens and $20 per million output tokens.
A Docs-bot answer is usually a few thousand tokens. The usage log shows the totals.
