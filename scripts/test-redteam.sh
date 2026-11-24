#!/usr/bin/env bash
# Checks the red-team scripts in policies/redteam/ (slice H7, docs/openshell-any-host-spec.md §5)
# without OpenShell, Docker or a host: every script parses; each dry run prints its probes and
# runs nothing; arguments are refused; a feature another slice builds (H10, H12, an approved
# entry) is reported "not built" and skipped, with exit 3, never as a pass; a host-changing step
# without --allow-host-changes is skipped and changes nothing; RT-13b's temporary hosts line is
# put back on every exit, Ctrl-C included; RT-16's flush is refused on a chain with other rules
# and reapplied after; RT-18 prints only "found" or "not found", and no output holds the key,
# its first 8 characters or the provider's address. CI's deploy job runs it.
#
# Everything runs on a copy of the scripts in a fake checkout under a temporary directory, with
# stand-ins for openshell, sudo, docker, curl, timeout, sleep, iptables, ip6tables and
# systemctl on PATH, and a stand-in deploy/pmos-admin and deploy/preflight.sh. The only change
# to a script is the one line of isolation.sh that names /etc/hosts, so nothing here can write
# the machine's /etc/hosts. Nothing it runs opens a network connection. What OpenShell and the
# host actually do is shown only on a real host (checklist.md).
set -euo pipefail

SRC="$(cd "$(dirname "$0")/.." && pwd)"
RT="$SRC/policies/redteam"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/pmos-redteam-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
n=0

