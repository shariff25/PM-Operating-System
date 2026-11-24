#!/usr/bin/env bash
# PM OS red team: the stack's own steps, run on PM OS's host outside any sandbox (slice H7,
# docs/openshell-any-host-spec.md §5, "Red-team checklist additions"): RT-16 (the metadata block
# survives a Docker restart and a flush of DOCKER-USER), RT-19 (the gateway refuses a changed
# hosted setting), RT-20 (the hosted kill switch holds through every restart path) and RT-21 (a
# source change under an open session). One step per run.
#
# Run it on the host, never on a machine shared with other work, from the deploy kit's checkout
# (it reads deploy/.env as data, never as shell). Use a test stack with fake data.
#
#   policies/redteam/stack.sh [--dry-run] [--allow-host-changes] rt16|rt19|rt20|rt21
#
#   --dry-run             print every probe and every change, run and change nothing
#   --allow-host-changes  let the step change the host as the spec's step says (below); without
#                         it the changing parts are skipped and the run is not a pass
#
# rt16  RT-16. Without changes: H-METADATA's probe (deploy/preflight.sh --only H-METADATA) and a
#       plain TCP connect to 169.254.169.254:80, each from a throwaway container on the pmos
#       network (the image the preflight's probe uses; `docker run --rm`). With the block applied
#       (pmos-admin host-firewall check), no answer at all is expected. With
#       --allow-host-changes and sudo: `systemctl restart docker`, a wait for the stack's
#       containers to come back by themselves (no pmos-admin up), the probes again; then a flush
#       of DOCKER-USER (refused when the chain holds any rule but the block's and Docker's
#       `-j RETURN`), 70 s for the block's timer, the probes again. A trap runs
#       `sudo deploy/pmos-admin host-firewall apply` (and puts `-j RETURN` back) on every exit
#       after the flush, whatever happened.
# rt19  RT-19, only once an approved hosted entry exists (slices H12 and H13); "not built" until
#       then. Each changed setting goes into a temporary copy of deploy/.env (mode 600, in a
#       private directory, removed on exit), never into deploy/.env itself, for
#       `docker compose up -d gateway`; a trap brings the gateway back on deploy/.env. Emptying
#       the key file keeps a copy beside it (mode 600) that the trap puts back.
# rt20  RT-20, on the stack-test-list gateway (slice H12; "not built" until then):
#       `sudo pmos-admin hosted stop`, then `docker start`, `docker compose up -d gateway` and
#       `systemctl restart docker` in turn, each leaving the gateway refusing; removing the stop
#       file without sudo must fail; then `sudo pmos-admin hosted start`. If the script stops
#       early the hosted model stays stopped (the safe state); it never starts it from a trap.
# rt21  RT-21. Changes nothing itself: you restart the gateway on the other source (hosted-test ↔
#       local with the stub) while a session is open, and the script watches the gateway's
#       /readyz and the gateway's and tool server's logs (`docker logs`, read only; it prints
#       timestamps and counts, never a log line) and writes the measured window. Refused unless
#       the gateway serves hosted-test or the stub when it starts.
#
# Exit status (lib.sh): 0 all checks passed, 1 a check failed, 2 the script couldn't run,
# 3 nothing failed but a check was skipped (not a pass). A dry run exits 0.

set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=policies/redteam/lib.sh
. "$HERE/lib.sh"

DEPLOY="$REPO/deploy"
PMOS_ADMIN="$DEPLOY/pmos-admin"
PREFLIGHT="$DEPLOY/preflight.sh"
BLOCK_SCRIPT=/usr/local/sbin/pmos-metadata-block
STOP_FILE=/var/lib/pmos/hosted-control/stopped
NETWORK_NAME=pmos_pmos
RT16_WAIT=${RT16_WAIT:-180}
RT21_WAIT=${RT21_WAIT:-600}

usage() {
  echo "usage: $0 [--dry-run] [--allow-host-changes] rt16|rt19|rt20|rt21" >&2
  exit 2
}
STEP=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY=1; shift ;;
    --allow-host-changes) ALLOW_CHANGES=1; shift ;;
    rt16 | rt19 | rt20 | rt21) [ -z "$STEP" ] || usage; STEP=$1; shift ;;
    *) usage ;;
  esac
