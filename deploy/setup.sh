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

# The two paths go into deploy/.env, which pmos-admin reads as data and Docker Compose
# interpolates: only an absolute path of plain characters is taken (no space, `$`, quote, `#`,
# `%` or newline). The clone URL goes to git after `--`, and may not look like an option.
PATH_CHARS='ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_./+-'
for p in "$CONTENT" "$STATE"; do
  case "$p" in
    /*[!$PATH_CHARS]* | [!/]* | '')
      echo "$(printf '%q' "$p") must be an absolute path of letters, digits and _ . / + -" >&2
      exit 2
      ;;
  esac
done
case "$CLONE" in -*) echo "--clone takes a git URL, not $(printf '%q' "$CLONE")" >&2; exit 2 ;; esac
# A URL with a password or token in it (https://user:<token>@host/…) would be on git's command
# line, where every account on this machine can read it, and in the checkout's .git/config. Use
# the content repo's read-only deploy key (an ssh URL) or a git credential helper instead.
if [[ "$CLONE" =~ ^[A-Za-z][A-Za-z0-9+.-]*://[^/@]*@ ]]; then
  echo "--clone: don't put a user, password or token in the URL; use an ssh deploy key (git@host:org/repo.git) or a git credential helper" >&2
  exit 2
fi

# What is removed however the script stops (an error, Ctrl-C, a kill; the review of eb49991): a
# new deploy/.env not yet renamed into place (fill). A signal becomes an exit,
# so the EXIT trap runs for it too.
ENV_TMP=""
cleanup() {
  [ -z "$ENV_TMP" ] || rm -f -- "$ENV_TMP"
  return 0
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

ok() { printf '  \033[32mok\033[0m   %s\n' "$1"; }
warn() { printf '  \033[33mwarn\033[0m %s\n' "$1"; }
fail() { printf '  \033[31mfail\033[0m %s\n' "$1"; exit 1; }

# The host preflight first (docs/openshell-any-host-spec.md §3.2, slice H1): every host check,
# before anything is installed or written; H-NAMES and H-METADATA can't be set yet and pass. A
# fail stops here, with nothing changed. It reads deploy/.env when one exists (a second run).
echo "== host preflight (deploy/preflight.sh --stage install)"
pf_code=0
"$DEPLOY/preflight.sh" --stage install --env-file "$ENV" || pf_code=$?
if [ "$pf_code" != 0 ]; then
  echo "setup.sh stops: the host preflight failed (each fail above). Nothing was changed." >&2
  exit 1
fi

echo "== checking tools"
command -v docker >/dev/null || fail "docker is not installed"
docker compose version >/dev/null 2>&1 || fail "docker compose (v2) is not installed"
ok "docker $(docker version --format '{{.Server.Version}}' 2>/dev/null || echo '(daemon not reachable)')"
command -v openssl >/dev/null || fail "openssl is not installed"
command -v git >/dev/null || fail "git is not installed"
command -v flock >/dev/null || fail "flock is not installed (util-linux: sudo apt-get install util-linux); deploy/.env is changed only under its lock"
ok "openssl, git and flock"
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
  git clone -- "$CLONE" "$CONTENT"
fi
[ -f "$CONTENT/content/tools.yaml" ] || fail "$CONTENT has no content/tools.yaml; is it the PM OS content repo?"
ok "$CONTENT"
OWNER_UID=$(stat -c %u "$CONTENT")
OWNER_GID=$(stat -c %g "$CONTENT")
# The services and the bots' bring-up and eval workers run as the checkout's owner (PMOS_UID and
# PMOS_GID): a root-owned checkout would make them root (the review of eb49991, T3).
if [ "$OWNER_UID" = 0 ] || [ "$OWNER_GID" = 0 ]; then
  fail "$CONTENT is owned by root (uid $OWNER_UID, gid $OWNER_GID), and the services and the bots' workers run as the checkout's owner, so they would run as root. Give it to a service account, then run this again: sudo useradd --system --no-create-home --shell /usr/sbin/nologin pmos; sudo chown -R pmos:pmos $CONTENT"
fi

echo "== state directories"
mkdir -p "$STATE"/{audit,usage,slack} "$DEPLOY/backups"
chown -R "$OWNER_UID:$OWNER_GID" "$STATE" 2>/dev/null || warn "couldn't chown $STATE to $OWNER_UID:$OWNER_GID"
ok "$STATE (owned by $OWNER_UID:$OWNER_GID, the checkout's owner)"

echo "== deploy/.env"
# Under the same lock as pmos-admin (deploy/.env.lock with flock; where flock isn't installed,
# nothing is changed), so the two never change it at once.
if command -v flock >/dev/null; then
  # Read-only where it exists (a lock file made by root must not stop another account's run).
  if [ ! -e "$DEPLOY/.env.lock" ]; then
    (umask 077 && : >>"$DEPLOY/.env.lock") 2>/dev/null || fail "couldn't create $DEPLOY/.env.lock (the lock for deploy/.env): check who owns deploy/"
  fi
  # In braces, so only this exec's error is silenced, not every later one.
  { exec 9<"$DEPLOY/.env.lock"; } 2>/dev/null || fail "couldn't open $DEPLOY/.env.lock for reading (the lock for deploy/.env): check its owner and mode"
  flock -w 60 9 || fail "another setup.sh or pmos-admin has been changing deploy/.env for a minute: try again"
else
  fail "flock (util-linux) isn't installed: install it and run this again. deploy/.env is changed only under its lock."
fi
# Made private before it holds anything (umask), then kept so.
[ -f "$ENV" ] || { (umask 077 && cp "$DEPLOY/env.example" "$ENV"); ok "created from env.example"; }
chmod 600 "$ENV"
token() { openssl rand -hex 24; }
password() { openssl rand -hex 16; }
# Put the file $1 (a new deploy/.env, in the same directory) in place of deploy/.env: mode 0600,
# the old file's owner and ACL (the eval runner's, docs/runbooks/upgrade.md), then renamed over
# it, so no reader ever sees half a file (as pmos-admin's replace_env).
replace_env() {
  chmod 600 "$1"
  chown --reference="$ENV" "$1" 2>/dev/null || true
  if command -v getfacl >/dev/null && command -v setfacl >/dev/null; then
    getfacl -p -- "$ENV" 2>/dev/null | setfacl --set-file=- -- "$1" 2>/dev/null ||
      warn "couldn't copy deploy/.env's ACL; if the eval runner reads it: sudo setfacl -m u:gh-runner:r $ENV"
  elif [[ "$(ls -ld -- "$ENV")" == ??????????+* ]]; then
    warn "deploy/.env had an ACL and getfacl/setfacl aren't installed to keep it: sudo setfacl -m u:gh-runner:r $ENV"
  fi
  mv -f -- "$1" "$ENV"
}
# Set KEY to VALUE when it is empty or still `generated`; never replace a real value.
# Every value it writes is a path checked above, a number, hex or base64: anything else is a bug,
# refused before the file is touched.
fill() {
  local key=$1 value=$2 current tmp
  case "$value" in
    *[!ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_.,:=/+-]*)
      echo "refusing to write $key with characters deploy/.env can't hold" >&2
      exit 2
      ;;
  esac
  current=$(grep -E "^$key=" "$ENV" | head -1 | cut -d= -f2- || true)
  if grep -qE "^$key=" "$ENV" && [ -n "$current" ] && [ "$current" != generated ]; then
    return 0
  fi
  tmp=$(mktemp "$DEPLOY/.env.XXXXXX")
  ENV_TMP=$tmp
  # The value (the vault key, a password, a token) reaches awk through its environment, never
  # its command line (`-v`), where every account on this machine can read it in
  # /proc/<pid>/cmdline while awk runs (the review of eb49991, T1); printf is a builtin.
  if ! {
    if ! grep -qE "^$key=" "$ENV"; then
      cat "$ENV" >"$tmp" && printf '%s=%s\n' "$key" "$value" >>"$tmp"
    else
      FILL_VALUE=$value awk -v k="$key" -F= 'BEGIN{OFS="="} $1==k{print k, ENVIRON["FILL_VALUE"]; next} {print}' "$ENV" >"$tmp"
    fi
  }; then
    fail "couldn't write deploy/.env; it is unchanged" # cleanup removes the new file
  fi
  replace_env "$tmp"
  ENV_TMP=""
}
fill PMOS_CONTENT_CHECKOUT "$CONTENT"
fill PMOS_STATE_DIR "$STATE"
fill PMOS_UID "$OWNER_UID"
fill PMOS_GID "$OWNER_GID"
# A value kept from an earlier run (an older setup.sh wrote 0 for a root-owned checkout): only a
# number other than 0 is taken (pmos-admin refuses anything else too).
for k in PMOS_UID PMOS_GID; do
  v=$(grep -E "^$k=" "$ENV" | head -1 | cut -d= -f2- || true)
  [[ "$v" =~ ^\"(.*)\"$ || "$v" =~ ^\'(.*)\'$ ]] && v=${BASH_REMATCH[1]}
  case "$v" in
    '' | *[!0123456789]*) fail "$k=$v in deploy/.env isn't a number: set it to the checkout's owner ($OWNER_UID:$OWNER_GID)" ;;
  esac
  [ ${#v} -le 10 ] && [ $((10#$v)) != 0 ] ||
    fail "$k=$v in deploy/.env: the services and the bots' workers would run as root (or root's group). Set PMOS_UID=$OWNER_UID and PMOS_GID=$OWNER_GID (the checkout's owner) in deploy/.env, then run this again"
done
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
  warn "PMOS_ASSERTION_MODE=$mode in deploy/.env: with the web app or the console configured, the runtime and the tool server refuse to start with it; set it to require"
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
