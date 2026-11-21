# Testing PM OS with Claude and a test Slack workspace

**Testing only (decided 2026-11-14).** Before the Spark's local models are up, PM OS can be
tried end to end with a Claude model behind the gateway, in a Slack workspace made for testing.
Prompts to Claude leave the machine. So this setup uses only:
- a test Slack workspace;
- fake Jira, Confluence and Slack fixtures;
- your company's public docs site (`PMOS_DOCS_BASE_URL`), if you set one.

Never use it with real PM tokens or company data. The production rule (local open-weight
models only) is unchanged: the gateway refuses the Claude upstream unless
`PMOS_GATEWAY_CLOUD_TEST=1` is set.

The same stack can use another provider's model instead of Claude: see
[Another provider](#another-provider).

## What you can test

| Try | What happens |
| --- | --- |
| `docs: how do I get started?` | The Docs bot searches your docs site and answers with links (with no `PMOS_DOCS_BASE_URL`, it says no docs site is configured) |
| Any other DM | Your personal bot `pm-<you>` answers from the fake Jira, Confluence and Slack fixtures and the team context |
| A request to draft a Jira issue or a Slack reply | An **Approve / Edit / Reject** card. Approving writes to the fixtures only. |
| `every weekday at 9am, summarize the launch issues` | A routine is parsed and confirmed |
| `setup` | The onboarding checklist. Its Connect links can't complete here, because the OAuth callback needs a public URL. |
| `/docs rotate a key` | The `/docs` shortcut |
| `requests`, `remove <handle>` | Platform-owner commands. You are an admin in the test stack's user database (its seed). |

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
- copies `content/` to `state/claude-test/content` and adds
  your bot `pm-<handle>` there (`pmos bot add-personal`), and you as `pm, admin` in the user
  database's seed (`$STATE/users.seed.yaml`, beside the content, never in it);
- starts the services with fresh tokens between them;
- deletes the Slack service's `notifications.cursor` from the last run, since the in-memory
  runtime restarts its notification ids at 1 and a stale cursor would make the bot skip replies;
- prints what to try.

**Logs** are in `state/claude-test/logs`. The gateway's usage log
(`state/claude-test/usage.jsonl`) records each Claude call: who made it, the role and the
tokens, never the text. **Ctrl-C** stops everything.

## The web app (optional)

To try the web app (`web/` and its backend `pmos-web`) against this stack, run
`PMOS_TEST_WEB=1 scripts/claude-test-stack.sh`. It builds the app and the backend, starts
`pmos-web` on `127.0.0.1:8770` with development sign-in (this machine only) and prints the
address. Without the variable nothing changes. See [Running the web app](web-app-running.md),
including why it isn't ready for production.

## Benchmarks (optional)

`PMOS_TEST_EVALS=1 scripts/claude-test-stack.sh` also sets the stack up for `pmos-evals`: the
benchmark member `evals` with a personal bot `pm-evals` and every offered persona on its team,
the runner's and judges' tokens, and the fake docs fixtures. It can't be combined with
`PMOS_TEST_WEB=1`. See "On the test stack" in [Benchmarks](evals.md).

## The model

`config/gateway.claude-test.yaml` maps both roles to `claude-opus-5-5`:
- **`pmos/default`** at effort `medium`;
- **`pmos/extraction`**, which parses routines, at effort `low`.

The gateway translates the callers' OpenAI-style requests to the Claude Messages API
(`crates/gateway/src/anthropic.rs`). Sampling parameters are dropped, server-side refusal
fallbacks are on, and a refusal reaches the bot as `finish_reason: "content_filter"`.

**Cost:** Claude Opus 5.5 costs $4 per million input tokens and $20 per million output tokens.
A Docs-bot answer is usually a few thousand tokens. The usage log shows the totals.

## Another provider

The stack can use any OpenAI-compatible API instead of Claude. The gateway forwards the
callers' requests to it as they are, with no translation (`docs/gateway.md`, "Remote test
upstreams"). The same testing-only rules apply: prompts leave the machine, so use the test
Slack workspace, the fake fixtures and your public docs site only, never real PM tokens or
company data.

**Set these** instead of `PMOS_GATEWAY_ANTHROPIC_KEY`, outside the repo like the others:

| Variable | What it is |
| --- | --- |
| `PMOS_TEST_MODEL_URL` | The provider's OpenAI-compatible base URL, `https` only. The gateway calls `<url>/chat/completions`. |
| `PMOS_TEST_MODEL` | The model name for the `default`, `reasoning` and `extraction` roles. It must support tool calling. |
| `PMOS_GATEWAY_REMOTE_KEY` | The provider's API key, sent as `Authorization: Bearer`. It is never logged. |
| `PMOS_TEST_MODEL_FAST` | Optional: a different model for `extraction` (routine parsing) |

Then run the same script:

```sh
scripts/claude-test-stack.sh
```

With `PMOS_TEST_MODEL_URL` set, the script writes `state/claude-test/gateway.yaml` with one
upstream, `openai+<url>`, and every role on `PMOS_TEST_MODEL`, and starts the gateway with it.
The usage log (`state/claude-test/usage.jsonl`) names the provider's host on every call.

**Worked example: Gemini.** Create an API key in Google AI Studio, then:

```sh
export PMOS_TEST_MODEL_URL=https://generativelanguage.googleapis.com/v1beta/openai
export PMOS_TEST_MODEL=gemini-flash-latest
export PMOS_GATEWAY_REMOTE_KEY=...   # the AI Studio key, from your shell or the environment's settings
scripts/claude-test-stack.sh
```

In a Claude Code cloud session, the network must also allow
`generativelanguage.googleapis.com`. Gemini returns a signature with each tool call and needs
it back on the next turn; the gateway keeps it for the bots, so nothing else changes.

**If calls fail:** the provider's own error reaches the bot and the gateway log with its
status. A 429 means the key's limits were reached; wait and try again. A 503 means the model is
busy; try again, or pick another model.