done
[ -n "$STEP" ] || usage
if [ "$DRY" = 0 ]; then
  for t in docker curl jq; do command -v "$t" >/dev/null || { echo "missing $t" >&2; exit 2; }; done
  [ -r "$ENV_FILE" ] || { echo "deploy/.env isn't there or can't be read: run this on the host, from the deploy kit's checkout" >&2; exit 2; }
fi

# --- putting the host back ------------------------------------------------------------------

FLUSHED=0 HAD_RETURN4=0 HAD_RETURN6=0 BLOCK_APPLIED=0
GW_ENV_CHANGED=0 KEY_BACKUP="" HOSTED_STOPPED=0 GW_EXTRA="" RT19_SECRETS=""
SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/pmos-redteam.XXXXXX") || exit 2
chmod 700 "$SCRATCH"
# shellcheck disable=SC2329 # run by the EXIT trap
on_exit() {
  local code=$?
  if [ "$FLUSHED" = 1 ]; then
    echo "putting the metadata block back: sudo deploy/pmos-admin host-firewall apply" >&2
    restore_block || code=1
  fi
  if [ -n "$KEY_BACKUP" ]; then
    echo "putting the hosted key file back" >&2
    restore_key || code=1
  fi
  if [ "$GW_ENV_CHANGED" = 1 ]; then
    echo "bringing the gateway back on deploy/.env: docker compose up -d gateway" >&2
    gw_compose "$ENV_FILE" up -d gateway >/dev/null 2>&1 || { echo "COULDN'T restart the gateway on deploy/.env: run deploy/pmos-admin up" >&2; code=1; }
  fi
  if [ "$HOSTED_STOPPED" = 1 ]; then
    echo "the hosted model is still stopped (the safe state). When you are done: sudo deploy/pmos-admin hosted start --reason test" >&2
  fi
  rm -rf "$SCRATCH"
  exit "$code"
}
trap on_exit EXIT
trap 'exit 130' INT TERM HUP

echo "PM OS stack red-team step $STEP ($(date -u +%Y-%m-%dT%H:%M:%SZ))"
[ "$DRY" = 0 ] || echo "DRY RUN: the probes and changes below are printed, not run"
commit_line

# --- RT-16 ----------------------------------------------------------------------------------

IMAGE=""
metadata_checks() { # when
  local out st code
  if [ "$DRY" = 1 ]; then
    dry RT-16 "$1: $PREFLIGHT --env-file deploy/.env --only H-METADATA --json: expect pass"
    dry RT-16 "$1: docker run --rm --network $NETWORK_NAME $IMAGE: curl --connect-timeout 5 http://169.254.169.254:80/: expect no connection with the block applied"
    return
  fi
  out=$("$PREFLIGHT" --env-file "$ENV_FILE" --only H-METADATA --json 2>/dev/null)
  st=$(jq -r '.checks[] | select(.id == "H-METADATA") | .status' <<<"$out" 2>/dev/null)
  if [ "$st" = pass ]; then
    pass RT-16 "$1: H-METADATA passes ($(jq -r '.checks[] | select(.id == "H-METADATA") | .measured' <<<"$out"))"
  else
    fail RT-16 "$1: H-METADATA is '${st:-not measured}' ($(jq -r '.checks[] | select(.id == "H-METADATA") | .measured' <<<"$out" 2>/dev/null))"
  fi
  # curl's exit status only: 7 couldn't connect, 28 timed out. Nothing it gets is kept.
  code=$(timeout 30 docker run --rm --network "$NETWORK_NAME" --entrypoint sh "$IMAGE" -c \
    'curl -s -o /dev/null --connect-timeout 5 -m 6 http://169.254.169.254:80/; echo $?' 2>/dev/null | tail -1)
  if [ "$BLOCK_APPLIED" = 1 ]; then
    case "$code" in
      7 | 28) pass RT-16 "$1: a TCP connect to 169.254.169.254:80 from the pmos network gets no answer (curl $code)" ;;
      '') fail RT-16 "$1: the TCP probe container didn't run (inconclusive)" ;;
      *) fail RT-16 "$1: a TCP connect to 169.254.169.254:80 from the pmos network got an answer (curl $code), with the block applied" ;;
    esac
  else
    skip RT-16 "$1: the block isn't applied (pmos-admin host-firewall check), so 'no answer at all' isn't expected; the TCP probe gave curl ${code:-nothing}"
  fi
}
stack_containers() {
  docker ps --filter label=com.docker.compose.project=pmos --format '{{.Names}}' 2>/dev/null | sort
}
# Every rule in a DOCKER-USER chain is the block's or Docker's own `-j RETURN`.
chain_is_ours() { # iptables|ip6tables
  sudo "$1" -w -S DOCKER-USER 2>/dev/null |
    awk '$0 == "-N DOCKER-USER" || $0 == "-A DOCKER-USER -j RETURN" || /--comment "?pmos-metadata-block"? / { next } { bad = 1 } END { exit bad }'
}
has_return() { sudo "$1" -w -C DOCKER-USER -j RETURN 2>/dev/null; }
have6() { command -v ip6tables >/dev/null && sudo ip6tables -w -S DOCKER-USER >/dev/null 2>&1; }
restore_block() {
  local ok=0
  sudo "$PMOS_ADMIN" host-firewall apply >/dev/null || ok=1
  if [ "$HAD_RETURN4" = 1 ] && ! has_return iptables; then sudo iptables -w -A DOCKER-USER -j RETURN || ok=1; fi
  if [ "$HAD_RETURN6" = 1 ] && ! has_return ip6tables; then sudo ip6tables -w -A DOCKER-USER -j RETURN || ok=1; fi
  [ "$ok" = 0 ] && FLUSHED=0
  return $ok
}

