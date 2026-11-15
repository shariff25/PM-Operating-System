#!/usr/bin/env bash
# TESTING ONLY: run every PM OS service on this machine against a test Slack workspace, with a
# Claude model behind the gateway (docs/testing-with-claude.md).
#
# What runs: pmos-docs (your public docs site, if PMOS_DOCS_BASE_URL is set), the tool server (fake Jira, Confluence and
# Slack fixtures), the gateway (Claude test upstream), the bot runtime, one harness worker per
# bot (docs and pm-<you>), and the Slack bot. Everything keeps its state in memory and in
# $STATE; nothing touches the real content repo.
#
# Needs, in the environment (never in a file in the repo):
#   PMOS_SLACK_APP_TOKEN  the test Slack app's app-level token (xapp-…, connections:write)
#   PMOS_SLACK_BOT_TOKEN  the test Slack app's bot token (xoxb-…)
#   PMOS_TEST_SLACK_ID    your member id in the test workspace (U…)
#   PMOS_GATEWAY_ANTHROPIC_KEY  a Claude API key
# Optional:
#   PMOS_TEST_HANDLE      your PM OS handle in the test (default: tester)
#   PMOS_TEST_STATE       where state and logs go (default: state/claude-test)
#   PMOS_DOCS_BASE_URL    your public docs site for the Docs bot, e.g. https://docs.example.com
#                         (also PMOS_DOCS_NAME, PMOS_DOCS_PAGE_PREFIXES, PMOS_DOCS_API_PREFIX and
#                         PMOS_DOCS_PRODUCTS; see docs/docs-server.md). Unset: the Docs bot
#                         says no docs site is configured.
#
# Stop with Ctrl-C; every service is stopped with it.
set -euo pipefail

: "${PMOS_SLACK_APP_TOKEN:?set PMOS_SLACK_APP_TOKEN (the test app's xapp- token)}"
: "${PMOS_SLACK_BOT_TOKEN:?set PMOS_SLACK_BOT_TOKEN (the test app's xoxb- token)}"
: "${PMOS_TEST_SLACK_ID:?set PMOS_TEST_SLACK_ID (your member id in the test workspace)}"
: "${PMOS_GATEWAY_ANTHROPIC_KEY:?set PMOS_GATEWAY_ANTHROPIC_KEY (a Claude API key)}"
HANDLE="${PMOS_TEST_HANDLE:-tester}"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
STATE="${PMOS_TEST_STATE:-state/claude-test}"
mkdir -p "$STATE/logs" "$STATE/slack"
STATE="$(cd "$STATE" && pwd)"

echo "== building"
cargo build -q -p pmos -p pmos-docs -p pmos-toolserver -p pmos-gateway -p pmos-bots -p pmos-slack
(cd harness && pnpm -s install --frozen-lockfile >/dev/null && pnpm -s build)
BIN="$ROOT/target/debug"

echo "== test content in $STATE/content (you as admin, plus your bot pm-$HANDLE)"
rm -rf "$STATE/content"
cp -r content "$STATE/content"
"$BIN/pmos" member add --root "$STATE/content" --handle "$HANDLE" \
  --slack-id "$PMOS_TEST_SLACK_ID" --roles pm,admin >/dev/null
"$BIN/pmos" lint --root "$STATE/content" | tail -1

# Fresh tokens between the services for this run only.
tok() { openssl rand -hex 24; }
DOCS_TOKEN=$(tok) SERVICE_TOKEN=$(tok) BOT_TOKEN=$(tok)
GW_RUNTIME=$(tok) GW_WORKER=$(tok)
RT_SLACK=$(tok) RT_DOCS=$(tok) RT_PERSONAL=$(tok)
BOT_PERSONAL="pm-$HANDLE"

PIDS=()
start() { # name, then the command; logs go to $STATE/logs/<name>.log
  local name=$1
  shift
  "$@" >"$STATE/logs/$name.log" 2>&1 &
  PIDS+=($!)
  echo "   $name (pid $!)"
}
stop() {
  echo
  echo "== stopping"
  for pid in "${PIDS[@]}"; do kill "$pid" 2>/dev/null || true; done
  wait 2>/dev/null || true
}
trap stop EXIT INT TERM
wait_for() { # url, name
  for _ in $(seq 1 60); do
    if curl -fsS --noproxy '*' "$1" >/dev/null 2>&1; then return 0; fi
    sleep 1
  done
  echo "error: $2 didn't come up; see $STATE/logs/$2.log" >&2
  exit 1
}