for f in "$RT"/*.sh; do bash -n "$f"; done
[ "$(grep -c '^readonly HOSTS_FILE=/etc/hosts$' "$RT/isolation.sh")" = 1 ] || fail "isolation.sh doesn't name /etc/hosts exactly once"
[ "$(grep -c 'HOSTS_FILE=' "$RT/isolation.sh")" = 1 ] || fail "isolation.sh sets HOSTS_FILE somewhere else too"
! grep -q 'HOSTS_FILE' "$RT/exposure.sh" "$RT/stack.sh" "$RT/lib.sh" || fail "only isolation.sh may name the hosts file"
# No script takes an override for the hosts file or pmos-admin (RT-13b: no override).
! grep -Eq '\$\{(HOSTS_FILE|PMOS_ADMIN|REDTEAM_[A-Z_]*):-' "$RT"/*.sh || fail "a red-team script takes an override from the environment"

# --- the fake checkout -------------------------------------------------------------------------

R="$WORK/repo"
BIN="$WORK/bin"
LOG="$WORK/calls.log"
KEY=sk-FAKEKEY-0123456789abcdef
PREFIX=${KEY:0:8}
PROVIDER=192.0.2.7
GOOD_BLOCK='# BEGIN pmos (managed by pmos-admin)
10.201.77.10 toolserver.pmos.internal
10.201.77.11 gateway.pmos.internal
10.201.77.12 bots.pmos.internal
# END pmos'
good_hosts() { printf '127.0.0.1 localhost\n%s\n' "$GOOD_BLOCK" >"$WORK/hosts"; }

fresh_repo() { # [h10] [h12] [entry]
  rm -rf "$R" "$WORK/ws"
  mkdir -p "$R/policies/redteam" "$R/policies/bots" "$R/deploy" "$R/crates/gateway" "$WORK/ws"
  cp "$RT/lib.sh" "$RT/exposure.sh" "$RT/stack.sh" "$R/policies/redteam/"
  sed "s|^readonly HOSTS_FILE=/etc/hosts\$|readonly HOSTS_FILE=$WORK/hosts|" "$RT/isolation.sh" >"$R/policies/redteam/isolation.sh"
  [ "$(grep -c "^readonly HOSTS_FILE=$WORK/hosts\$" "$R/policies/redteam/isolation.sh")" = 1 ] || fail "the copy of isolation.sh doesn't use the test's hosts file"
  chmod 755 "$R"/policies/redteam/*.sh
  cp "$SRC/policies/bots/docs.yaml" "$SRC/policies/bots/docs.sandbox.yaml" "$R/policies/bots/"
  cp "$SRC/deploy/compose.yaml" "$R/deploy/compose.yaml"
  printf '%s\n' "PMOS_MODEL_SOURCE=hosted-test" "PMOS_HOSTED_TEST_URL=https://$PROVIDER/v1" \
    "PMOS_GATEWAY_REMOTE_KEY=$KEY" "PMOS_GW_RUNTIME_TOKEN=runtime-token-fake" >"$R/deploy/.env"
  chmod 600 "$R/deploy/.env"
  local a hosted="" h10=0
  for a in "$@"; do
    case "$a" in
      h10) h10=1 ;;
      h12) printf 'entries: []\n' >"$R/crates/gateway/approved-hosted.yaml"; hosted='  hosted)' ;;
      entry) printf 'entries:\n  - name: test\n    url_sha256: "%064d"\n    max_data_class: internal\n' 0 >"$R/crates/gateway/approved-hosted.yaml" ;;
    esac
  done
  # Slice H10 is built in this checkout: without `h10`, the copy is made as a checkout before it
  # (no admin listener in compose.yaml), so the scripts' "not built" path stays tested.
  if [ "$h10" = 0 ]; then
    grep -v 'PMOS_WEB_ADMIN' "$SRC/deploy/compose.yaml" | grep -v ':8772:8772' >"$R/deploy/compose.yaml"
  fi
  # The stand-in pmos-admin: host-names check and apply on the test's hosts file,
  # host-firewall check (BLOCK_APPLIED) and apply, hosted stop and start; every call logged.
  cat >"$R/deploy/pmos-admin" <<EOF
#!/usr/bin/env bash
printf 'pmos-admin %s\n' "\$*" >>"$LOG"
case "\$1 \${2:-}" in
  "host-names check") grep -qxF '10.201.77.10 toolserver.pmos.internal' "$WORK/hosts" ;;
  "host-names apply")
    awk '/^# BEGIN pmos/ { skip = 1 } !skip { print } /^# END pmos/ { skip = 0 }' "$WORK/hosts" >"$WORK/hosts.new"
    printf '%s\n' "\$GOOD_BLOCK_TEXT" >>"$WORK/hosts.new"
    mv "$WORK/hosts.new" "$WORK/hosts" ;;
  "host-firewall check") [ "\${BLOCK_APPLIED:-1}" = 1 ] ;;
  "host-firewall apply") : ;;
  "hosted stop" | "hosted start") : ;;
  *) exit 2 ;;
esac
exit \$?
EOF
  chmod 755 "$R/deploy/pmos-admin"
  if [ -n "$hosted" ]; then
    # h12_built looks for pmos-admin's top-level `  hosted)` command; never reached here.
    printf 'case x in\n  hosted) ;;\nesac\n' >>"$R/deploy/pmos-admin"
  fi
  # The stand-in preflight: H-METADATA as PF_METADATA says (default pass).
  cat >"$R/deploy/preflight.sh" <<EOF
#!/usr/bin/env bash
printf 'preflight %s\n' "\$*" >>"$LOG"
printf '{"checks": [{"id": "H-METADATA", "status": "%s", "measured": "probe: none", "expected": "", "hint": ""}]}\n' "\${PF_METADATA:-pass}"
EOF
  chmod 755 "$R/deploy/preflight.sh"
  good_hosts
  : >"$LOG"
}
export GOOD_BLOCK_TEXT="$GOOD_BLOCK"

# --- the stand-ins -----------------------------------------------------------------------------

mkdir -p "$BIN"
SLEEP=$(command -v sleep)
# openshell: sandbox get/exec, policy, settings and logs. A request made by the worker binary
# (node) is never run: it answers as OS_HTTP says (403 by default), and for the tool server
# 401 while the test's hosts file points its name at .10, 403 otherwise. Any other command in a
# sandbox runs here, with /sandbox/workspace mapped to $WORK/ws.
cat >"$BIN/openshell" <<EOF
#!/usr/bin/env bash
printf 'openshell %s\n' "\$*" >>"$LOG"
case "\$1 \${2:-}" in
  "sandbox get") exit 0 ;;
  "policy list" | "policy get" | "settings get") exit 0 ;;
  "logs "*) printf '%s\n' "\${OS_LOGS:-}"; exit 0 ;;
  "sandbox exec") ;;
  *) exit 2 ;;
esac
shift 2
while [ "\$1" != -- ]; do shift; done
shift
if [ "\$1" = /usr/local/bin/node ]; then
  prog=\$(cat)
  if [[ "\$prog" == *toolserver.pmos.internal:8710/mcp* ]]; then
    if [ -n "\${OS_SLOW:-}" ] && grep -q '^10.201.77.11 toolserver' "$WORK/hosts"; then "$SLEEP" 3; fi
    if grep -qxF '10.201.77.10 toolserver.pmos.internal' "$WORK/hosts"; then echo 'STATUS 401 {"error":"unauthorized"}'; else echo 'STATUS 403 {"error":"policy_denied"}'; fi
  else
    echo "\${OS_HTTP:-STATUS 403 {\"error\":\"policy_denied\"}}"
  fi
  exit 0
fi
args=()
for a in "\$@"; do args+=("\${a//\/sandbox\/workspace/$WORK/ws}"); done
exec "\${args[@]}"
EOF
# sudo: logs, then runs the command as this user.
printf '#!/usr/bin/env bash\nprintf "sudo %%s\\n" "$*" >>"%s"\nexec "$@"\n' "$LOG" >"$BIN/sudo"
# docker: the stack's containers; `run` answers as DOCKER_RUN says (curl's exit 7 by default).
cat >"$BIN/docker" <<EOF
#!/usr/bin/env bash
printf 'docker %s\n' "\$*" >>"$LOG"
case "\$1" in
  image | network) exit 0 ;;
  ps) [ "\${2:-}" = -aq ] && echo c0ffee || printf 'pmos-gateway-1\npmos-toolserver-1\n' ;;
  run) echo "\${DOCKER_RUN:-7}" ;;
  inspect) echo "\${DOCKER_INSPECT:-false}" ;;
  logs) : ;;
  *) : ;;
esac
EOF
# curl: only RT-21's /readyz and exposure.sh's requests reach it here.
cat >"$BIN/curl" <<EOF
#!/usr/bin/env bash
printf 'curl %s\n' "\$*" >>"$LOG"
for a in "\$@"; do
  case "\$a" in
    */readyz) [[ " \$* " == *" -w "* ]] || { printf '{"model_source": "%s"}\n' "\${READYZ_SOURCE:-local}"; exit 0; } ;;
  esac