step_rt16() {
  local before after i missing
  BLOCK_APPLIED=0
  if [ "$DRY" = 1 ]; then
    IMAGE="pmos:<PMOS_VERSION or local>"
    dry RT-16 "would check: $PMOS_ADMIN host-firewall check (is the block applied?)"
    BLOCK_APPLIED=1
  else
    IMAGE="pmos:$(env_value PMOS_VERSION || true)"
    [ "$IMAGE" != "pmos:" ] || IMAGE=pmos:local
    docker image inspect "$IMAGE" >/dev/null 2>&1 || { echo "the image $IMAGE isn't there: deploy/pmos-admin build" >&2; exit 2; }
    docker network inspect "$NETWORK_NAME" >/dev/null 2>&1 || { echo "the network $NETWORK_NAME isn't there: the stack isn't up" >&2; exit 2; }
    "$PMOS_ADMIN" host-firewall check >/dev/null 2>&1 && BLOCK_APPLIED=1
    [ "$BLOCK_APPLIED" = 1 ] || note "the metadata block isn't applied on this host"
  fi
  metadata_checks "before"
  if [ "$BLOCK_APPLIED" = 0 ]; then
    skip RT-16 "the Docker restart and the flush test the block, which isn't applied: not run"
    return
  fi

  # The Docker restart.
  if may_change RT-16 "sudo systemctl restart docker: every container on the host restarts"; then
    if [ "$DRY" = 1 ]; then
      dry RT-16 "would run: sudo systemctl restart docker, then wait up to ${RT16_WAIT}s for the same containers to run again without pmos-admin up"
    else
      before=$(stack_containers)
      sudo systemctl restart docker || { fail RT-16 "sudo systemctl restart docker failed"; return; }
      after=""
      for ((i = 0; i < RT16_WAIT; i += 5)); do
        sleep 5
        after=$(stack_containers)
        [ "$after" != "$before" ] || break
      done
      if [ "$after" = "$before" ]; then
        pass RT-16 "after the Docker restart the stack's $(wc -l <<<"$before") containers came back by themselves"
      else
        missing=$(comm -23 <(printf '%s\n' "$before") <(printf '%s\n' "$after") | tr '\n' ' ')
        fail RT-16 "after the Docker restart, not back within ${RT16_WAIT}s: ${missing:-?}"
      fi
    fi
    metadata_checks "after the Docker restart"
  fi

  # The flush.
  if may_change RT-16 "sudo iptables -F DOCKER-USER (and ip6tables): the block is gone until its timer puts it back; a trap reapplies it"; then
    if [ "$DRY" = 1 ]; then
      dry RT-16 "would refuse if DOCKER-USER holds rules other than the block's and -j RETURN"
      dry RT-16 "would run: sudo iptables -w -F DOCKER-USER; sudo ip6tables -w -F DOCKER-USER (if it has the chain); wait 70 s"
      dry RT-16 "would check: sudo $BLOCK_SCRIPT check (the timer put the rules back)"
    else
      if ! chain_is_ours iptables || { have6 && ! chain_is_ours ip6tables; }; then
        skip RT-16 "DOCKER-USER holds rules that aren't the block's: not flushed (put them in the record and flush by hand if you may)"
        return
      fi
      has_return iptables && HAD_RETURN4=1
      have6 && has_return ip6tables && HAD_RETURN6=1
      FLUSHED=1
      sudo iptables -w -F DOCKER-USER || { fail RT-16 "couldn't flush DOCKER-USER"; return; }
      if have6; then sudo ip6tables -w -F DOCKER-USER || fail RT-16 "couldn't flush ip6tables' DOCKER-USER"; fi
      note "flushed DOCKER-USER; waiting 70 s for pmos-metadata-block.timer"
      sleep 70
      if sudo "$BLOCK_SCRIPT" check >/dev/null 2>&1; then
        pass RT-16 "70 s after the flush, the timer had put every block rule back"
      else
        fail RT-16 "70 s after the flush, a block rule is still missing (sudo $BLOCK_SCRIPT check)"
      fi
    fi
    metadata_checks "70 s after the flush"
    if [ "$DRY" = 0 ]; then restore_block || fail RT-16 "couldn't reapply the block: run sudo deploy/pmos-admin host-firewall apply"; fi
  fi
}

