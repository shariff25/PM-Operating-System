#!/usr/bin/env bash
# TESTING ONLY: run every PM OS service on this machine against a test Slack workspace, with a
# cloud model behind the gateway (docs/testing-with-claude.md). Prompts leave this machine, so
# use fake data only.
#
# Two model modes:
#   Claude (the default): config/gateway.claude-test.yaml, the Claude test upstream.
#   Another provider: set PMOS_TEST_MODEL_URL to any OpenAI-compatible API's base URL (e.g.
#     https://generativelanguage.googleapis.com/v1beta/openai). The script writes
#     $STATE/gateway.yaml with one `openai+<url>` upstream and every role on PMOS_TEST_MODEL.
#
# What runs: pmos-docs (your public docs site, if PMOS_DOCS_BASE_URL is set), the tool server (fake Jira, Confluence and
# Slack fixtures), the gateway (the test upstream), the bot runtime, one harness worker per
# bot (docs and pm-<you>), and the Slack bot. Everything keeps its state in memory and in
# $STATE; nothing touches the real content repo.
#
# Needs, in the environment (never in a file in the repo):
#   PMOS_SLACK_APP_TOKEN  the test Slack app's app-level token (xapp-…, connections:write)
#   PMOS_SLACK_BOT_TOKEN  the test Slack app's bot token (xoxb-…)
#   PMOS_TEST_SLACK_ID    your member id in the test workspace (U…)
# and for Claude:
#   PMOS_GATEWAY_ANTHROPIC_KEY  a Claude API key
# or for another provider:
#   PMOS_TEST_MODEL_URL   the provider's OpenAI-compatible base URL (https)
#   PMOS_TEST_MODEL       the model name for every role (it must support tool calling)
#   PMOS_GATEWAY_REMOTE_KEY  the provider's API key
#   PMOS_TEST_MODEL_FAST  optional: a model for the `extraction` role (default PMOS_TEST_MODEL)
# Optional:
#   PMOS_TEST_HANDLE      your PM OS handle in the test (default: tester)
#   PMOS_TEST_STATE       where state and logs go (default: state/claude-test)
#   PMOS_DOCS_BASE_URL    your public docs site for the Docs bot, e.g. https://docs.example.com
#                         (also PMOS_DOCS_NAME, PMOS_DOCS_PAGE_PREFIXES, PMOS_DOCS_API_PREFIX and
#                         PMOS_DOCS_PRODUCTS; see docs/docs-server.md). Unset: the Docs bot
#                         says no docs site is configured.
#
# Optional web app (docs/web-app-running.md):
#   PMOS_TEST_WEB=1       also build web/ and pmos-web, and serve the app at
#                         http://127.0.0.1:8770 with DEVELOPMENT sign-in (anyone who can reach
#                         the port signs in as any member of the test team; it binds to this
#                         machine only). Unset: nothing about the web app runs.
#
# Stop with Ctrl-C; every service is stopped with it.
set -euo pipefail

: "${PMOS_SLACK_APP_TOKEN:?set PMOS_SLACK_APP_TOKEN (the test app's xapp- token)}"
: "${PMOS_SLACK_BOT_TOKEN:?set PMOS_SLACK_BOT_TOKEN (the test app's xoxb- token)}"
: "${PMOS_TEST_SLACK_ID:?set PMOS_TEST_SLACK_ID (your member id in the test workspace)}"
if [ -n "${PMOS_TEST_MODEL_URL:-}" ]; then
  : "${PMOS_TEST_MODEL:?set PMOS_TEST_MODEL (the model name at PMOS_TEST_MODEL_URL)}"
  : "${PMOS_GATEWAY_REMOTE_KEY:?set PMOS_GATEWAY_REMOTE_KEY (the API key for PMOS_TEST_MODEL_URL)}"
  case "$PMOS_TEST_MODEL_URL" in
    https://*) ;;
    *) echo "error: PMOS_TEST_MODEL_URL must start with https://" >&2; exit 1 ;;
  esac
  MODEL_DESC="$PMOS_TEST_MODEL at $PMOS_TEST_MODEL_URL"
else
  : "${PMOS_GATEWAY_ANTHROPIC_KEY:?set PMOS_GATEWAY_ANTHROPIC_KEY (a Claude API key), or PMOS_TEST_MODEL_URL for another provider}"
  MODEL_DESC="Claude (config/gateway.claude-test.yaml)"
fi
HANDLE="${PMOS_TEST_HANDLE:-tester}"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
STATE="${PMOS_TEST_STATE:-state/claude-test}"
mkdir -p "$STATE/logs" "$STATE/slack"
STATE="$(cd "$STATE" && pwd)"
# The runtime keeps its notification ids in memory, so every start numbers them from 1 again. A
# cursor left by the previous run would then sit ahead of every new id, and the Slack service
# would skip each reply until the ids passed it. This script always runs the in-memory stores,
# so the cursor is dropped at each start.
rm -f "$STATE/slack/notifications.cursor"

echo "== building"
cargo build -q -p pmos -p pmos-docs -p pmos-toolserver -p pmos-gateway -p pmos-bots -p pmos-slack
(cd harness && pnpm -s install --frozen-lockfile >/dev/null && pnpm -s build)
if [ "${PMOS_TEST_WEB:-}" = 1 ]; then
  echo "== building the web app"
  cargo build -q -p pmos-web
  pnpm -s -C web build >/dev/null
fi
BIN="$ROOT/target/debug"

echo "== test content in $STATE/content (you as admin, plus your bot pm-$HANDLE)"
rm -rf "$STATE/content"
cp -r content "$STATE/content"
"$BIN/pmos" member add --root "$STATE/content" --handle "$HANDLE" \
  --slack-id "$PMOS_TEST_SLACK_ID" --roles pm,admin >/dev/null