done
case " \$* " in
  *password/sign-in*) printf '{"error":"password_sign_in_not_from_here"}\n%s' "\${CURL_SIGNIN:-403}" ;;
  *) printf 'not found\n%s' "\${CURL_CODE:-404}" ;;
esac
EOF
# timeout: a TCP probe (bash -c … /dev/tcp) succeeds only for a port in OPEN_PORTS; nothing
# connects. Anything else runs.
cat >"$BIN/timeout" <<'EOF'
#!/usr/bin/env bash
shift
if [ "$1" = bash ] && [ "${2:-}" = -c ] && [[ "$3" == *"/dev/tcp/"* ]]; then
  [[ " ${OPEN_PORTS:-} " == *" $6 "* ]]
  exit
fi
exec "$@"
EOF
printf '#!/usr/bin/env bash\nexit 0\n' >"$BIN/sleep"
# iptables and ip6tables: DOCKER-USER as CHAIN_RULES says; -C succeeds for RETURN when listed.
cat >"$BIN/iptables" <<EOF
#!/usr/bin/env bash
printf '%s %s\n' "\$(basename "\$0")" "\$*" >>"$LOG"
case " \$* " in
  *" -S DOCKER-USER "*) printf '%s\n' "-N DOCKER-USER" \${CHAIN_RULES:+"\$CHAIN_RULES"} '-A DOCKER-USER -j RETURN' ;;
  *" -C DOCKER-USER -j RETURN "*) exit 0 ;;
  *) exit 0 ;;