# --- RT-19 (slices H12 and H13) ---------------------------------------------------------------

# The kit's gateway, by hand (no pmos-admin), on an env file: under hosted with the hosted overlay
# (deploy/compose.hosted.yaml, slice H12), as pmos-admin adds it.
gw_compose() { # env-file compose-arguments…
  local env=$1 hosted=()
  shift
  if grep -qx 'PMOS_MODEL_SOURCE=hosted' "$env" 2>/dev/null && [ -f "$DEPLOY/compose.hosted.yaml" ]; then
    hosted=(-f "$DEPLOY/compose.hosted.yaml")
  fi
  docker compose --project-directory "$DEPLOY" -f "$DEPLOY/compose.yaml" "${hosted[@]}" ${GW_EXTRA:+-f "$GW_EXTRA"} --env-file "$env" "$@"
}
# A one-token model call as the runtime, naming the source the gateway serves (X4) and a run
# (slice H12), so a refusal comes from the condition probed, never from a missing header.
gw_call() { # run-id; prints the status, the body in $SCRATCH/answer
  printf 'header = "Authorization: Bearer %s"\n' "$(env_value PMOS_GW_RUNTIME_TOKEN || true)" |
    curl --config - -s -o "$SCRATCH/answer" -w '%{http_code}' --noproxy '*' -m 30 \
      -H 'content-type: application/json' -H "X-Pmos-Model-Source: ${GW_SOURCE:-hosted}" -H "X-Pmos-Run: $1" \
      -d '{"model":"pmos/default","max_tokens":1,"messages":[{"role":"user","content":"ping"}]}' \
      "http://127.0.0.1:$(env_value PMOS_GATEWAY_PORT || echo 8740)/v1/chat/completions"
}
# A copy of deploy/.env with one key's value replaced; the value comes through the environment.
env_copy_with() { # key (value in RT_VALUE) → prints the copy's path
  local out=$SCRATCH/env.$1
  (umask 077 && K=$1 awk -F= 'BEGIN { k = ENVIRON["K"]; v = ENVIRON["RT_VALUE"] } $1 == k { next } { print } END { print k "=" v }' "$ENV_FILE" >"$out")
  printf '%s\n' "$out"
}
gateway_running() {
  local id
  id=$(compose_container gateway)
  [ -n "$id" ] && [ "$(docker inspect -f '{{.State.Running}}' "$id" 2>/dev/null)" = true ]
}
# Whether any of the patterns on standard input (one per line) is in the gateway's log since a
# time. Prints found or not found; the patterns go to grep as a file, never as arguments.
gateway_log_has() { # since
  local id pats=$SCRATCH/patterns
  (umask 077 && cat >"$pats")
  id=$(compose_container gateway)
  if docker logs --since "$1" "$id" 2>&1 | grep -qF -f "$pats"; then echo found; else echo "not found"; fi
  rm -f "$pats"
}
rt19_start() { # what env-file [ip to watch]
  local what=$1 env=$2 ip=${3:-} t0 seen=0 i
  t0=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  GW_ENV_CHANGED=1
  gw_compose "$env" up -d gateway >/dev/null 2>&1 &
  for ((i = 0; i < 15; i++)); do
    sleep 1
    if [ -n "$ip" ] && [ -n "$(ss -tnH dst "$ip" 2>/dev/null)" ]; then seen=1; fi
  done
  wait
  if gateway_running; then
    fail RT-19 "$what: the gateway is running"
  else
    pass RT-19 "$what: the gateway exited at start"
  fi
  [ "$seen" = 0 ] || fail RT-19 "$what: a connection to the provider's address was open during the start (ss)"
  case "$(printf '%s\n' "$RT19_SECRETS" | sed '/^$/d' | gateway_log_has "$t0")" in
    "not found") pass RT-19 "$what: the gateway's log names neither the URL's host nor a model id" ;;
    *) fail RT-19 "$what: the gateway's log holds the URL's host or a model id" ;;
  esac
}
restore_key() {
  local key="$DEPLOY/secrets/hosted-model.key"
  [ -n "$KEY_BACKUP" ] || return 0
  cat -- "$KEY_BACKUP" >"$key" && rm -f -- "$KEY_BACKUP" && KEY_BACKUP="" && return 0
  echo "COULDN'T put the key back: it is in $KEY_BACKUP (mode 600); move it to $key" >&2
  return 1
}
step_rt19() {
  local url host addr models cls max env key code list="$REPO/crates/gateway/approved-hosted.yaml"
  if ! h12_built; then
    skip RT-19 "not built: skipped (the hosted source is slice H12)"
    [ "$DRY" = 1 ] || return
  elif ! hosted_entry_exists; then
    # Without an approved entry (H13), RT-19 runs as far as it can on a test stack whose gateway
    # carries the test-only list (approved_list: stack-test), against that list's one entry.
    if [ "$DRY" = 0 ] && [ "$(gateway_field approved_list)" = stack-test ]; then
      list="$REPO/crates/gateway/approved-hosted.stack-test.yaml"
      GW_EXTRA="$DEPLOY/test/compose.hosted-stack-test.yaml"
    else
      skip RT-19 "skipped: no approved hosted entry yet (slice H13), and the gateway isn't the stack-test-list one; RT-19 runs once one exists, or on a test stack"
      [ "$DRY" = 1 ] || return
    fi
  fi
  if [ "$DRY" = 1 ]; then
    for what in "PMOS_GATEWAY_HOSTED_URL=https://example.invalid/v1" "PMOS_GATEWAY_HOSTED_URL=<the right URL>?x=1" \
      "PMOS_GATEWAY_HOSTED_URL=<the provider's address as an IP literal>" "PMOS_GATEWAY_HOSTED_MODELS=\$RT19_OTHER_MODELS" \
      "SSL_CERT_FILE in the gateway's environment" "PMOS_HOSTED_MAX_DATA_CLASS above the entry's"; do
      dry RT-19 "would start: docker compose up -d gateway with $what (a temporary env copy): expect the gateway to exit, naming the setting, never the URL or a model id"
    done
    dry RT-19 "would empty deploy/secrets/hosted-model.key (a copy kept beside it) and expect 503 hosted_key_missing"
    byhand RT-19 "the run-time CA: docker cp a test CA into the gateway's /etc/ssl/certs/ and /etc/pki/tls/certs/, point the hosted host at a server with its certificate: the TLS handshake fails"
    return
  fi
  [ "$(env_value PMOS_MODEL_SOURCE || true)" = hosted ] || { skip RT-19 "PMOS_MODEL_SOURCE isn't hosted in deploy/.env: nothing to probe"; return; }
  may_change RT-19 "restarts the gateway with changed settings from a temporary copy of deploy/.env, and empties the hosted key file for a moment" || return
  url=$(env_value PMOS_GATEWAY_HOSTED_URL || true)
  models=$(env_value PMOS_GATEWAY_HOSTED_MODELS || true)
  host=${url#https://}
  host=${host%%/*}
  host=${host%%:*}
  addr=$(getent ahostsv4 "$host" 2>/dev/null | awk 'NR == 1 { print $1 }')
  # What the gateway's log must never hold: the host and each model id (the values after `=`).
  RT19_SECRETS=$(printf '%s\n' "$host"; tr ',' '\n' <<<"$models" | sed 's/^[^=]*=//')
  rt19_start "another https URL" "$(RT_VALUE=https://example.invalid/v1 env_copy_with PMOS_GATEWAY_HOSTED_URL)"
  rt19_start "the right URL with a query" "$(RT_VALUE="$url?x=1" env_copy_with PMOS_GATEWAY_HOSTED_URL)"
  if [ -n "$addr" ]; then
    rt19_start "the provider's address as an IP literal" "$(RT_VALUE="${url/$host/$addr}" env_copy_with PMOS_GATEWAY_HOSTED_URL)" "$addr"
  else
    fail RT-19 "the provider's host doesn't resolve here: the IP-literal start wasn't tried (inconclusive)"
  fi
  if [ -n "${RT19_OTHER_MODELS:-}" ]; then
    rt19_start "a model id changed" "$(RT_VALUE=$RT19_OTHER_MODELS env_copy_with PMOS_GATEWAY_HOSTED_MODELS)"
  else
    skip RT-19 "a model id changed: set RT19_OTHER_MODELS to PMOS_GATEWAY_HOSTED_MODELS with one id changed (it is never printed)"
  fi
  printf 'services:\n  gateway:\n    environment:\n      SSL_CERT_FILE: /etc/ssl/certs/ca-certificates.crt\n' >"$SCRATCH/compose.ssl.yaml"
  GW_EXTRA=$SCRATCH/compose.ssl.yaml rt19_start "SSL_CERT_FILE set" "$ENV_FILE"
  cls=$(sed -n 's/^[[:space:]]*max_data_class:[[:space:]]*"\{0,1\}\([a-z]*\)"\{0,1\}.*/\1/p' "$list" | head -1)
  case "$cls" in public) max=internal ;; internal) max=restricted ;; *) max="" ;; esac
  if [ -n "$max" ]; then
    rt19_start "PMOS_HOSTED_MAX_DATA_CLASS=$max, above the entry's $cls" "$(RT_VALUE=$max env_copy_with PMOS_HOSTED_MAX_DATA_CLASS)"
  else
    skip RT-19 "the entry's max_data_class is '${cls:-unreadable}': there is no class above it to try"
  fi
  # Back on the right settings, then the key emptied while the gateway runs.
  gw_compose "$ENV_FILE" up -d gateway >/dev/null 2>&1 && GW_ENV_CHANGED=0
  sleep 10
  key="$DEPLOY/secrets/hosted-model.key"
  KEY_BACKUP="$DEPLOY/secrets/.hosted-model.key.rt19"
  (umask 077 && cp -p -- "$key" "$KEY_BACKUP") || { KEY_BACKUP=""; fail RT-19 "couldn't keep a copy of the key file: it wasn't emptied"; return; }
  : >"$key" || { restore_key; fail RT-19 "couldn't empty the key file (inconclusive)"; return; }
  sleep 2
  GW_SOURCE=$(gateway_field model_source)
  code=$(gw_call redteam-rt19)
  if [ "$code" = 503 ] && grep -qF hosted_key_missing "$SCRATCH/answer"; then
    pass RT-19 "with the key file emptied, the next call answers 503 hosted_key_missing"
  else
    fail RT-19 "with the key file emptied, the call answered $code, not 503 hosted_key_missing"
  fi
  rm -f "$SCRATCH/answer"
  restore_key || fail RT-19 "the key file wasn't put back"
  byhand RT-19 "nothing keyed left for the provider while the key was empty (the host's proxy log)"
  byhand RT-19 "the run-time CA: docker cp a test CA into the gateway's /etc/ssl/certs/ and /etc/pki/tls/certs/, point the hosted host at a server with its certificate: the TLS handshake fails"
}