"$BIN/pmos" lint --root "$STATE/content" | tail -1

if [ -n "${PMOS_TEST_MODEL_URL:-}" ]; then
  GATEWAY_CONFIG="$STATE/gateway.yaml"
  echo "== gateway config in $GATEWAY_CONFIG ($MODEL_DESC)"
  cat >"$GATEWAY_CONFIG" <<EOF
# TESTING ONLY, written by scripts/claude-test-stack.sh: every role on a remote
# OpenAI-compatible API, so prompts leave this machine. Fake data only.
upstreams:
  remote: "openai+${PMOS_TEST_MODEL_URL%/}"
roles:
  default: {upstream: remote, model: "$PMOS_TEST_MODEL"}
  reasoning: {upstream: remote, model: "$PMOS_TEST_MODEL"}
  extraction: {upstream: remote, model: "${PMOS_TEST_MODEL_FAST:-$PMOS_TEST_MODEL}"}
limits:
  max-in-flight: 4
  per-user: 2
  max-queue: 64
EOF
else
  GATEWAY_CONFIG=config/gateway.claude-test.yaml
fi

# Fresh tokens between the services for this run only.
tok() { openssl rand -hex 24; }
DOCS_TOKEN=$(tok) SERVICE_TOKEN=$(tok) BOT_TOKEN=$(tok)
GW_RUNTIME=$(tok) GW_WORKER=$(tok)
RT_SLACK=$(tok) RT_DOCS=$(tok) RT_PERSONAL=$(tok)
BOT_PERSONAL="pm-$HANDLE"
BOTS_TOKENS="slack=$RT_SLACK,worker-docs=$RT_DOCS,worker-$BOT_PERSONAL=$RT_PERSONAL"
# With the web app: its own tool server token, a user assertion key for each of its upstreams,
# and both receivers requiring assertions on its calls (docs/user-assertion.md).
ASSERTION_MODE=off TS_WEB="" AK_BOTS="" AK_TOOLS=""
if [ "${PMOS_TEST_WEB:-}" = 1 ]; then
  RT_WEB=$(tok)
  BOTS_TOKENS="$BOTS_TOKENS,web=$RT_WEB"
  TS_WEB=$(tok)
  AK_BOTS="web:bots:k1:$(openssl rand -base64 32)"
  AK_TOOLS="web:toolserver:k1:$(openssl rand -base64 32)"
  ASSERTION_MODE=require
fi

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
  PMOS_WEB_TOKEN="$TS_WEB" PMOS_ASSERTION_MODE="$ASSERTION_MODE" PMOS_ASSERTION_KEYS="$AK_TOOLS" \
  PMOS_CONTENT_ROOT="$STATE/content" PMOS_AUDIT_LOG="$STATE/audit.jsonl" \
  PMOS_DOCS_URL=http://127.0.0.1:8730/mcp PMOS_DOCS_TOKEN="$DOCS_TOKEN" \
  "$BIN/pmos-toolserver"
wait_for http://127.0.0.1:8710/healthz toolserver

start gateway env PMOS_GATEWAY_CLOUD_TEST=1 PMOS_GATEWAY_CONFIG="$GATEWAY_CONFIG" \
  PMOS_GATEWAY_TOKENS="runtime=$GW_RUNTIME,worker=$GW_WORKER" \
  PMOS_GATEWAY_USAGE_LOG="$STATE/usage.jsonl" \
  "$BIN/pmos-gateway"
wait_for http://127.0.0.1:8740/healthz gateway

start bots env PMOS_DEV=1 PMOS_CONTENT_ROOT="$STATE/content" \
  PMOS_BOTS_TOKENS="$BOTS_TOKENS" \
  PMOS_ASSERTION_MODE="$ASSERTION_MODE" PMOS_ASSERTION_KEYS="$AK_BOTS" \
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

if [ "${PMOS_TEST_WEB:-}" = 1 ]; then
  # Development sign-in: closed to this machine (127.0.0.1) and never for a shared network.
  start web env PMOS_WEB_BIND=127.0.0.1:8770 PMOS_WEB_DEV_LOGIN=1 \
    PMOS_WEB_BOTS_TOKEN="$RT_WEB" PMOS_WEB_TOOLSERVER_TOKEN="$TS_WEB" \
    PMOS_ASSERTION_KEYS="$AK_BOTS,$AK_TOOLS" \
    PMOS_BOTS_URL=http://127.0.0.1:8750 PMOS_TOOLSERVER_URL=http://127.0.0.1:8710 \
    PMOS_CONTENT_ROOT="$STATE/content" PMOS_WEB_STATIC_DIR="$ROOT/web/dist" \
    "$BIN/pmos-web"
  wait_for http://127.0.0.1:8770/healthz web
fi

echo
echo "== PM OS test stack is up, with $MODEL_DESC behind the gateway."
echo "   In the test Slack workspace, DM the PM OS app:"
echo "   docs: how do I get started?         (the Docs bot, your docs site)"
echo "   summarize the open launch issues    (your bot $BOT_PERSONAL, fake Jira)"
echo "   setup                               (the onboarding checklist)"
if [ "${PMOS_TEST_WEB:-}" = 1 ]; then
  echo "   Web app: http://127.0.0.1:8770 (sign in as $HANDLE; development sign-in, this machine only)"
fi
echo "   Logs: $STATE/logs; model calls (who, role, tokens; never text): $STATE/usage.jsonl."
echo "   Prompts go to the test model off this machine: fake data only. Ctrl-C stops everything."
wait -n "${PIDS[@]}" || true
echo "a service stopped; see $STATE/logs" >&2