esac
EOF
cp "$BIN/iptables" "$BIN/ip6tables"
printf '#!/usr/bin/env bash\nprintf "systemctl %%s\\n" "$*" >>"%s"\n' "$LOG" >"$BIN/systemctl"
chmod 755 "$BIN"/*
export PATH="$BIN:$PATH"
# The stand-in for the installed block script (stack.sh names /usr/local/sbin/pmos-metadata-block,
# which isn't on this machine: `sudo … check` fails there, which RT-16 reports).

OUT="" CODE=0
run() { # script arguments…
  local s=$1
  shift
  CODE=0
  OUT=$("$R/policies/redteam/$s" "$@" 2>&1) || CODE=$?
  n=$((n + 1))
}
expect_code() { [ "$CODE" = "$1" ] || fail "$2: exit $CODE, not $1: $OUT"; }
has() { grep -qF -- "$1" <<<"$OUT" || fail "$2: no '$1' in: $OUT"; }
hasnt() { ! grep -qF -- "$1" <<<"$OUT" || fail "$2: '$1' printed: $OUT"; }
no_secrets() { # label
  hasnt "$KEY" "$1"
  hasnt "$PREFIX" "$1"
  hasnt "$PROVIDER" "$1"
  hasnt runtime-token-fake "$1"
}
no_change_calls() { # label: nothing that changes the host was called
  ! grep -Eq '^(sudo|systemctl|iptables|ip6tables) |^docker (compose|start)|^pmos-admin (host-names apply|host-firewall apply|hosted)' "$LOG" ||
    fail "$1: a host-changing command ran: $(cat "$LOG")"
}

# --- arguments ---------------------------------------------------------------------------------

fresh_repo
for a in "" "docs" "docs docs" "../x pm-a" "docs A" "docs pm--a" "--bogus docs pm-a" "--only RT-x docs pm-a" "--only"; do
  # shellcheck disable=SC2086
  run isolation.sh $a
  expect_code 2 "isolation.sh $a"
done
for a in "" "a b" "-x" "--web maybe h" "--ssh nobody h" "h;id" "--private --web on 10.0.0.1"; do
  # shellcheck disable=SC2086
  run exposure.sh $a
  expect_code 2 "exposure.sh $a"
done
for a in "" "rt99" "rt16 rt19" "--bogus rt16"; do
  # shellcheck disable=SC2086
  run stack.sh $a
  expect_code 2 "stack.sh $a"
done

# --- dry runs: every probe printed, nothing run --------------------------------------------------

fresh_repo
run isolation.sh --dry-run docs pm-redteam
expect_code 0 "isolation dry run"
for id in RT-1 RT-2 RT-3 RT-4 RT-5 RT-6 RT-7 RT-8 RT-9 RT-10 RT-11 RT-12 RT-13 RT-13b RT-18; do has "$(printf 'DRY  %-7s ' "$id")" "isolation dry run"; done
for h in 169.254.169.254:80 169.254.169.254:443 metadata.google.internal:80 '[fd00:ec2::254]:443' 168.63.129.16:80 100.100.100.200:443 \
  10.201.77.1:8770 10.201.77.10:8740 "10.201.77.11 toolserver.pmos.internal" "10.201.78.10 toolserver.pmos.internal" \
  /var/run/docker.sock "the key's first 8 characters"; do
  has "$h" "isolation dry run"
done
# RT-12 probes this machine's own addresses, when it has any.
if ip -o addr show scope global 2>/dev/null | grep -q .; then
  for p in 22 17670 2375 2376; do has ":$p/" "isolation dry run (RT-12)"; done
fi
has "not a result" "isolation dry run"
no_secrets "isolation dry run"
! grep -q '^openshell sandbox exec' "$LOG" || fail "the dry run ran something in a sandbox"
no_change_calls "isolation dry run"
[ "$(cat "$WORK/hosts")" = "$(printf '127.0.0.1 localhost\n%s' "$GOOD_BLOCK")" ] || fail "the dry run changed the hosts file"

run exposure.sh --dry-run --web on --admin-published 203.0.113.5
expect_code 0 "exposure dry run"
for p in 5432 8710 8730 8740 8750 8760 8770 8771 8772 17670 22 443; do has "203.0.113.5:$p " "exposure dry run"; done
has "SKIP RT-14   not built: skipped" "exposure dry run"
run exposure.sh --dry-run --private 10.0.0.5
expect_code 0 "exposure RT-15 dry run"
has "10.201.77.10:8710" "exposure RT-15 dry run"

for s in rt16 rt19 rt20 rt21; do
  : >"$LOG"
  run stack.sh --dry-run "$s"
  expect_code 0 "stack $s dry run"
  has "DRY  RT-${s#rt} " "stack $s dry run"
  no_secrets "stack $s dry run"
  [ ! -s "$LOG" ] || fail "stack $s dry run called: $(cat "$LOG")"
done
run stack.sh --dry-run rt16
has "sudo systemctl restart docker" "stack rt16 dry run"
has "iptables -w -F DOCKER-USER" "stack rt16 dry run"

# --- not built: skipped with exit 3, never a pass ------------------------------------------------

fresh_repo
run stack.sh rt19
expect_code 3 "RT-19 before H12"
has "SKIP RT-19   not built: skipped" "RT-19 before H12"
hasnt PASS "RT-19 before H12"
run stack.sh rt20
expect_code 3 "RT-20 before H12"
has "SKIP RT-20   not built: skipped" "RT-20 before H12"
fresh_repo h12
run stack.sh rt19
expect_code 3 "RT-19 with H12 and no entry"
has "no approved hosted entry" "RT-19 with H12 and no entry"
fresh_repo h12 entry
printf 'PMOS_MODEL_SOURCE=hosted\nPMOS_GATEWAY_HOSTED_URL=https://%s/v1\n' "$PROVIDER" >>"$R/deploy/.env"
run stack.sh rt19
expect_code 3 "RT-19 with an entry, without --allow-host-changes"
has "not run: it changes the host" "RT-19 without --allow-host-changes"
no_change_calls "RT-19 without --allow-host-changes"
no_secrets "RT-19 without --allow-host-changes"

# exposure.sh: closed ports pass; an open one fails; through 443, H10's routes are skipped until
# H10 is built, and checked after.
fresh_repo
OPEN_PORTS="" run exposure.sh 198.51.100.9
expect_code 0 "exposure, everything closed"
OPEN_PORTS="8771" run exposure.sh 198.51.100.9
expect_code 1 "exposure, 8771 open"
has "FAIL RT-14   198.51.100.9:8771" "exposure, 8771 open"
OPEN_PORTS="443" run exposure.sh --web on 198.51.100.9
expect_code 3 "exposure, web on, before H10"
has "SKIP RT-14   not built" "exposure, web on, before H10"
OPEN_PORTS="443" run exposure.sh --web on --admin-published 198.51.100.9
expect_code 3 "exposure, 8772 published, before H10"
has "PASS RT-14   198.51.100.9:8772 (the admin listener's port, before H10)" "exposure, 8772 published, before H10"
fresh_repo h10
OPEN_PORTS="443" run exposure.sh --web on 198.51.100.9
expect_code 0 "exposure, web on, H10 built"
has "PASS RT-14   POST /api/auth/password/sign-in" "exposure, web on, H10 built"
OPEN_PORTS="443" CURL_SIGNIN=200 run exposure.sh --web on 198.51.100.9
expect_code 1 "exposure, the password sign-in open to outside"
# 8772 published on the public address on purpose (a test host): every request from outside
# answers 404 (the admin door's peer check), and anything else fails.
OPEN_PORTS="443 8772" run exposure.sh --web on --admin-published 198.51.100.9
has "PASS RT-14   GET /api/admin/users on the admin listener published on purpose" "exposure, 8772 published, H10 built"
has "HAND RT-14   on the host: deploy/preflight.sh fails H-ADMIN" "exposure, 8772 published, H10 built"
[[ "$OUT" != *"not built"* ]] || fail "exposure, 8772 published, H10 built: still says not built"
OPEN_PORTS="443 8772" CURL_CODE=200 run exposure.sh --web on --admin-published 198.51.100.9
expect_code 1 "exposure, the admin listener answering from outside"
# This checkout itself is detected as built (the real compose.yaml).
grep -q '^      PMOS_WEB_ADMIN_BIND: 0.0.0.0:8772$' "$SRC/deploy/compose.yaml" || fail "this checkout's compose.yaml has no admin listener"

# --- RT-13b: the temporary hosts line, put back on every exit ----------------------------------

fresh_repo
run isolation.sh --only RT-13b docs pm-redteam
expect_code 3 "RT-13b without --allow-host-changes"
has "not run: it changes the host" "RT-13b without --allow-host-changes"
no_change_calls "RT-13b without --allow-host-changes"
: >"$LOG"
# The stand-in answers 401 (the tool server) while the name points at .10, and 403 policy_denied
# while it points elsewhere.
run isolation.sh --allow-host-changes --only RT-13b docs pm-redteam
expect_code 0 "RT-13b, refused while swapped"
has "PASS RT-13b  control: before the change" "RT-13b"
has "PASS RT-13b  host-names check fails while toolserver.pmos.internal points at 10.201.77.11" "RT-13b"
has "PASS RT-13b  after the test, host-names check passes again" "RT-13b"
grep -q 'pmos-admin host-names apply' "$LOG" || fail "RT-13b didn't put the block back with host-names apply"
grep -qxF '10.201.77.10 toolserver.pmos.internal' "$WORK/hosts" || fail "RT-13b left the hosts file changed"
# Interrupted while the line is there: the trap puts the block back.
fresh_repo
OS_SLOW=1 "$R/policies/redteam/isolation.sh" --allow-host-changes --only RT-13b docs pm-redteam >"$WORK/int.out" 2>&1 &
pid=$!
for _ in $(seq 1 100); do grep -q '^10.201.77.11 toolserver' "$WORK/hosts" && break; /bin/sleep 0.05; done
grep -q '^10.201.77.11 toolserver' "$WORK/hosts" || fail "RT-13b never wrote the temporary line"
kill -TERM "$pid"
code=0
wait "$pid" || code=$?
[ "$code" = 130 ] || fail "RT-13b interrupted exits $code, not 130: $(cat "$WORK/int.out")"
grep -qxF '10.201.77.10 toolserver.pmos.internal' "$WORK/hosts" || fail "an interrupted RT-13b left the temporary line: $(cat "$WORK/hosts")"
n=$((n + 1))
# A failing names check before the test: nothing is changed.
fresh_repo
sed -i 's/^10.201.77.10 toolserver/10.201.77.99 toolserver/' "$WORK/hosts"
run isolation.sh --allow-host-changes --only RT-13b docs pm-redteam
expect_code 1 "RT-13b with a failing names check"
! grep -q '^sudo' "$LOG" || fail "RT-13b changed the hosts file although host-names check failed"

# --- RT-18: only found or not found ----------------------------------------------------------

fresh_repo
run isolation.sh --only RT-18 docs pm-redteam
expect_code 0 "RT-18, nothing found"
has "PASS RT-18   the provider key's first 8 characters in A's environment: not found" "RT-18"
has "PASS RT-18   the provider key's first 8 characters in A's /sandbox/workspace: not found" "RT-18"
no_secrets "RT-18, nothing found"
! grep -F -- "$PREFIX" "$LOG" | grep -q . || fail "the key's prefix reached a command's arguments"
printf 'x %s y\n' "$PREFIX" >"$WORK/ws/notes.txt"
run isolation.sh --only RT-18 docs pm-redteam
expect_code 1 "RT-18, the prefix in the workspace"
has "FAIL RT-18   the provider key's first 8 characters in A's /sandbox/workspace: found" "RT-18"
no_secrets "RT-18, found in the workspace"
rm -f "$WORK/ws/notes.txt"
LEAKED=$PREFIX run isolation.sh --only RT-18 docs pm-redteam
expect_code 1 "RT-18, the prefix in the environment"
has "in A's environment: found" "RT-18"
no_secrets "RT-18, found in the environment"
OS_HTTP='STATUS 200 {}' run isolation.sh --only RT-18 docs pm-redteam
expect_code 1 "RT-18, the provider answers"
has "something answered through the sandbox" "RT-18"
no_secrets "RT-18, the provider answers"
OS_HTTP='ERROR ECONNREFUSED' run isolation.sh --only RT-18 docs pm-redteam
expect_code 1 "RT-18, no connection and no log line"
has "inconclusive" "RT-18, no connection and no log line"
OS_HTTP='ERROR ECONNREFUSED' OS_LOGS="OCSF NET:OPEN [MED] DENIED /usr/local/bin/node(64) -> $PROVIDER:443 [policy:- engine:opa]" \
  run isolation.sh --only RT-18 docs pm-redteam
expect_code 0 "RT-18, no connection and logged"
no_secrets "RT-18, logged"
sed -i 's/^PMOS_MODEL_SOURCE=.*/PMOS_MODEL_SOURCE=hosted/' "$R/deploy/.env"
run isolation.sh --only RT-18 docs pm-redteam
expect_code 3 "RT-18 under hosted before H12"
has "not built: skipped" "RT-18 under hosted before H12"