# --- RT-20 (slice H12) -------------------------------------------------------------------------

gateway_refuses() { # what
  local code
  sleep 10
  if ! gateway_running; then
    pass RT-20 "$1: the gateway refuses to start while stopped"
    return
  fi
  code=$(gw_call redteam-rt20)
  if [ "$code" = 503 ] && grep -qF hosted_stopped "$SCRATCH/answer"; then
    pass RT-20 "$1: the gateway runs but answers 503 hosted_stopped"
  else
    fail RT-20 "$1: the gateway runs and answered $code, not 503 hosted_stopped"
  fi
  rm -f "$SCRATCH/answer"
}
step_rt20() {
  local id
  if ! h12_built; then
    skip RT-20 "not built: skipped (the hosted source and pmos-admin hosted stop are slice H12)"
    [ "$DRY" = 1 ] || return
  fi
  if [ "$DRY" = 0 ] && [ "$(gateway_field approved_list)" != stack-test ]; then
    skip RT-20 "the gateway isn't the stack-test-list gateway (§3.6): RT-20 runs on a test stack only"
    return
  fi
  # The source the stack-test gateway serves (hosted:stack-test), named on every probe call.
  [ "$DRY" = 1 ] || GW_SOURCE=$(gateway_field model_source)
  may_change RT-20 "stops the hosted model, restarts the gateway three ways and Docker itself, then starts the hosted model again" || return
  GW_EXTRA=""
  [ ! -f "$DEPLOY/test/compose.hosted-stack-test.yaml" ] || GW_EXTRA="$DEPLOY/test/compose.hosted-stack-test.yaml"
  if [ "$DRY" = 1 ]; then
    dry RT-20 "would run: sudo $PMOS_ADMIN hosted stop --reason test"
    dry RT-20 "would run: docker start <the gateway's container>; docker compose up -d gateway; sudo systemctl restart docker: after each, expect the gateway down or answering 503 hosted_stopped"
    dry RT-20 "would try: rm -f $STOP_FILE without sudo: expect it to fail and the file to stay"
    dry RT-20 "would run: sudo $PMOS_ADMIN hosted start --reason test"
    byhand RT-20 "a task streaming an answer is cut at once and fails with 'model unavailable'; the audit log has system.model_source to stopped and back; the web header and App Home show the source"
    return
  fi
  id=$(compose_container gateway)
  HOSTED_STOPPED=1
  sudo "$PMOS_ADMIN" hosted stop --reason test || { fail RT-20 "sudo pmos-admin hosted stop failed"; return; }
  if gateway_running; then fail RT-20 "after hosted stop, the gateway still runs"; else pass RT-20 "hosted stop killed the gateway"; fi
  docker start "$id" >/dev/null 2>&1
  gateway_refuses "docker start of the killed container"
  gw_compose "$ENV_FILE" up -d gateway >/dev/null 2>&1
  gateway_refuses "docker compose up -d gateway by hand"
  sudo systemctl restart docker
  gateway_refuses "sudo systemctl restart docker"
  if rm -f -- "$STOP_FILE" 2>/dev/null && [ ! -e "$STOP_FILE" ]; then
    fail RT-20 "the stop file could be removed without sudo"
  else
    pass RT-20 "the stop file can't be removed without sudo"
  fi
  if sudo "$PMOS_ADMIN" hosted start --reason test; then
    HOSTED_STOPPED=0
    pass RT-20 "hosted start put the hosted model back"
  else
    fail RT-20 "sudo pmos-admin hosted start failed"
  fi
  byhand RT-20 "a task streaming an answer is cut at once and fails with 'model unavailable', and isn't retried until the gateway is back on the same source"
  byhand RT-20 "the audit log has system.model_source to stopped and back; the web header and App Home show the source throughout"
}

