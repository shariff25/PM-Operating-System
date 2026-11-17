#!/usr/bin/env bash
# First-time setup of PM OS on the Spark (docs/runbooks/install.md). Safe to run again: it
# never replaces a value already in deploy/.env, and only fills in what's missing.
#
#   deploy/setup.sh [--content /srv/pmos/content-repo] [--clone <git url>] [--state /srv/pmos/state]
#
# It checks the tools, writes deploy/.env with fresh secrets, makes the state directories and,
# with --clone, clones the content repo. Then build and start with deploy/pmos-admin.
set -euo pipefail

DEPLOY="$(cd "$(dirname "$0")" && pwd)"
ENV="$DEPLOY/.env"
CONTENT=/srv/pmos/content-repo
STATE=/srv/pmos/state
CLONE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --content) CONTENT="$2"; shift 2 ;;
    --state) STATE="$2"; shift 2 ;;
    --clone) CLONE="$2"; shift 2 ;;
    -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done

ok() { printf '  \033[32mok\033[0m   %s\n' "$1"; }
warn() { printf '  \033[33mwarn\033[0m %s\n' "$1"; }
fail() { printf '  \033[31mfail\033[0m %s\n' "$1"; exit 1; }

echo "== checking tools"
command -v docker >/dev/null || fail "docker is not installed"
docker compose version >/dev/null 2>&1 || fail "docker compose (v2) is not installed"
ok "docker $(docker version --format '{{.Server.Version}}' 2>/dev/null || echo '(daemon not reachable)')"
command -v openssl >/dev/null || fail "openssl is not installed"
command -v git >/dev/null || fail "git is not installed"
ok "openssl and git"
if docker info 2>/dev/null | grep -qi nvidia; then
  ok "NVIDIA container runtime"
else
  warn "no NVIDIA container runtime found: the model servers (--models) won't start"
fi
if command -v openshell >/dev/null; then
  ok "openshell $(openshell --version 2>/dev/null | head -1)"
else
  warn "openshell is not installed: bots can run only in the bring-up worker until it is"
fi

echo "== content checkout"
if [ ! -d "$CONTENT/.git" ]; then
  [ -n "$CLONE" ] || fail "$CONTENT is not a git checkout; pass --clone <git url> or clone it first"
  mkdir -p "$(dirname "$CONTENT")"
  git clone "$CLONE" "$CONTENT"
fi
[ -f "$CONTENT/content/tools.yaml" ] || fail "$CONTENT has no content/tools.yaml; is it the PM OS content repo?"
ok "$CONTENT"
OWNER_UID=$(stat -c %u "$CONTENT")
OWNER_GID=$(stat -c %g "$CONTENT")

echo "== state directories"
mkdir -p "$STATE"/{audit,usage,slack} "$DEPLOY/backups"
chown -R "$OWNER_UID:$OWNER_GID" "$STATE" 2>/dev/null || warn "couldn't chown $STATE to $OWNER_UID:$OWNER_GID"
ok "$STATE (owned by $OWNER_UID:$OWNER_GID, the checkout's owner)"

echo "== deploy/.env"
[ -f "$ENV" ] || { cp "$DEPLOY/env.example" "$ENV"; ok "created from env.example"; }
chmod 600 "$ENV"
token() { openssl rand -hex 24; }
password() { openssl rand -hex 16; }
# Set KEY to VALUE when it is empty or still `generated`; never replace a real value.
fill() {
  local key=$1 value=$2 current
  current=$(grep -E "^$key=" "$ENV" | head -1 | cut -d= -f2- || true)
  if ! grep -qE "^$key=" "$ENV"; then
    printf '%s=%s\n' "$key" "$value" >>"$ENV"
  elif [ -z "$current" ] || [ "$current" = generated ]; then
    local tmp
    tmp=$(mktemp)
    awk -v k="$key" -v v="$value" -F= 'BEGIN{OFS="="} $1==k{print k, v; next} {print}' "$ENV" >"$tmp"
    cat "$tmp" >"$ENV"
    rm -f "$tmp"
  fi
}
fill PMOS_CONTENT_CHECKOUT "$CONTENT"
fill PMOS_STATE_DIR "$STATE"
fill PMOS_UID "$OWNER_UID"
fill PMOS_GID "$OWNER_GID"
for k in PMOS_POSTGRES_PASSWORD PMOS_DB_TOOLSERVER_PASSWORD PMOS_DB_BOTS_PASSWORD PMOS_DB_DOCS_PASSWORD; do
  fill "$k" "$(password)"
done
for k in PMOS_SERVICE_TOKEN PMOS_BOT_TOKEN PMOS_DOCS_TOKEN PMOS_GW_RUNTIME_TOKEN PMOS_GW_EVALS_TOKEN \
  PMOS_GW_DOCS_BOT_TOKEN PMOS_RT_SLACK_TOKEN PMOS_RT_CONSOLE_TOKEN PMOS_RT_WEB_TOKEN \
  PMOS_RT_EVALS_TOKEN PMOS_RT_DOCS_WORKER_TOKEN PMOS_CONSOLE_PROXY_SECRET \
  PMOS_TS_WEB_TOKEN PMOS_CONSOLE_TS_TOKEN PMOS_WEB_BRIEF_TOKEN; do
  fill "$k" "$(token)"
done
# One user assertion key per caller and receiver (docs/user-assertion.md): `<key id>:<base64>`.
for k in PMOS_AK_WEB_BOTS PMOS_AK_WEB_TOOLSERVER PMOS_AK_CONSOLE_BOTS PMOS_AK_CONSOLE_TOOLSERVER; do
  fill "$k" "k$(date +%Y%m%d):$(openssl rand -base64 32)"
done
fill PMOS_VAULT_KEY "k1:$(openssl rand -base64 32)"
ok "secrets filled in (existing values kept)"
# Kept in deploy/.env, so every later `pmos-admin up`, `add-bot` and `restore` keeps it: the
# runtime and the tool server refuse to start in another mode while the web app's or the
# console's tokens are set (docs/user-assertion.md, Modes).
fill PMOS_ASSERTION_MODE require
fill PMOS_ONBOARDING_V2 1
fill PMOS_NUDGES 1
mode=$(grep -E "^PMOS_ASSERTION_MODE=" "$ENV" | head -1 | cut -d= -f2-)
if [ "$mode" = require ]; then
  ok "user assertions: require"
else
  warn "PMOS_ASSERTION_MODE=$mode in deploy/.env: the runtime and the tool server will refuse to start with it unless PMOS_ASSERTION_INSECURE_OFF=1 (development only)"
fi

missing=()
for k in PMOS_SLACK_BOT_TOKEN PMOS_SLACK_APP_TOKEN; do
  grep -qE "^$k=.+" "$ENV" || missing+=("$k")
done
if [ ${#missing[@]} -gt 0 ]; then
  warn "not set yet: ${missing[*]} (the Slack bot stays off until they are)"
fi
if ! grep -qE "^PMOS_DOCS_BASE_URL=.+" "$ENV"; then
  warn "PMOS_DOCS_BASE_URL is not set: the Docs bot has no docs site until it is (optional)"
fi

echo
echo "Next:"
echo "  1. Keep a copy of deploy/.env in the team password manager (it holds the vault key)."
echo "  2. deploy/pmos-admin build     # builds the images on this machine"
echo "  3. deploy/pmos-admin up        # starts the stack; then deploy/pmos-admin status"