# RT-11: each address on 80 and 443, refused and so passed; an answer fails.
fresh_repo
run isolation.sh --only RT-11 docs pm-redteam
expect_code 0 "RT-11 refused"
[ "$(grep -c '^PASS RT-11' <<<"$OUT")" = 10 ] || fail "RT-11 didn't probe five addresses on two ports: $OUT"
OS_HTTP='STATUS 200 {"Code":"Success"}' run isolation.sh --only RT-11 docs pm-redteam
expect_code 1 "RT-11 answered"
hasnt Success "RT-11 answered (no body printed)"

# --- RT-16: changes only with the flag; the flush refused on a foreign rule, reapplied after ------

fresh_repo
run stack.sh rt16
expect_code 3 "RT-16 without --allow-host-changes"
has "PASS RT-16   before: H-METADATA passes" "RT-16"
has "PASS RT-16   before: a TCP connect to 169.254.169.254:80" "RT-16"
no_change_calls "RT-16 without --allow-host-changes"
CHAIN_RULES='-A DOCKER-USER -s 10.9.0.0/16 -j DROP' run stack.sh --allow-host-changes rt16
expect_code 3 "RT-16 with a foreign rule"
has "holds rules that aren't the block's: not flushed" "RT-16 with a foreign rule"
! grep -q 'iptables -w -F' "$LOG" || fail "RT-16 flushed a chain with a foreign rule"
fresh_repo
run stack.sh --allow-host-changes rt16
grep -q '^sudo systemctl restart docker' "$LOG" || fail "RT-16 didn't restart Docker"
grep -q '^iptables -w -F DOCKER-USER' "$LOG" || fail "RT-16 didn't flush DOCKER-USER"
grep -q '^pmos-admin host-firewall apply' "$LOG" || fail "RT-16 didn't reapply the block after the flush"
# The installed block script isn't on this machine, so its check after the flush fails here.
expect_code 1 "RT-16 with the block's check failing"
has "a block rule is still missing" "RT-16"
BLOCK_APPLIED=0 run stack.sh --allow-host-changes rt16
expect_code 3 "RT-16 without the block"
has "the block isn't applied" "RT-16 without the block"

# --- RT-21: only on a test stack; observes, changes nothing ------------------------------------

fresh_repo
READYZ_SOURCE=local run stack.sh rt21
expect_code 2 "RT-21 on a local gateway"
has "runs only on a test stack" "RT-21 on a local gateway"
no_change_calls "RT-21"

echo "redteam tests: $n cases passed"