# --- RT-21 ----------------------------------------------------------------------------------

readyz_source() {
  curl -s -m 5 --noproxy '*' "http://127.0.0.1:$(env_value PMOS_GATEWAY_PORT || echo 8740)/readyz" 2>/dev/null |
    jq -r '.model_source // empty' 2>/dev/null
}
# The first timestamp (docker logs --timestamps) of a line holding a fixed string, since a time.
first_log_time() { # service since string
  local id
  id=$(compose_container "$1")
  [ -n "$id" ] || return 0
  docker logs --timestamps --since "$2" "$id" 2>&1 | grep -F -- "$3" | awk 'NR == 1 { print $1 }'
}
epoch_of() { date -u -d "$1" +%s.%N 2>/dev/null; }
step_rt21() {
  local s1 s2 t_restart started gw_refused ts_refused i mock_before="" mock_after window
  if [ "$DRY" = 1 ]; then
    dry RT-21 "would read the gateway's source: GET 127.0.0.1:<PMOS_GATEWAY_PORT>/readyz (model_source; must be hosted-test or stub)"
    dry RT-21 "would wait up to ${RT21_WAIT}s for you to restart the gateway on the other source with a session open, then ask the worker to answer"
    dry RT-21 "would read: docker logs of gateway (\"refused before forwarding\", 409) and toolserver (\"tool call refused and session closed\") since the restart: timestamps only"
    dry RT-21 "would compare RT21_MOCK_LOG's line count (the new source's mock upstream) before and after: expect no new line"
    byhand RT-21 "the harness aborts the task; the runtime dispatches nothing until the sources agree"
    return
  fi
  s1=$(readyz_source)
  case "$s1" in
    hosted-test | stub) ;;
    '') echo "the gateway's /readyz gives no model_source: is the stack up?" >&2; exit 2 ;;
    *) echo "the gateway serves '$s1': RT-21 runs only on a test stack (hosted-test or the stub)" >&2; exit 2 ;;
  esac
  [ -z "${RT21_MOCK_LOG:-}" ] || mock_before=$(wc -l <"$RT21_MOCK_LOG" 2>/dev/null || echo "")
  note "the gateway serves $s1. With a session open (a task the worker is on), restart the gateway on the"
  note "other source (hosted-test <-> local with the stub), then ask the worker to answer. Waiting up to ${RT21_WAIT}s."
  s2=$s1
  for ((i = 0; i < RT21_WAIT; i++)); do
    sleep 1
    s2=$(readyz_source)
    [ -z "$s2" ] || [ "$s2" = "$s1" ] || break
  done
  if [ -z "$s2" ] || [ "$s2" = "$s1" ]; then
    fail RT-21 "the gateway's source didn't change within ${RT21_WAIT}s (inconclusive)"
    return
  fi
  started=$(docker inspect -f '{{.State.StartedAt}}' "$(compose_container gateway)" 2>/dev/null)
  t_restart=$(epoch_of "$started")
  note "the gateway now serves $s2 (started $started). Watching the logs for up to ${RT21_WAIT}s."
  gw_refused="" ts_refused=""
  for ((i = 0; i < RT21_WAIT; i += 2)); do
    [ -n "$gw_refused" ] || gw_refused=$(first_log_time gateway "$started" "refused before forwarding")
    [ -n "$ts_refused" ] || ts_refused=$(first_log_time toolserver "$started" "tool call refused and session closed")
    [ -z "$gw_refused" ] || [ -z "$ts_refused" ] || break
    sleep 2
  done
  if [ -n "$gw_refused" ]; then
    pass RT-21 "the worker's first model call after the restart was refused before forwarding (gateway, $gw_refused)"
  else
    fail RT-21 "no model call was refused by the gateway after the restart (409 model_source_mismatch expected)"
  fi
  if [ -n "$ts_refused" ]; then
    window=$(awk -v a="$(epoch_of "$ts_refused")" -v b="$t_restart" 'BEGIN { printf "%.1f", a - b }')
    pass RT-21 "a tool call was refused and its session closed (tool server, $ts_refused)"
    note "measured window, restart to the first tool-call refusal: ${window}s (write it in the record)"
  else
    fail RT-21 "no tool call was refused after the restart (409 model_source_changed expected)"
  fi
  if [ -n "${RT21_MOCK_LOG:-}" ]; then
    mock_after=$(wc -l <"$RT21_MOCK_LOG" 2>/dev/null || echo "")
    if [ -n "$mock_before" ] && [ "$mock_before" = "$mock_after" ]; then
      pass RT-21 "the new source's mock upstream received nothing ($RT21_MOCK_LOG unchanged)"
    else
      fail RT-21 "the new source's mock upstream log changed or couldn't be read ($mock_before → $mock_after lines)"
    fi
  else
    skip RT-21 "the new source's mock upstream: set RT21_MOCK_LOG to its request log to check it received nothing"
  fi
  byhand RT-21 "the harness aborts the task, and the runtime dispatches nothing until the sources agree"
}

case "$STEP" in
  rt16) step_rt16 ;;
  rt19) step_rt19 ;;
  rt20) step_rt20 ;;
  rt21) step_rt21 ;;
esac
finish
