#!/usr/bin/env bash
# TESTING ONLY: give one benchmark lane of the test stack a fresh tool server (docs/evals.md,
# "Isolated attempts"). scripts/claude-test-stack.sh with PMOS_TEST_EVALS=1 runs it to start each
# lane, and pmos-evals runs it (through $STATE/fresh-toolserver, PMOS_EVALS_FRESH_CMD) before
# every attempt, so no attempt sees a draft, memory or record an earlier one left.
#
#   scripts/eval-fresh-toolserver.sh <state dir> <lane>
#
# It stops the lane's tool server (only a pmos-toolserver process it started), starts a new one
# with the same settings ($STATE/lanes/toolserver.env, mode 0600, written by the stack script:
# in-memory stores, fake connectors, the stack's seed and content), and exits 0 once the new
# one answers /healthz. Lane n's tool server listens on 127.0.0.1:(8710 + 100 n). Prints no
# token: the settings go to the tool server through its environment only.
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

is_ours() { # pid: a live pmos-toolserver process
  [ -r "/proc/$1/cmdline" ] && tr '\0' ' ' <"/proc/$1/cmdline" | grep -q 'pmos-toolserver'
}

# Stop the lane's current tool server, and wait until it has gone (its memory with it).
if [ -f "$PIDFILE" ]; then
  old=$(cat "$PIDFILE")
  if [ -n "$old" ] && is_ours "$old"; then
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
(
  set -a
  # shellcheck disable=SC1090
  . "$SETTINGS"
  set +a
  export PMOS_BIND="127.0.0.1:$PORT" PMOS_AUDIT_LOG="$DIR/audit.jsonl"
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