echo "== starting"
start docs env PMOS_DEV=1 PMOS_DOCS_TOKEN="$DOCS_TOKEN" PMOS_DOCS_BIND=127.0.0.1:8730 \
  "$BIN/pmos-docs"
wait_for http://127.0.0.1:8730/healthz docs

start toolserver env PMOS_DEV=1 PMOS_CONNECTORS=fake \
  PMOS_SERVICE_TOKEN="$SERVICE_TOKEN" PMOS_BOT_TOKEN="$BOT_TOKEN" \
  PMOS_CONTENT_ROOT="$STATE/content" PMOS_AUDIT_LOG="$STATE/audit.jsonl" \
  PMOS_DOCS_URL=http://127.0.0.1:8730/mcp PMOS_DOCS_TOKEN="$DOCS_TOKEN" \
  "$BIN/pmos-toolserver"
wait_for http://127.0.0.1:8710/healthz toolserver

start gateway env PMOS_GATEWAY_CLOUD_TEST=1 PMOS_GATEWAY_CONFIG=config/gateway.claude-test.yaml \
  PMOS_GATEWAY_TOKENS="runtime=$GW_RUNTIME,worker=$GW_WORKER" \
  PMOS_GATEWAY_USAGE_LOG="$STATE/usage.jsonl" \
  "$BIN/pmos-gateway"
wait_for http://127.0.0.1:8740/healthz gateway

start bots env PMOS_DEV=1 PMOS_CONTENT_ROOT="$STATE/content" \
  PMOS_BOTS_TOKENS="slack=$RT_SLACK,worker-docs=$RT_DOCS,worker-$BOT_PERSONAL=$RT_PERSONAL" \
  PMOS_TOOLSERVER_URL=http://127.0.0.1:8710 PMOS_SERVICE_TOKEN="$SERVICE_TOKEN" \
  PMOS_GATEWAY_URL=http://127.0.0.1:8740/v1 PMOS_GATEWAY_TOKEN="$GW_RUNTIME" \
  "$BIN/pmos-bots"
wait_for http://127.0.0.1:8750/healthz bots

worker() { # bot, token
  start "worker-$1" env PMOS_BOTS_URL=http://127.0.0.1:8750 PMOS_WORKER_BOT="$1" \
    PMOS_WORKER_TOKEN="$2" PMOS_GATEWAY_URL=http://127.0.0.1:8740/v1 \
    PMOS_GATEWAY_TOKEN="$GW_WORKER" PMOS_TOOLSERVER_MCP_URL=http://127.0.0.1:8710/mcp \
    PMOS_CONTENT_ROOT="$STATE/content" NO_PROXY="127.0.0.1,localhost" \
    node harness/dist/main.js
}
worker docs "$RT_DOCS"
worker "$BOT_PERSONAL" "$RT_PERSONAL"

start slack env PMOS_SLACK_APP_TOKEN="$PMOS_SLACK_APP_TOKEN" \
  PMOS_SLACK_BOT_TOKEN="$PMOS_SLACK_BOT_TOKEN" \
  PMOS_TOOLSERVER_URL=http://127.0.0.1:8710 PMOS_BOT_TOKEN="$BOT_TOKEN" \
  PMOS_BOTS_URL=http://127.0.0.1:8750 PMOS_BOTS_TOKEN="$RT_SLACK" \
  PMOS_CONTENT_ROOT="$STATE/content" PMOS_SLACK_STATE_DIR="$STATE/slack" \
  "$BIN/pmos-slack"

echo
echo "== PM OS test stack is up. In the test Slack workspace, DM the PM OS app:"
echo "   docs: how do I get started?         (the Docs bot, your docs site)"
echo "   summarize the open launch issues    (your bot $BOT_PERSONAL, fake Jira)"
echo "   setup                               (the onboarding checklist)"
echo "   Logs: $STATE/logs. Ctrl-C stops everything."
wait -n "${PIDS[@]}" || true
echo "a service stopped; see $STATE/logs" >&2
