#!/usr/bin/env bash
# TESTING ONLY: give one benchmark lane of the test stack a fresh tool server (docs/evals.md,
# "Isolated attempts"). scripts/claude-test-stack.sh with PMOS_TEST_EVALS=1 runs it to start each
# lane, and pmos-evals runs it (through $STATE/fresh-toolserver, PMOS_EVALS_FRESH_CMD) before
# every attempt, so no attempt sees a draft, memory or record an earlier one left.
#
#   scripts/eval-fresh-toolserver.sh <state dir> <lane>
#
# It stops the lane's tool server (only a process it started, running the lane's tool server
# binary), starts a new one with the same settings ($STATE/lanes/toolserver.env, mode 0600,
# written by the stack script: in-memory stores, fake connectors, the stack's seed and content),
# and exits 0 once the new one answers /healthz. Lane n's tool server listens on
# 127.0.0.1:(8710 + 100 n). Prints no token: the settings go to the tool server through its
# environment only.
#
# The settings file is read as data, never run: one KEY=VALUE per line (the value is the rest
# of the line, as written), `#` comments and blank lines skipped, and only the keys
# LANE_TOOLSERVER_BIN, NO_PROXY and PMOS_* taken; anything else stops it. The tool server starts
# from an empty environment with those settings and an allow-list of what it inherits (PATH,
# HOME, TMPDIR, the locale, and the proxy and CA variables), as the stack's services do.
set -euo pipefail

STATE=${1:?usage: eval-fresh-toolserver.sh <state dir> <lane>}
LANE=${2:?usage: eval-fresh-toolserver.sh <state dir> <lane>}
case "$LANE" in
  [1-8]) ;;
  *) echo "eval-fresh-toolserver: lane must be 1 to 8, not '$LANE'" >&2; exit 2 ;;
esac
SETTINGS="$STATE/lanes/toolserver.env"
[ -r "$SETTINGS" ] || {
  echo "eval-fresh-toolserver: no $SETTINGS; start scripts/claude-test-stack.sh with PMOS_TEST_EVALS=1 and PMOS_TEST_EVALS_LANES" >&2
  exit 2
}
DIR="$STATE/lanes/$LANE"
mkdir -p "$DIR"
PORT=$((8710 + 100 * LANE))
PIDFILE="$DIR/toolserver.pid"

# The settings, as KEY=VALUE data.
SET_ENV=() LANE_TOOLSERVER_BIN=""
n=0
while IFS= read -r line || [ -n "$line" ]; do
  n=$((n + 1))
  case "$line" in '' | '#'*) continue ;; esac
  key=${line%%=*}
  if [ "$key" = "$line" ] || ! [[ "$key" =~ ^[A-Z][A-Z0-9_]*$ ]]; then
    echo "eval-fresh-toolserver: $SETTINGS line $n isn't KEY=VALUE" >&2
    exit 2
  fi
  value=${line#*=}
  case "$key" in
    LANE_TOOLSERVER_BIN) LANE_TOOLSERVER_BIN=$value ;;
    NO_PROXY | PMOS_*) SET_ENV+=("$key=$value") ;;
    *)
      echo "eval-fresh-toolserver: $SETTINGS line $n sets $key, which a lane's tool server doesn't take" >&2
      exit 2
      ;;
  esac
done <"$SETTINGS"
case "$LANE_TOOLSERVER_BIN" in
  /*) [ -x "$LANE_TOOLSERVER_BIN" ] || {
    echo "eval-fresh-toolserver: LANE_TOOLSERVER_BIN in $SETTINGS isn't an executable" >&2
    exit 2
  } ;;
  *) echo "eval-fresh-toolserver: $SETTINGS has no absolute LANE_TOOLSERVER_BIN" >&2; exit 2 ;;
esac
BIN_REAL=$(readlink -f "$LANE_TOOLSERVER_BIN")

# What the tool server inherits: nothing but these (a database URL, vault key, address or
# connector setting exported in the caller's shell never reaches it).
KEEP_ENV=()
for v in PATH HOME USER LOGNAME TMPDIR TMP TEMP LANG LANGUAGE TZ \
  HTTPS_PROXY HTTP_PROXY ALL_PROXY https_proxy http_proxy all_proxy no_proxy \
  SSL_CERT_FILE SSL_CERT_DIR CURL_CA_BUNDLE REQUESTS_CA_BUNDLE RUST_LOG RUST_BACKTRACE; do
  [ -n "${!v+x}" ] && KEEP_ENV+=("$v=${!v}")
done
for v in $(compgen -e); do
  case "$v" in LC_*) KEEP_ENV+=("$v=${!v}") ;; esac
done

is_ours() { # pid: a live process running the lane's tool server binary, by command line and exe
  [ -r "/proc/$1/cmdline" ] && tr '\0' ' ' <"/proc/$1/cmdline" | grep -q 'pmos-toolserver' || return 1
  local exe
  exe=$(readlink "/proc/$1/exe" 2>/dev/null) || return 1
  # A binary rebuilt since it started shows as "<path> (deleted)": still the same path.
  exe=${exe% (deleted)}
  [ "$exe" = "$BIN_REAL" ]
}

# Stop the lane's current tool server, and wait until it has gone (its memory with it).
if [ -f "$PIDFILE" ]; then
  old=$(cat "$PIDFILE")
  if [[ "$old" =~ ^[0-9]+$ ]] && is_ours "$old"; then
    kill "$old" 2>/dev/null || true
    for _ in $(seq 1 100); do
      is_ours "$old" || break
      sleep 0.1
    done
    if is_ours "$old"; then
      kill -9 "$old" 2>/dev/null || true
      sleep 0.2
    fi
  fi
  rm -f "$PIDFILE"
fi
if curl -fsS --noproxy '*' -m 2 "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1; then
  echo "eval-fresh-toolserver: something else answers on 127.0.0.1:$PORT; lane $LANE can't start" >&2
  exit 1
fi

# Start a new one, detached from the caller (its output goes to the lane's log, never to the
# caller), and wait for it.
# The settings (tokens among them) are exported by this subshell, never passed to `env` on a
# command line, where every account on this machine could read them in /proc/<pid>/cmdline
# (the security review of 6bb806f, S1): the subshell clears its environment, exports them and
# execs setsid.
(
  for v in $(compgen -e); do unset "$v" 2>/dev/null || true; done
  for kv in "${KEEP_ENV[@]}" "${SET_ENV[@]}" PMOS_BIND="127.0.0.1:$PORT" PMOS_AUDIT_LOG="$DIR/audit.jsonl"; do
    export "$kv"
  done
  exec setsid "$LANE_TOOLSERVER_BIN" >>"$DIR/toolserver.log" 2>&1 </dev/null
) &
new=$!
echo "$new" >"$PIDFILE"
for _ in $(seq 1 300); do
  if ! kill -0 "$new" 2>/dev/null; then
    echo "eval-fresh-toolserver: lane $LANE's tool server exited at start; see $DIR/toolserver.log" >&2
    exit 1
  fi
  if curl -fsS --noproxy '*' -m 2 "http://127.0.0.1:$PORT/healthz" >/dev/null 2>&1; then
    exit 0
  fi
  sleep 0.1
done
echo "eval-fresh-toolserver: lane $LANE's tool server didn't answer in 30 s; see $DIR/toolserver.log" >&2
exit 1
