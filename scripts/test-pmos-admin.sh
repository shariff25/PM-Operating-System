#!/usr/bin/env bash
# Checks deploy/pmos-admin without Docker or a stack (CI's deploy job runs it):
# - `evals-gate` parses `--checkout` and `--base`, defaults to the script's own repo, and
#   refuses unknown options (a dry parse, on a copy of the script with a throwaway deploy/.env);
# - the eval gate treats a checkout as data: every line of the gate and of eval_stack_up that
#   touches the checkout is one of the reviewed forms (git on it, a copy of its content/, a
#   read-only check, the runner's mount). A new use fails here until it is reviewed.
# - deploy/.env is read as data, not run as shell, and setup.sh writes only plain values (H0);
# - bot names are checked as PM OS's valid_name before they reach deploy/.env, a path, a lookup,
#   OpenShell or systemd; tokens are found by exact key; `sandbox` creates with `--from` and
#   starts the worker as the systemd user unit deploy/systemd/pmos-worker@.service (checked
#   with systemd-analyze), only with lingering on; `pause` and `resume` drive the unit (H0).
# - from the security review of 6bb806f (H0b): no token reaches curl's, docker's or openshell's
#   arguments (curl reads it with --config -, docker and openshell from their environment);
#   existing providers are updated, not created again; `pause` refuses root, says what is left
#   and stops the sandbox whatever happened to the unit; `pause` and `down` go on with a
#   deploy/.env that can't be parsed, has the wrong mode or (pause) is missing, and everything
#   else stops; deploy/.env's mode and owner are checked; it is rewritten by a rename, under a
#   lock, keeping its ACL; `revoke`, `smoke`, `logs` and `restart` take only a handle, a known
#   provider or a known service; setup.sh refuses a clone URL with credentials in it.
# - from the review of eb49991: no value written to deploy/.env reaches awk's (or any recorded
#   tool's) arguments, from pmos-admin or setup.sh (T1); `pause` never reads deploy/.env, so one
#   the caller can't read doesn't stop it (T2); a PMOS_UID or PMOS_GID of 0 is refused, and
#   setup.sh refuses a root-owned checkout (T3); `pause` always tries the unit and names it when
#   refusing root; without flock, deploy/.env is not changed; an interrupted write leaves deploy/.env as it was.
# - slice H1: `up`, `restore` and `add-bot` run the host preflight's `run` stage, `sandbox` and
#   `resume` add its OpenShell checks, and each refuses with exit 2 on a failing host, before
#   anything starts or is written; `resume` checks the names; setup.sh runs the `install` stage
#   first; the worker unit runs the `quick` stage (ExecStartPre); `host-firewall` applies, checks
#   and removes the metadata block (rules on pmos0 and docker0, the docker.service drop-in, the
#   timer); `host acknowledge` takes H-PRIVATE only, `host attest local-gpu` needs the owner's
#   statement, each writes deploy/.env and the root-owned log; `status` shows them. The preflight
#   itself is tested in scripts/test-preflight.sh; here it reads the fixture host in $WORK/pfroot.
set -euo pipefail

SRC="$(cd "$(dirname "$0")/.." && pwd)/deploy/pmos-admin"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/pmos-admin-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

bash -n "$SRC"

# Every copy of pmos-admin here reads and writes a hosts file under $WORK, never the machine's
# /etc/hosts (slice H2): the one line that names the file is changed in the copy, and nothing
# else. The script has no other way to point it elsewhere (no setting, no argument), so the copy
# is how a test gets one; these checks often run as root, where /etc/hosts is writable.
[ "$(grep -c '^readonly HOSTS_FILE=/etc/hosts$' "$SRC")" = 1 ] || fail "pmos-admin doesn't name /etc/hosts exactly once"
[ "$(grep -c 'HOSTS_FILE=' "$SRC")" = 1 ] || fail "pmos-admin sets HOSTS_FILE somewhere else too"
# Slice H1 in the same way: the host's own files (the metadata block, the acknowledgement log) are
# under $WORK/pfroot in the copy (HOST_PREFIX), and the copy's preflight reads that directory as
# its fixture host (HOST_ROOT_DEFAULT), a copy of deploy/test/preflight/ubuntu-24.04-amd64. The
# copy's deploy/preflight.sh records its arguments (PF_LOG) and runs that copy.
PFROOT="$WORK/pfroot"
PF_FIXTURE="$(cd "$(dirname "$0")/.." && pwd)/deploy/test/preflight/ubuntu-24.04-amd64"
[ "$(grep -c '^readonly HOST_PREFIX=""$' "$SRC")" = 1 ] && [ "$(grep -c 'HOST_PREFIX=' "$SRC")" = 1 ] ||
  fail "pmos-admin doesn't set HOST_PREFIX exactly once, empty"
good_host() { # the fixture host, as it passes every check (with this machine's clock)
  rm -rf "$PFROOT"
  cp -a "$PF_FIXTURE" "$PFROOT"
  rm -f "$PFROOT/cmd/now"
}
good_host
copy_preflight() { # deploy directory: a preflight reading $PFROOT, and what it sources
  local d=$1 src
  src="$(dirname "$SRC")"
  sed "s|^readonly HOST_ROOT_DEFAULT=\"\"\$|readonly HOST_ROOT_DEFAULT=\"$PFROOT\"|" "$src/preflight.sh" >"$d/preflight-real.sh"
  chmod 755 "$d/preflight-real.sh"
  [ "$(grep -c "^readonly HOST_ROOT_DEFAULT=\"$PFROOT\"\$" "$d/preflight-real.sh")" = 1 ] ||
    fail "the copy of the preflight in $d doesn't read the fixture host"
  printf '#!/usr/bin/env bash\nprintf "%%s\\n" "$*" >>"${PF_LOG:-/dev/null}"\nexec "$(dirname "$0")/preflight-real.sh" "$@"\n' >"$d/preflight.sh"
  chmod 755 "$d/preflight.sh"
  cp "$src/sandbox-create.sh" "$d/"
  sed -e "s|^readonly CONF=/etc/pmos/metadata-block.conf\$|readonly CONF=$PFROOT/etc/pmos/metadata-block.conf|" \
    -e "s|^readonly STATUS=/run/pmos/metadata-block.status\$|readonly STATUS=$PFROOT/run/pmos/metadata-block.status|" \
    "$src/pmos-metadata-block" >"$d/pmos-metadata-block"
  chmod 755 "$d/pmos-metadata-block"
  [ "$(grep -c "^readonly \(CONF\|STATUS\)=$PFROOT/" "$d/pmos-metadata-block")" = 2 ] ||
    fail "the copy of pmos-metadata-block in $d doesn't use the fixture host"
}
copy_admin() { # destination; the hosts file it uses is $WORK/hosts
  sed -e "s|^readonly HOSTS_FILE=/etc/hosts\$|readonly HOSTS_FILE=$WORK/hosts|" \
    -e "s|^readonly HOST_PREFIX=\"\"\$|readonly HOST_PREFIX=\"$PFROOT\"|" "$SRC" >"$1"
  chmod 755 "$1"
  [ "$(grep -c "^readonly HOSTS_FILE=$WORK/hosts\$" "$1")" = 1 ] && [ "$(grep -c 'HOSTS_FILE=' "$1")" = 1 ] ||
    fail "the copy of pmos-admin at $1 doesn't use the test's hosts file"
  [ "$(grep -c "^readonly HOST_PREFIX=\"$PFROOT\"\$" "$1")" = 1 ] || fail "the copy of pmos-admin at $1 doesn't use the fixture host"
  copy_preflight "$(dirname "$1")"
}
export PF_LOG="$WORK/preflight.log"
# The managed block as pmos-admin writes it (docs/openshell-any-host-spec.md §3.3 step 4).
GOOD_BLOCK='# BEGIN pmos (managed by pmos-admin)
10.201.77.10 toolserver.pmos.internal
10.201.77.11 gateway.pmos.internal
10.201.77.12 bots.pmos.internal
# END pmos'
good_hosts() { printf '127.0.0.1 localhost\n::1 localhost ip6-localhost\n%s\n' "$GOOD_BLOCK" >"$WORK/hosts"; }
good_hosts

mkdir -p "$WORK/repo/deploy" "$WORK/pr"
copy_admin "$WORK/repo/deploy/pmos-admin"
: >"$WORK/repo/deploy/.env"
chmod 600 "$WORK/repo/deploy/.env"
admin=("$WORK/repo/deploy/pmos-admin")

out=$("${admin[@]}" evals-gate --checkout "$WORK/pr" --base origin/release --dry-run)
[ "$out" = "checkout=$WORK/pr"$'\n'"base=origin/release" ] || fail "--checkout/--base parse: $out"

out=$("${admin[@]}" evals-gate --dry-run)
[ "$out" = "checkout=$WORK/repo"$'\n'"base=origin/main" ] || fail "defaults: $out"

code=0
"${admin[@]}" evals-gate --bogus >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "an unknown option exits $code, not 2"
code=0
"${admin[@]}" evals-gate --checkout >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "--checkout without a value exits $code, not 2"
code=0
"${admin[@]}" evals-gate --checkout "$WORK/missing" --dry-run >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "a missing checkout exits $code, not 2"

# The review check. The lines of eval_stack_up and of the evals-gate case that name $checkout.
uses=$(awk '
  /^eval_stack_up\(\)/ { on = 1 }
  /^  evals-gate\)/ { on = 1 }
  on && /\$checkout|\$\{checkout/ { print }
  on && (/^}/ || /^    ;;/) { on = 0 }
' "$SRC" | sed 's/^ *//')
allowed=(
  'checkout=$(cd "$1" && pwd)'
  '[ -f "$checkout/content/tools.yaml" ] || { say "$checkout has no content/tools.yaml" >&2; exit 2; }'
  'cp -RP "$checkout/content" "$EVAL_DIR/content"'
  'export PMOS_EVAL_CONTENT="$EVAL_DIR/content" PMOS_EVAL_CHECKOUT="$checkout" PMOS_EVAL_BASELINE="$EVAL_DIR/baseline" PMOS_EVAL_OUT="$EVAL_DIR/out"'
  'checkout=$ROOT'
  '--checkout) [ $# -ge 2 ] || gate_usage; checkout=$2; shift 2 ;;'
  '[ -d "$checkout" ] || { say "no directory $checkout" >&2; exit 2; }'
  'checkout=$(cd "$checkout" && pwd)'
  'say "checkout=$checkout"'
  'changed=$(git -C "$checkout" diff --name-only --end-of-options "$base"...HEAD)'
  'eval_stack_up "$checkout"'
  'git -C "$checkout" archive --end-of-options "$base" evals/reports | tar -x -C "$PMOS_EVAL_BASELINE"'
)
while IFS= read -r line; do
  [ -n "$line" ] || continue
  ok=0
  for a in "${allowed[@]}"; do [ "$line" = "$a" ] && ok=1 && break; done
  [ "$ok" = 1 ] || fail "an unreviewed use of the checkout in the eval gate: $line"
done <<<"$uses"
# The eval stack's compose file mounts the checkout for the runner only, and runs images, never
# a build from it.
compose="$(dirname "$SRC")/compose.evals.yaml"
! grep -nE '^\s+build:' "$compose" || fail "compose.evals.yaml builds an image"
[ "$(grep -c 'PMOS_EVAL_CHECKOUT' "$compose")" = 1 ] || fail "compose.evals.yaml uses the checkout more than once"

# --- User assertions stay `require` on every path that (re)creates the runtime (W2) ---------
# A stand-in `docker` records the command and the assertion mode, the worker tokens and the
# env file that compose would read; nothing runs.
DEPLOY_SRC="$(dirname "$SRC")"
ROOT_SRC="$(dirname "$DEPLOY_SRC")"
mkdir -p "$WORK/bin" "$WORK/stack/deploy/systemd"
copy_admin "$WORK/stack/deploy/pmos-admin"
cp "$DEPLOY_SRC/systemd/pmos-worker@.service" "$WORK/stack/deploy/systemd/"
cat >"$WORK/bin/docker" <<'EOF'
#!/usr/bin/env bash
envfile=""
prev=""
for a in "$@"; do [ "$prev" = --env-file ] && envfile=$a; prev=$a; done
filemode=$(grep -E '^PMOS_ASSERTION_MODE=' "$envfile" 2>/dev/null | cut -d= -f2-)
printf 'mode=%s file=%s workers=%s args=%s\n' "${PMOS_ASSERTION_MODE:-}" "$filemode" \
  "${PMOS_RT_WORKER_TOKENS:-}" "$*" >>"$DOCKER_LOG"
# `docker network inspect`: the whole network as slice H2's check reads it (subnet, gateway,
# ip_range and bridge name) when the test gives one; otherwise the pmos network's gateway, when
# the test gives one.
if [ "${1:-}" = network ]; then
  if [[ "$*" == *IPRange* ]]; then
    [ -z "${FAKE_NETWORK:-}" ] || echo "$FAKE_NETWORK"
  elif [ -n "${FAKE_GATEWAY:-}" ]; then
    echo "$FAKE_GATEWAY"
  fi
fi
# `compose … ps --services`: the running services, when the test gives them.
if [[ " $* " == *" ps "*"--services"* ]] && [ -n "${FAKE_PS:-}" ]; then echo "$FAKE_PS"; fi
# `compose … config --services`: the stack's services (logs and restart check against them).
if [[ " $* " == *" config --services "* ]]; then
  printf '%s\n' ${FAKE_SERVICES:-postgres docs toolserver gateway bots slack console web worker-docs vllm-default vllm-extraction}
fi
# What reached docker through its environment, never its arguments (S1).
printf 'evals_token=%s https_proxy=%s personal_rt=%s personal_gw=%s\n' "${PMOS_EVALS_TOKEN-unset}" "${HTTPS_PROXY-unset}" \
  "${PMOS_BRINGUP_PERSONAL_RT_TOKEN-unset}" "${PMOS_BRINGUP_PERSONAL_GW_TOKEN-unset}" >>"${DOCKER_ENV_LOG:-/dev/null}"
[ "${FAKE_DOCKER_FAIL:-0}" != 1 ]
EOF
chmod +x "$WORK/bin/docker"
# A stand-in `curl`: every /readyz and /healthz answers FAKE_READYZ with 200 (for `status`), the
# gateway's /readyz FAKE_GW_READYZ when the test gives one; the runtime's task routes answer as for a finished task (for `smoke`). It records its arguments
# (CURL_LOG, one call per line), what it read with `--config -` (CURL_CONFIG_LOG) and a `-d`
# body (CURL_BODY_LOG).
cat >"$WORK/bin/curl" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"${CURL_LOG:-/dev/null}"
config=0 prev="" body=""
for a in "$@"; do
  [ "$prev" = --config ] && [ "$a" = - ] && config=1
  [ "$prev" = -d ] && body=$a
  prev=$a
done
if [ "$config" = 1 ]; then cat >>"${CURL_CONFIG_LOG:-/dev/null}"; fi
[ -z "$body" ] || printf '%s\n' "$body" >>"${CURL_BODY_LOG:-/dev/null}"
case "$*" in
  *8740/readyz*) printf '%s\n200' "${FAKE_GW_READYZ:-${FAKE_READYZ:-{\}}}" ;;
  *readyz* | *healthz*) printf '%s\n200' "${FAKE_READYZ:-{\}}" ;;
  *"-X POST http://127.0.0.1:"*"/v1/tasks "*) echo '{"id":"tsk_0abc12","status":"queued"}' ;;
  *750/v1/tasks/*/events*) echo '[{"kind":"answer","text":"see the getting started page"}]' ;;
  *750/v1/tasks/*) echo '{"id":"tsk_0abc12","status":"done"}' ;;
  *) echo '{}' ;;
esac
EOF
chmod +x "$WORK/bin/curl"
# A stand-in `getent ahostsv4 <name>`: what the host's resolver answers, from the test's hosts
# file as glibc's `files` source reads it (the first line naming it, any case, comments
# dropped), or FAKE_GETENT_ANSWER when the test gives one (a resolver consulted before the file).
cat >"$WORK/bin/getent" <<'EOF'
#!/usr/bin/env bash
[ "$1" = ahostsv4 ] && [ $# = 2 ] || exit 2
if [ -n "${FAKE_GETENT_ANSWER:-}" ]; then
  addr=$FAKE_GETENT_ANSWER
else
  addr=$(awk -v n="$2" '{ sub(/#.*/, "") } NF >= 2 && $1 ~ /^[0-9.]+$/ {
    for (i = 2; i <= NF; i++) { h = tolower($i); sub(/\.$/, "", h); if (h == tolower(n)) { print $1; exit } } }' "$GETENT_HOSTS")
fi
[ -n "$addr" ] || exit 2
# FAKE_GETENT_ANSWER may give several addresses, space-separated: each is answered.
for a in $addr; do
  for t in STREAM DGRAM RAW; do printf '%-15s %s %s\n' "$a" "$t" "$2"; done
done
EOF
chmod +x "$WORK/bin/getent"
export GETENT_HOSTS="$WORK/hosts"
# Stand-ins for the tools pmos-admin and setup.sh run on deploy/.env and its values: each records
# its arguments (ARGV_LOG, "<tool> <arguments>" per call), then runs the real one. Only on the
# PATH of the runs that check them. No secret may be among those arguments (the review of
# eb49991, T1: awk got every bot's tokens, the vault key and the database passwords).
ARGV_BIN="$WORK/argv"
mkdir -p "$ARGV_BIN"
for t in awk grep cut tr sed cat head mv cp chmod chown getfacl setfacl mktemp rm rmdir mkdir stat date \
  openssl flock sleep ls; do
  real=$(command -v "$t") || continue
  printf '#!/bin/sh\nprintf "%%s %%s\\n" %s "$*" >>"${ARGV_LOG:-/dev/null}"\nexec %s "$@"\n' "$t" "$real" >"$ARGV_BIN/$t"
done
chmod +x "$ARGV_BIN"/*
export ARGV_LOG="$WORK/argv.log"
export DOCKER_LOG="$WORK/docker.log" DOCKER_ENV_LOG="$WORK/docker-env.log"
export CURL_LOG="$WORK/curl.log" CURL_CONFIG_LOG="$WORK/curl-config.log" CURL_BODY_LOG="$WORK/curl-body.log"
stack_env() { # mode [extra lines...]
  {
    echo "PMOS_ASSERTION_MODE=$1"
    shift
    echo "PMOS_RT_WEB_TOKEN=rt-web-0123456789abcdef0123456789abcdef"
    echo "PMOS_TS_WEB_TOKEN=ts-web-0123456789abcdef0123456789abcdef"
    echo "PMOS_RT_CONSOLE_TOKEN=rt-console-0123456789abcdef0123456789ab"
    echo "PMOS_CONSOLE_TS_TOKEN=ts-console-0123456789abcdef0123456789ab"
    echo "PMOS_AK_WEB_BOTS=k1:d2ViLWJvdHM="
    echo "PMOS_AK_WEB_TOOLSERVER=k1:d2ViLXRz"
    echo "PMOS_AK_CONSOLE_BOTS=k1:Y29uc29sZS1ib3Rz"
    echo "PMOS_AK_CONSOLE_TOOLSERVER=k1:Y29uc29sZS10cw=="
    echo "PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=1"
    echo "PMOS_RT_WORKER_TOKENS="
    echo "PMOS_GW_BOT_TOKENS="
    # Slice H1: the fixture host measures private (no public address, no metadata answer), and
    # the local source has its model image named, so the preflight passes for these settings.
    echo "PMOS_EXPOSURE=private"
    echo "PMOS_VLLM_IMAGE=example.test/vllm:1"
    printf '%s\n' "$@"
  } >"$WORK/stack/deploy/.env"
  chmod 600 "$WORK/stack/deploy/.env"
  good_hosts
  good_host
  : >"$PF_LOG"
  : >"$DOCKER_LOG"
  : >"$DOCKER_ENV_LOG"
  : >"$CURL_LOG"
  : >"$CURL_CONFIG_LOG"
  : >"$CURL_BODY_LOG"
  : >"$ARGV_LOG"
}
stack=("$WORK/stack/deploy/pmos-admin")
run_stack() { PATH="$WORK/bin:$PATH" "${stack[@]}" "$@"; }

# `up --web`, then `add-bot`: both start the runtime with `require`, from deploy/.env.
stack_env require
run_stack up --web >/dev/null
grep -q 'args=compose .*--profile web up -d' "$DOCKER_LOG" || fail "up --web didn't start the web profile: $(cat "$DOCKER_LOG")"
grep -q '^mode=require file=require ' "$DOCKER_LOG" || fail "up --web without require: $(cat "$DOCKER_LOG")"
: >"$DOCKER_LOG"
run_stack add-bot scout >/dev/null
line=$(grep 'up -d bots gateway' "$DOCKER_LOG") || fail "add-bot didn't recreate the runtime"
[[ "$line" == mode=require\ file=require\ * ]] || fail "add-bot recreated the runtime without require: $line"
[[ "$line" == *"workers=worker-scout="* ]] || fail "add-bot recreated the runtime without the new worker token: $line"
grep -q '^PMOS_ASSERTION_MODE=require$' "$WORK/stack/deploy/.env" || fail "add-bot changed the mode in deploy/.env"
# Password sign-in alone is a sign-in for the web app (docs/password-sign-in.md); with neither
# it, Entra nor the development opt-in, `up --web` refuses before anything starts.
stack_env require "PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=0" "PMOS_WEB_PASSWORD_LOGIN=1" \
  "PMOS_WEB_TRUSTED_PROXIES=10.201.77.1" "PMOS_WEB_PUBLIC_URL=https://pmos.example.com"
run_stack up --web >/dev/null
grep -q 'args=compose .*--profile web up -d' "$DOCKER_LOG" || fail "up --web with password sign-in didn't start the web profile: $(cat "$DOCKER_LOG")"
# The same with the network already made at its pinned gateway.
: >"$DOCKER_LOG"
FAKE_GATEWAY=10.201.77.1 run_stack up --web >/dev/null ||
  fail "up --web at the pinned gateway was refused"
grep -q ' up -d' "$DOCKER_LOG" || fail "up --web at the pinned gateway didn't start"
# Password sign-in with no trusted proxy (P2): `up --web` refuses before anything starts and
# says exactly what to set: the pmos network's (pinned) gateway, and how to check it. Without
# an https:// public URL it refuses too.
stack_env require "PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=0" "PMOS_WEB_PASSWORD_LOGIN=1" \
  "PMOS_WEB_PUBLIC_URL=https://pmos.example.com"
code=0
out=$(FAKE_GATEWAY=10.201.77.1 run_stack up --web 2>&1) || code=$?
[ "$code" = 2 ] || fail "up --web with password sign-in and no trusted proxy exits $code, not 2"
[[ "$out" == *"PMOS_WEB_TRUSTED_PROXIES=10.201.77.1"* ]] || fail "up --web doesn't say what to set: $out"
[[ "$out" == *"docker network inspect pmos_pmos"* ]] || fail "up --web doesn't say how to find it: $out"
! grep -q ' up -d' "$DOCKER_LOG" || fail "up --web with no trusted proxy started something"
code=0
out=$(run_stack up --web 2>&1) || code=$?
[ "$code" = 2 ] || fail "up --web before the network exists exits $code, not 2"
[[ "$out" == *"PMOS_WEB_TRUSTED_PROXIES=10.201.77.1"* && "$out" == *"docker network inspect pmos_pmos"* ]] ||
  fail "up --web before the network exists doesn't say the pinned address: $out"
# M1 (the re-check of eb24d28): a wrong or stale setting is refused with the exact fix.
# - A trusted address that isn't the gateway (an old, unpinned one): refused, naming the line.
stack_env require "PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=0" "PMOS_WEB_PASSWORD_LOGIN=1" \
  "PMOS_WEB_TRUSTED_PROXIES=172.18.0.1" "PMOS_WEB_PUBLIC_URL=https://pmos.example.com"
code=0
out=$(run_stack up --web 2>&1) || code=$?
[ "$code" = 2 ] || fail "up --web with a trusted address that isn't the gateway exits $code, not 2"
[[ "$out" == *"names 172.18.0.1"* && "$out" == *"PMOS_WEB_TRUSTED_PROXIES=10.201.77.1"* ]] ||
  fail "up --web doesn't give the fix for a wrong trusted address: $out"
! grep -q ' up -d' "$DOCKER_LOG" || fail "up --web with a wrong trusted address started something"
# - A network made before its gateway was pinned: refused, saying to re-create it.
stack_env require "PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=0" "PMOS_WEB_PASSWORD_LOGIN=1" \
  "PMOS_WEB_TRUSTED_PROXIES=10.201.77.1" "PMOS_WEB_PUBLIC_URL=https://pmos.example.com"
code=0
out=$(FAKE_GATEWAY=172.18.0.1 run_stack up --web 2>&1) || code=$?
[ "$code" = 2 ] || fail "up --web on a stale network exits $code, not 2"
[[ "$out" == *"gateway is 172.18.0.1"* && "$out" == *"PMOS_NETWORK_GATEWAY=10.201.77.1"* &&
  "$out" == *"deploy/pmos-admin down && deploy/pmos-admin up --web"* ]] ||
  fail "up --web on a stale network doesn't say to re-create it: $out"
! grep -q ' up -d' "$DOCKER_LOG" || fail "up --web on a stale network started something"
# - A proxy in a container on the pmos network (inside the pinned subnet): refused by default
#   (GL4: Docker assigns those addresses), naming the gateway and the override; with
#   PMOS_WEB_TRUST_PROXY_IN_NETWORK=1 it starts, with a warning. A site that overrides the
#   subnet and gateway starts.
stack_env require "PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=0" "PMOS_WEB_PASSWORD_LOGIN=1" \
  "PMOS_WEB_TRUSTED_PROXIES=10.201.77.40" "PMOS_WEB_PUBLIC_URL=https://pmos.example.com"
code=0
out=$(FAKE_GATEWAY=10.201.77.1 run_stack up --web 2>&1) || code=$?
[ "$code" = 2 ] || fail "a proxy inside the pinned subnet without the override exits $code, not 2"
[[ "$out" == *"PMOS_WEB_TRUSTED_PROXIES=10.201.77.1"* && "$out" == *"PMOS_WEB_TRUST_PROXY_IN_NETWORK=1"* ]] ||
  fail "a proxy inside the subnet isn't told the fix: $out"
! grep -q ' up -d' "$DOCKER_LOG" || fail "a proxy inside the subnet without the override started something"
stack_env require "PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=0" "PMOS_WEB_PASSWORD_LOGIN=1" \
  "PMOS_WEB_TRUSTED_PROXIES=10.201.77.40" "PMOS_WEB_PUBLIC_URL=https://pmos.example.com" \
  "PMOS_WEB_TRUST_PROXY_IN_NETWORK=1"
out=$(FAKE_GATEWAY=10.201.77.1 run_stack up --web 2>&1) || fail "a proxy inside the subnet with the override was refused: $out"
[[ "$out" == *"WARNING: PMOS_WEB_TRUSTED_PROXIES names 10.201.77.40"* ]] || fail "the override isn't warned about: $out"
grep -q ' up -d' "$DOCKER_LOG" || fail "the override didn't start the web app"
# The override never trusts an address outside the subnet.
stack_env require "PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=0" "PMOS_WEB_PASSWORD_LOGIN=1" \
  "PMOS_WEB_TRUSTED_PROXIES=172.18.0.1" "PMOS_WEB_PUBLIC_URL=https://pmos.example.com" \
  "PMOS_WEB_TRUST_PROXY_IN_NETWORK=1"
code=0
run_stack up --web >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "the override let an address outside the subnet through ($code)"
# `status` shows a readiness warning from the web app (GL3).
stack_env require "PMOS_WEB_PASSWORD_LOGIN=1" "PMOS_WEB_TRUSTED_PROXIES=10.201.77.1"
out=$(FAKE_PS=web FAKE_READYZ='{"checks":{},"ready":true,"untrusted_proxy_refusals":3,"warnings":["untrusted_proxy_refusals"]}' run_stack status 2>&1)
[[ "$out" == *"WARN"*"untrusted_proxy_refusals"* && "$out" == *"PMOS_WEB_TRUSTED_PROXIES"* ]] ||
  fail "status doesn't show the web app's warning: $out"
out=$(FAKE_PS=web FAKE_READYZ='{"checks":{},"ready":true,"warnings":[]}' run_stack status 2>&1)
[[ "$out" != *"WARN"* ]] || fail "status warns with no warning: $out"
# `status` reads the gateway's /readyz as it is now (labels only, keyed by role; G2 and G8b): the
# stub and a gateway that isn't production-class each get one WARN line; a production one none.
gw_up='{"class":"production","cloud_test":false,"ready":true,"running":0,"stub":false,"upstreams":{"default":{"status":"ok","upstream":"local"}},"waiting":0}'
out=$(FAKE_GW_READYZ="$gw_up" run_stack status 2>&1)
[[ "$out" != *"WARN"* ]] || fail "status warns about a production gateway: $out"
[[ "$out" == *"gateway"*"ok"*'"class":"production"'* ]] || fail "status doesn't show the gateway ready: $out"
out=$(FAKE_GW_READYZ='{"class":"test","ready":true,"running":0,"stub":true,"waiting":0}' run_stack status 2>&1)
[[ "$out" == *"gateway"*"WARN"*"STUB model answers every call"* ]] || fail "status doesn't warn about the stub: $out"
[[ "$out" != *"not a production-class gateway"* ]] || fail "status warns twice about the stub: $out"
out=$(FAKE_GW_READYZ="${gw_up/production/test}" run_stack status 2>&1)
[[ "$out" == *"gateway"*"WARN"*"not a production-class gateway"* ]] || fail "status doesn't warn about a test-class gateway: $out"
[[ "$out" != *"STUB"* ]] || fail "status calls a test-class gateway the stub: $out"
# The published ports (first deployment run, 2026-11-23: a test stack on the same host already
# listened on 8710 to 8770): `status` and `smoke` follow PMOS_*_PORT; a value that isn't a port
# number stops pmos-admin.
stack_env require "PMOS_TOOLSERVER_PORT=18710" "PMOS_GATEWAY_PORT=18740" "PMOS_BOTS_PORT=18750" \
  "PMOS_WEB_PORT=18770" "PMOS_RT_SLACK_TOKEN=rt-slack-0123456789abcdef0123456789ab"
FAKE_PS=web run_stack status >/dev/null 2>&1 || fail "status with other ports failed"
for u in 127.0.0.1:18710/readyz 127.0.0.1:18740/readyz 127.0.0.1:18750/readyz 127.0.0.1:18770/readyz; do
  grep -q "$u" "$CURL_LOG" || fail "status didn't call $u: $(cat "$CURL_LOG")"
done
! grep -qE '127\.0\.0\.1:87[0-9]0' "$CURL_LOG" || fail "status still called a default port: $(cat "$CURL_LOG")"
: >"$CURL_LOG"
run_stack smoke ana >/dev/null 2>&1 || fail "smoke with another runtime port failed"
grep -q '127.0.0.1:18750/v1/tasks' "$CURL_LOG" || fail "smoke didn't use PMOS_BOTS_PORT: $(cat "$CURL_LOG")"
! grep -q '127.0.0.1:8750' "$CURL_LOG" || fail "smoke still called 8750"
for bad in 0 70000 87x0 "8750 8751"; do
  stack_env require "PMOS_BOTS_PORT=$bad"
  code=0
  run_stack status >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "PMOS_BOTS_PORT=$bad exits $code, not 2"
done
# PMOS_MODEL_SOURCE=hosted-test (the gateway's part of H4): the overlay is added to every compose
# call, the gateway's test config is written outside the repo with every role behind the label,
# live connectors and a missing key are refused, Slack stays off, and a run-time CA is taken
# only with hosted-test.
mkdir -p "$WORK/state"
ht=("PMOS_STATE_DIR=$WORK/state" "PMOS_MODEL_SOURCE=hosted-test"
  "PMOS_HOSTED_TEST_URL=https://models.example.test/v1/" "PMOS_HOSTED_TEST_MODEL=example-model-1"
  "PMOS_GATEWAY_REMOTE_KEY=proxy-injected")
stack_env require "${ht[@]}" "PMOS_SLACK_APP_TOKEN=xapp-test" "PMOS_SLACK_BOT_TOKEN=xoxb-test"
out=$(run_stack up --web 2>&1) || fail "up with hosted-test failed: $out"
grep -q 'args=compose .*-f [^ ]*/compose.yaml -f [^ ]*/compose.hosted-test.yaml .* up -d' "$DOCKER_LOG" ||
  fail "up with hosted-test didn't add the overlay: $(cat "$DOCKER_LOG")"
! grep -q 'compose.gateway-ca.yaml' "$DOCKER_LOG" || fail "the CA overlay was added without PMOS_GATEWAY_CA_FILE"
! grep -q -- '--profile slack' "$DOCKER_LOG" || fail "hosted-test started the Slack bot"
[[ "$out" == *"Slack bot stays off"* ]] || fail "up doesn't say why Slack is off: $out"
cfg="$WORK/state/gateway.hosted-test.yaml"
[ -f "$cfg" ] || fail "up with hosted-test wrote no gateway config"
grep -qx '  remote: "openai+https://models.example.test/v1"' "$cfg" || fail "the upstream isn't the URL: $(cat "$cfg")"
[ "$(grep -c 'model: "example-model-1", label: "hosted-model-a"' "$cfg")" = 3 ] || fail "not every role has the label: $(cat "$cfg")"
! grep -q '^class:' "$cfg" || fail "the hosted-test config sets a class (it must be a test gateway)"
! grep -q 'proxy-injected' "$cfg" || fail "the key reached the gateway config"
: >"$DOCKER_LOG"
run_stack status >/dev/null 2>&1
grep -q 'compose.hosted-test.yaml .* ps' "$DOCKER_LOG" || fail "status doesn't see the hosted-test stack: $(cat "$DOCKER_LOG")"
for broken in "PMOS_CONNECTORS=live" "PMOS_GATEWAY_REMOTE_KEY=" "PMOS_HOSTED_TEST_URL=http://models.example.test/v1" \
  "PMOS_HOSTED_TEST_MODEL=" "PMOS_HOSTED_TEST_LABEL=bad label" "PMOS_GATEWAY_CA_FILE=relative/ca.crt"; do
  stack_env require "${ht[@]}" "$broken"
  code=0
  run_stack up >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "hosted-test with $broken exits $code, not 2"
  ! grep -q ' up -d' "$DOCKER_LOG" || fail "hosted-test with $broken started something"
done
printf 'fake CA\n' >"$WORK/ca.crt"
stack_env require "${ht[@]}" "PMOS_GATEWAY_CA_FILE=$WORK/ca.crt"
run_stack up >/dev/null 2>&1 || fail "hosted-test with a CA file failed"
grep -q 'compose.hosted-test.yaml -f [^ ]*/compose.gateway-ca.yaml .* up -d' "$DOCKER_LOG" ||
  fail "the CA overlay wasn't added: $(cat "$DOCKER_LOG")"
stack_env require "PMOS_STATE_DIR=$WORK/state" "PMOS_GATEWAY_CA_FILE=$WORK/ca.crt"
code=0
run_stack up >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "a run-time CA with the local source exits $code, not 2"
for src in hosted bogus; do
  stack_env require "PMOS_MODEL_SOURCE=$src"
  code=0
  run_stack up >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "PMOS_MODEL_SOURCE=$src exits $code, not 2"
done
stack_env require "${ht[@]}"
code=0
run_stack up --models >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "up --models with hosted-test exits $code, not 2"
# The bring-up personal worker (first deployment run: no OpenShell, so no worker for a PM's own
# bot): `up --bringup` with PMOS_BRINGUP_PERSONAL_BOT starts it with the bot's two tokens, found
# by exact key and passed through docker's environment, never its arguments; without the tokens
# or with a bad name, nothing starts.
stack_env require "PMOS_BRINGUP_PERSONAL_BOT=pm-ana" \
  "PMOS_RT_WORKER_TOKENS=worker-pm-ana2=rt-other-0123,worker-pm-ana=rt-ana-0123456789abcdef" \
  "PMOS_GW_BOT_TOKENS=bot-pm-ana=gw-ana-0123456789abcdef,bot-pm-an=gw-other-0123"
run_stack up --bringup >/dev/null 2>&1 || fail "up --bringup with a personal bot failed"
grep -q -- '--profile bringup --profile bringup-personal up -d' "$DOCKER_LOG" ||
  fail "up --bringup didn't start the personal worker: $(cat "$DOCKER_LOG")"
grep -q 'personal_rt=rt-ana-0123456789abcdef personal_gw=gw-ana-0123456789abcdef' "$DOCKER_ENV_LOG" ||
  fail "the personal worker's tokens didn't reach docker's environment: $(cat "$DOCKER_ENV_LOG")"
! sed 's/.* args=//' "$DOCKER_LOG" | grep -q 'rt-ana-0123456789abcdef\|gw-ana-0123456789abcdef' ||
  fail "a personal worker token reached docker's arguments"
for bad in "PMOS_BRINGUP_PERSONAL_BOT=pm-bob" "PMOS_BRINGUP_PERSONAL_BOT=docs" "PMOS_BRINGUP_PERSONAL_BOT=Pm_Ana"; do
  stack_env require "$bad" "PMOS_RT_WORKER_TOKENS=worker-pm-ana=rt-ana-0123" "PMOS_GW_BOT_TOKENS=bot-pm-ana=gw-ana-0123"
  code=0
  run_stack up --bringup >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "up --bringup with $bad exits $code, not 2"
  ! grep -q ' up -d' "$DOCKER_LOG" || fail "up --bringup with $bad started something"
done
stack_env require
run_stack up --bringup >/dev/null 2>&1 || fail "up --bringup without a personal bot failed"
! grep -q 'bringup-personal' "$DOCKER_LOG" || fail "the personal worker started without PMOS_BRINGUP_PERSONAL_BOT"
# An overridden network that still holds the pinned addresses (slice H2: a /25 with its own
# ip_range and gateway) starts; one that doesn't is refused in H2's checks below.
stack_env require "PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=0" "PMOS_WEB_PASSWORD_LOGIN=1" \
  "PMOS_NETWORK_SUBNET=10.201.77.0/25" "PMOS_NETWORK_GATEWAY=10.201.77.126" "PMOS_NETWORK_IP_RANGE=10.201.77.64/26" \
  "PMOS_WEB_TRUSTED_PROXIES=10.201.77.126" "PMOS_WEB_PUBLIC_URL=https://pmos.example.com"
FAKE_GATEWAY=10.201.77.126 run_stack up --web >/dev/null || fail "an overridden network was refused"
stack_env require "PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=0" "PMOS_WEB_PASSWORD_LOGIN=1" \
  "PMOS_WEB_TRUSTED_PROXIES=10.201.77.1" "PMOS_WEB_PUBLIC_URL=http://pmos.example.com"
code=0
out=$(run_stack up --web 2>&1) || code=$?
[ "$code" = 2 ] || fail "up --web with password sign-in and an http public URL exits $code, not 2"
[[ "$out" == *"https://"* ]] || fail "up --web doesn't ask for https: $out"
! grep -q ' up -d' "$DOCKER_LOG" || fail "up --web with an http public URL started something"
stack_env require "PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=0"
code=0
run_stack up --web >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "up --web with no sign-in exits $code, not 2"
[ ! -s "$DOCKER_LOG" ] || fail "up --web with no sign-in called docker"
# A blank mode means `require` once a web or console token is set (the services' default).
stack_env ""
out=$(run_stack up 2>&1)
[[ "$out" == *"user assertions: require"* ]] || fail "a blank mode with web tokens isn't reported as require: $out"

# `off` or `verify` with a web or console token refuses before anything starts or is written.
for mode in off verify; do
  stack_env "$mode"
  for cmd in "up" "up --web" "add-bot scout"; do
    code=0
    # shellcheck disable=SC2086
    run_stack $cmd >/dev/null 2>&1 || code=$?
    [ "$code" = 2 ] || fail "$cmd with PMOS_ASSERTION_MODE=$mode exits $code, not 2"
    [ ! -s "$DOCKER_LOG" ] || fail "$cmd with PMOS_ASSERTION_MODE=$mode called docker: $(cat "$DOCKER_LOG")"
  done
  grep -q '^PMOS_RT_WORKER_TOKENS=$' "$WORK/stack/deploy/.env" || fail "a refused add-bot wrote tokens"
done
# ... and the development override doesn't change that here (R3): it is for fake-data
# development stacks only, and production compose doesn't pass it.
stack_env off PMOS_ASSERTION_INSECURE_OFF=1
code=0
out=$(run_stack up 2>&1) || code=$?
[ "$code" = 2 ] || fail "up with PMOS_ASSERTION_MODE=off and the override exits $code, not 2"
[[ "$out" == *"fake-data"* ]] || fail "the refused override isn't explained: $out"
[ ! -s "$DOCKER_LOG" ] || fail "up with the override called docker: $(cat "$DOCKER_LOG")"
# A client set up without its keys is refused (nothing half-configured).
stack_env require
sed -i '/^PMOS_AK_WEB_BOTS=/d' "$WORK/stack/deploy/.env"
code=0
run_stack up >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "a web token without its key exits $code, not 2"
code=0
run_stack up --web >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "up --web without its key exits $code, not 2"
# The restore path checks before it stops anything.
stack_env off
mkdir -p "$WORK/backup"
(cd "$WORK/backup" && echo dump >pmos_bots.dump && sha256sum pmos_bots.dump >SHA256SUMS)
code=0
run_stack restore "$WORK/backup" </dev/null >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "restore with PMOS_ASSERTION_MODE=off exits $code, not 2"
[ ! -s "$DOCKER_LOG" ] || fail "a refused restore called docker"

# --- users: thin wrappers over `pmos-toolserver users …` (docs/runbooks/users.md) ----------
# Without a database: the arguments the wrapper checks, the OS user it passes as
# PMOS_HOST_USER (refused before docker when it can't be recorded), the import file's
# read-only mount. The tool server's own refusals are tested in crates/toolserver.
stack_env require
for args in "" "drop-table" "import --apply" "import --from"; do
  code=0
  # shellcheck disable=SC2086
  run_stack users $args </dev/null >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "users $args exits $code, not 2"
done
code=0
run_stack users import --from "$WORK/missing.yaml" </dev/null >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "users import of a missing file exits $code, not 2"
[ ! -s "$DOCKER_LOG" ] || fail "a refused users command called docker: $(cat "$DOCKER_LOG")"
# The OS user who ran it: SUDO_USER first, else id -un.
me=$(id -un)
run_stack users list </dev/null >/dev/null
grep -qF -- "--profile cli run --rm --no-deps -e PMOS_HOST_USER=$me -T toolserver-cli pmos-toolserver users list" "$DOCKER_LOG" ||
  fail "users list didn't pass PMOS_HOST_USER=$me: $(cat "$DOCKER_LOG")"
: >"$DOCKER_LOG"
# A password setup code (docs/password-sign-in.md): the host's own command, with its host user.
run_stack users password-setup raj </dev/null >/dev/null
grep -qF -- "-e PMOS_HOST_USER=$me -T toolserver-cli pmos-toolserver users password-setup raj" "$DOCKER_LOG" ||
  fail "users password-setup didn't run with PMOS_HOST_USER: $(cat "$DOCKER_LOG")"
SUDO_USER=alice run_stack users deactivate ana </dev/null >/dev/null
grep -qF -- "-e PMOS_HOST_USER=alice -T toolserver-cli pmos-toolserver users deactivate ana" "$DOCKER_LOG" ||
  fail "users deactivate didn't pass SUDO_USER as PMOS_HOST_USER: $(cat "$DOCKER_LOG")"
# A name that can't be recorded: refused, docker never called.
for bad in "Bad User" "ROOT" "x;y"; do
  : >"$DOCKER_LOG"
  code=0
  SUDO_USER=$bad run_stack users list </dev/null >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "users with the OS user '$bad' exits $code, not 2"
  [ ! -s "$DOCKER_LOG" ] || fail "users with the OS user '$bad' called docker"
done
# import: the named file, mounted read-only; the other arguments passed through.
printf 'members: []\n' >"$WORK/old-members.yaml"
: >"$DOCKER_LOG"
run_stack users import --commit abc1234 --from "$WORK/old-members.yaml" --bootstrap --apply </dev/null >/dev/null
grep -qF -- "-v $WORK/old-members.yaml:/run/pmos-import/members.yaml:ro toolserver-cli pmos-toolserver users import --commit abc1234 --bootstrap --apply --from /run/pmos-import/members.yaml" "$DOCKER_LOG" ||
  fail "users import didn't mount the file read-only: $(cat "$DOCKER_LOG")"
# The tenant for --oid comes from the web app's Entra settings.
stack_env require PMOS_WEB_ENTRA_TENANT_ID=11111111-2222-3333-4444-555555555555
run_stack users set-entra raj --oid aaaaaaaa-0000-0000-0000-000000000001 </dev/null >/dev/null
grep -qF -- "-e PMOS_ENTRA_TENANT_ID=11111111-2222-3333-4444-555555555555" "$DOCKER_LOG" ||
  fail "users set-entra didn't pass the tenant: $(cat "$DOCKER_LOG")"
# Break glass works whatever the assertion mode: a one-off container starts no service.
stack_env off
run_stack users grant-admin ana --keep-entra </dev/null >/dev/null ||
  fail "users grant-admin was refused over the assertion mode"
grep -qF -- "pmos-toolserver users grant-admin ana --keep-entra" "$DOCKER_LOG" ||
  fail "users grant-admin didn't run: $(cat "$DOCKER_LOG")"

# --- deploy/.env is data, never shell (slice H0) ---------------------------------------------
# A value holding a command substitution or a backtick isn't run, a quoted value loses its
# quotes, an unquoted one its ` # comment`; a line that isn't an assignment stops the script.
stack_env require "PMOS_PWN=\$(touch $WORK/pwned)" "PMOS_PWN2=\`touch $WORK/pwned2\`" \
  "export PMOS_PWN3=x; touch $WORK/pwned3"
run_stack up >/dev/null 2>&1 || fail "up with odd values in deploy/.env failed"
[ ! -e "$WORK/pwned" ] && [ ! -e "$WORK/pwned2" ] && [ ! -e "$WORK/pwned3" ] ||
  fail "a value in deploy/.env was run as shell"
grep -q '^mode=require ' "$DOCKER_LOG" || fail "up didn't read the mode: $(cat "$DOCKER_LOG")"
stack_env require
sed -i 's/^PMOS_ASSERTION_MODE=.*/PMOS_ASSERTION_MODE="require"  # set by setup.sh/' "$WORK/stack/deploy/.env"
run_stack up >/dev/null || fail "up with a quoted, commented mode failed"
grep -q '^mode=require ' "$DOCKER_LOG" || fail "a quoted value kept its quotes or comment: $(cat "$DOCKER_LOG")"
stack_env require "PMOS_ASSERTION_MODE=require # set by setup.sh"
run_stack up >/dev/null || fail "up with an unquoted, commented mode failed"
grep -q '^mode=require ' "$DOCKER_LOG" || fail "an unquoted value kept its comment: $(cat "$DOCKER_LOG")"
stack_env require "touch $WORK/pwned4"
code=0
run_stack up >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "a line that isn't KEY=VALUE exits $code, not 2"
[ ! -e "$WORK/pwned4" ] && [ ! -s "$DOCKER_LOG" ] || fail "a line that isn't KEY=VALUE ran or started something"

# --- The containers never run as root (the review of eb49991, T3) ----------------------------
# PMOS_UID or PMOS_GID of 0 (a root-owned checkout), or not a number: every command that starts
# a container refuses before docker runs; `down` warns and stops the stack; `pause` never reads
# the file. A real id, or none (the images' default), starts.
for bad in PMOS_UID=0 PMOS_GID=0 PMOS_UID=00 PMOS_UID=abc PMOS_UID=-1 "PMOS_GID=10 01" PMOS_UID=12345678901 \
  PMOS_UID=+0 PMOS_UID=0x0 "PMOS_UID= 0" PMOS_UID=root; do
  stack_env require "$bad"
  for cmd in up "up --web" "add-bot scout" "evals run" "users list" lint "restart bots"; do
    code=0
    # shellcheck disable=SC2086
    out=$(run_stack $cmd </dev/null 2>&1) || code=$?
    [ "$code" = 2 ] || fail "$cmd with $bad exits $code, not 2"
    [[ "$out" == *"${bad%%=*}="* ]] || fail "$cmd with $bad doesn't name the setting: $out"
  done
  [ ! -s "$DOCKER_LOG" ] || fail "a command with $bad called docker: $(cat "$DOCKER_LOG")"
  grep -q '^PMOS_RT_WORKER_TOKENS=$' "$WORK/stack/deploy/.env" || fail "add-bot with $bad wrote tokens"
  out=$(run_stack down 2>&1) || fail "down with $bad failed: $out"
  [[ "$out" == *"WARNING: ${bad%%=*}="* ]] || fail "down with $bad didn't warn: $out"
  grep -q ' down$' "$DOCKER_LOG" || fail "down with $bad didn't stop the stack"
done
stack_env require PMOS_UID=0
out=$(run_stack up 2>&1) || true
[[ "$out" == *"would run as root"* && "$out" == *"chown -R pmos:pmos"* ]] || fail "PMOS_UID=0 isn't explained: $out"
# The same from the caller's environment (Compose prefers it to deploy/.env).
stack_env require
code=0
PMOS_UID=0 run_stack up >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] && [ ! -s "$DOCKER_LOG" ] || fail "PMOS_UID=0 from the environment wasn't refused ($code)"
# The commands that start no container as that user (status, logs, backup, reindex, revoke) work
# with a uid or gid of 0 or a name left by an older install, so an incident isn't blocked; `up`
# still refuses (above). The five reach docker or curl, so the run is checked, not just the exit.
for bad in PMOS_UID=0 PMOS_GID=0 PMOS_UID=root; do
  stack_env require "PMOS_SERVICE_TOKEN=svc0-0123456789abcdef0123456789abcdef" "PMOS_STATE_DIR=$WORK/state-uid" "$bad"
  for cmd in "revoke ana slack" status "logs bots" reindex backup; do
    code=0
    # shellcheck disable=SC2086
    out=$(run_stack $cmd </dev/null 2>&1) || code=$?
    [ "$code" = 0 ] || fail "$cmd with $bad exits $code, not 0: $out"
    [[ "$out" != *"would run as root"* && "$out" != *"isn't a number"* ]] || fail "$cmd with $bad was held up by the uid check: $out"
  done
  grep -q -- '-X DELETE http://127.0.0.1:8710/internal/credentials/ana/slack ' "$CURL_LOG" || fail "revoke with $bad didn't call the route"
  code=0
  run_stack up >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "up with $bad exits $code, not 2"
done
rm -rf "$WORK/stack/deploy/backups"
stack_env require PMOS_UID=1000 PMOS_GID=1000
run_stack up >/dev/null || fail "up with PMOS_UID=1000 was refused"
grep -q ' up -d' "$DOCKER_LOG" || fail "up with PMOS_UID=1000 didn't start"

# --- The eval gate's base is a revision, never an option (slice H0) --------------------------
for base in --output=x -x ""; do
  code=0
  "${admin[@]}" evals-gate --base "$base" --dry-run >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "evals-gate --base '$base' exits $code, not 2"
done

# --- setup.sh writes only plain paths into deploy/.env (slice H0) ----------------------------
mkdir -p "$WORK/setup/deploy"
cp "$DEPLOY_SRC/setup.sh" "$WORK/setup/deploy/setup.sh"
copy_preflight "$WORK/setup/deploy"
for args in "--content /srv/x\$(touch $WORK/pwned5)" "--content relative/path" "--state /srv/a b" \
  "--state /srv/x\`id\`" "--content /srv/x#y" "--clone --upload-pack=touch"; do
  code=0
  # shellcheck disable=SC2086
  bash "$WORK/setup/deploy/setup.sh" $args >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "setup.sh $args exits $code, not 2"
  [ ! -e "$WORK/setup/deploy/.env" ] || fail "setup.sh $args wrote deploy/.env"
done
code=0
bash "$WORK/setup/deploy/setup.sh" --content $'/srv/a\nPMOS_X=1' >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "setup.sh with a newline in --content exits $code, not 2"
[ ! -e "$WORK/pwned5" ] || fail "setup.sh ran a path as shell"
# A clone URL with a user or token in it would be on git's command line: refused (S1).
for url in "https://x-access-token:tok@github.com/o/r.git" "https://user@example.com/r.git"; do
  code=0
  bash "$WORK/setup/deploy/setup.sh" --clone "$url" >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "setup.sh --clone $url exits $code, not 2"
done
# A whole run, with the stand-in docker: deploy/.env made 0600 and filled; a value set back to
# `generated` is filled again by a rename (a new file), not rewritten in place; nothing left over.
cp "$DEPLOY_SRC/env.example" "$WORK/setup/deploy/env.example"
mkdir -p "$WORK/setup-content/.git" "$WORK/setup-content/content"
: >"$WORK/setup-content/content/tools.yaml"
# A root-owned checkout is refused (T3, below): when these checks run as root, it gets another owner.
if [ "$(id -u)" = 0 ]; then chown -R 4321:4321 "$WORK/setup-content"; fi
# With the argument-recording stand-ins (ARGV_LOG) first on its PATH.
setup_run() { PATH="$ARGV_BIN:$WORK/bin:$PATH" bash "$WORK/setup/deploy/setup.sh" --content "$WORK/setup-content" --state "$WORK/setup-state"; }
: >"$ARGV_LOG"
setup_run >/dev/null 2>&1 || fail "setup.sh failed: $(setup_run 2>&1 | tail -5)"
senv="$WORK/setup/deploy/.env"
# Slice H1: setup.sh ran the preflight's install stage first, before deploy/.env existed.
grep -qx -- "--stage install --env-file $WORK/setup/deploy/.env" "$PF_LOG" || fail "setup.sh didn't run the install stage: $(cat "$PF_LOG")"
[ "$(stat -c %a "$senv")" = 600 ] || fail "setup.sh's deploy/.env isn't 0600"
# No secret it wrote reached a command line (T1): the vault key, the database passwords, every
# token and assertion key went to awk (a value env.example has as `generated`) or printf only.
grep -q '^awk .*\$1==k{print k, ' "$ARGV_LOG" || fail "setup.sh's awk didn't run through the stand-in"
setup_secrets_not_in_args() {
  local line k v n=0
  while IFS= read -r line; do
    k=${line%%=*}
    v=${line#*=}
    case "$k" in *TOKEN | *PASSWORD | *SECRET | *_KEY | PMOS_AK_*) ;; *) continue ;; esac
    [ -n "$v" ] && [ "$v" != generated ] || continue
    n=$((n + 1))
    ! grep -qF -- "$v" "$ARGV_LOG" || fail "setup.sh put $k on a command line"
  done <"$senv"
  [ "$n" -ge 20 ] || fail "setup.sh wrote only $n secrets"
}
setup_secrets_not_in_args
grep -qE '^PMOS_SERVICE_TOKEN=[0-9a-f]{48}$' "$senv" || fail "setup.sh didn't fill PMOS_SERVICE_TOKEN"
grep -qx "PMOS_CONTENT_CHECKOUT=$WORK/setup-content" "$senv" || fail "setup.sh didn't write the checkout"
kept=$(grep '^PMOS_VAULT_KEY=' "$senv")
sed -i 's/^PMOS_SERVICE_TOKEN=.*/PMOS_SERVICE_TOKEN=generated/' "$senv"
# A hard link to the old file: a rename leaves it as it was (and deploy/.env with one link);
# a rewrite in place would change it. (An inode number can be reused, so it isn't compared.)
ln "$senv" "$WORK/setup-env.old"
cp "$senv" "$WORK/setup-env.copy"
setup_run >/dev/null 2>&1 || fail "setup.sh failed the second time"
cmp -s "$WORK/setup-env.old" "$WORK/setup-env.copy" && [ "$(stat -c %h "$senv")" = 1 ] ||
  fail "setup.sh rewrote deploy/.env in place, not by a rename"
grep -qE '^PMOS_SERVICE_TOKEN=[0-9a-f]{48}$' "$senv" || fail "setup.sh didn't fill a generated value again"
[ "$(grep '^PMOS_VAULT_KEY=' "$senv")" = "$kept" ] || fail "setup.sh replaced a real value"
[ "$(stat -c %a "$senv")" = 600 ] || fail "setup.sh's deploy/.env isn't 0600 after a fill"
[ -z "$(find "$WORK/setup/deploy" -maxdepth 1 -name '.env.??????')" ] || fail "setup.sh left a temporary file"
setup_secrets_not_in_args
# Stopped (a kill, Ctrl-C) while the new deploy/.env is written: the new file is removed and
# deploy/.env is as it was (the review of eb49991). The stand-in mv sends TERM to setup.sh.
mkdir -p "$WORK/killmv"
printf '#!/bin/sh\nkill -TERM "$PPID"\nexit 1\n' >"$WORK/killmv/mv"
chmod +x "$WORK/killmv/mv"
sed -i 's/^PMOS_SERVICE_TOKEN=.*/PMOS_SERVICE_TOKEN=generated/' "$senv"
cp "$senv" "$WORK/setup-env.before"
code=0
PATH="$WORK/killmv:$WORK/bin:$PATH" bash "$WORK/setup/deploy/setup.sh" --content "$WORK/setup-content" \
  --state "$WORK/setup-state" >/dev/null 2>&1 || code=$?
[ "$code" = 143 ] || fail "setup.sh stopped by TERM exits $code, not 143"
cmp -s "$WORK/setup-env.before" "$senv" || fail "setup.sh stopped by TERM changed deploy/.env"
[ -z "$(find "$WORK/setup/deploy" -maxdepth 1 -name '.env.??????')" ] || fail "setup.sh stopped by TERM left its new deploy/.env"
setup_run >/dev/null 2>&1 || fail "setup.sh failed after a stopped run"
# A root-owned checkout (T3): refused before the state directories or deploy/.env are made. The
# stand-in stat gives the checkout's owner as root (uid, or gid).
mkdir -p "$WORK/rootstat" "$WORK/setup2/deploy"
cp "$DEPLOY_SRC/setup.sh" "$DEPLOY_SRC/env.example" "$WORK/setup2/deploy/"
copy_preflight "$WORK/setup2/deploy"
cat >"$WORK/rootstat/stat" <<EOF
#!/bin/sh
case "\$1 \$2 \$3" in
  "-c %u $WORK/setup-content") echo "\${FAKE_OWNER_UID:-4321}"; exit 0 ;;
  "-c %g $WORK/setup-content") echo "\${FAKE_OWNER_GID:-4321}"; exit 0 ;;
esac
exec $(command -v stat) "\$@"
EOF
chmod +x "$WORK/rootstat/stat"
for owner in FAKE_OWNER_UID=0 FAKE_OWNER_GID=0; do
  code=0
  out=$(env "$owner" PATH="$WORK/rootstat:$WORK/bin:$PATH" bash "$WORK/setup2/deploy/setup.sh" \
    --content "$WORK/setup-content" --state "$WORK/setup2-state" 2>&1) || code=$?
  [ "$code" = 1 ] && [[ "$out" == *"is owned by root"* && "$out" == *"chown -R pmos:pmos"* ]] ||
    fail "setup.sh with a root-owned checkout ($owner) wasn't refused ($code): $out"
  [ ! -e "$WORK/setup2/deploy/.env" ] && [ ! -e "$WORK/setup2-state" ] ||
    fail "setup.sh with a root-owned checkout ($owner) wrote deploy/.env or the state"
done
# Slice H1: a host that fails the preflight (a 5.15 kernel) stops setup.sh first, before the
# state directories or deploy/.env are made.
cp "$PF_FIXTURE/../kernel-5.15/cmd/uname-r" "$PFROOT/cmd/uname-r"
code=0
out=$(PATH="$WORK/bin:$PATH" bash "$WORK/setup2/deploy/setup.sh" --content "$WORK/setup-content" --state "$WORK/setup2-state" 2>&1) || code=$?
[ "$code" = 1 ] && [[ "$out" == *"H-KERNEL"*"fail"* && "$out" == *"setup.sh stops"* ]] ||
  fail "setup.sh on a failing host wasn't stopped ($code): $out"
[ ! -e "$WORK/setup2/deploy/.env" ] && [ ! -e "$WORK/setup2-state" ] || fail "setup.sh on a failing host wrote deploy/.env or the state"
good_host
# A PMOS_UID or PMOS_GID of 0 (or not a number) kept in deploy/.env from an earlier run: refused.
cp "$senv" "$WORK/setup-env.good"
for bad in PMOS_UID=0 PMOS_GID=0 PMOS_UID=000 PMOS_UID=x1 'PMOS_UID="0"'; do
  sed -i "s/^${bad%%=*}=.*/$bad/" "$senv"
  code=0
  out=$(setup_run 2>&1) || code=$?
  [ "$code" = 1 ] && [[ "$out" == *"${bad%%=*}="* ]] || fail "setup.sh kept $bad ($code): $out"
  cp "$WORK/setup-env.good" "$senv"
done
setup_run >/dev/null 2>&1 || fail "setup.sh failed after the PMOS_UID checks"

# --- Bot names, token lookup, `--from` and the worker unit (slice H0) ------------------------
# Stand-ins for openshell, systemctl, loginctl and id (only on this PATH, so `users` above still
# sees the real id) record what they were asked; nothing runs. systemd-escape is the real one.
command -v systemd-escape >/dev/null || fail "systemd-escape is needed for these checks (CI's runner has it)"
command -v systemd-analyze >/dev/null || fail "systemd-analyze is needed for these checks (CI's runner has it)"
SBIN="$WORK/sbin"
mkdir -p "$SBIN"
export OPENSHELL_LOG="$WORK/openshell.log" SYSTEMCTL_LOG="$WORK/systemctl.log"
cat >"$SBIN/openshell" <<'EOF'
#!/usr/bin/env bash
printf 'args=%s worker=%s gateway=%s\n' "$*" "${PMOS_WORKER_TOKEN:-}" "${PMOS_GATEWAY_TOKEN:-}" >>"$OPENSHELL_LOG"
# `provider get`: a provider exists only when the test says so (FAKE_PROVIDERS_EXIST=1).
case "$*" in "provider get "*) [ "${FAKE_PROVIDERS_EXIST:-0}" = 1 ] || exit 1 ;; esac
[ "${FAKE_OPENSHELL_FAIL:-0}" != 1 ]
EOF
cat >"$SBIN/systemctl" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$SYSTEMCTL_LOG"
case "$*" in
  "is-enabled pmos-metadata-block.timer") echo "${FAKE_TIMER_ENABLED:-enabled}"; [ "${FAKE_TIMER_ENABLED:-enabled}" = enabled ]; exit ;;
  "is-active pmos-metadata-block.timer") echo "${FAKE_TIMER_ACTIVE:-active}"; [ "${FAKE_TIMER_ACTIVE:-active}" = active ]; exit ;;
esac
[ "${FAKE_SYSTEMCTL_FAIL:-0}" != 1 ]
EOF
cat >"$SBIN/loginctl" <<'EOF'
#!/usr/bin/env bash
[ "$*" = "show-user pmosop -p Linger --value" ] && echo "${FAKE_LINGER-yes}"
EOF
cat >"$SBIN/id" <<'EOF'
#!/usr/bin/env bash
case "$*" in -u) real=$(/usr/bin/id -u); [ "$real" != 0 ] || real=1000; echo "${FAKE_UID:-$real}" ;; -un) echo pmosop ;; *) exec /usr/bin/id "$@" ;; esac
EOF
chmod +x "$SBIN"/*
export XDG_CONFIG_HOME="$WORK/xdg"
UNIT="$XDG_CONFIG_HOME/systemd/user/pmos-worker@.service"
checkout="$WORK/content-checkout"
mkdir -p "$checkout/policies/bots"
for b in docs pm-ana pm abcdefghijklmnopqrstuvwxyz-12345; do
  cp "$ROOT_SRC/policies/bots/docs.yaml" "$checkout/policies/bots/$b.yaml"
  cp "$ROOT_SRC/policies/bots/docs.sandbox.yaml" "$checkout/policies/bots/$b.sandbox.yaml"
done
bot_env() { # extra lines...
  stack_env require "PMOS_CONTENT_CHECKOUT=$checkout" "PMOS_RT_DOCS_WORKER_TOKEN=rt-docs-token" \
    "PMOS_GW_DOCS_BOT_TOKEN=gw-docs-token" "$@"
  : >"$OPENSHELL_LOG"
  : >"$SYSTEMCTL_LOG"
  rm -rf "$XDG_CONFIG_HOME"
}
run_bot() { PATH="$SBIN:$WORK/bin:$PATH" "${stack[@]}" "$@"; }

# Refused names: shell metacharacters, path separators, a leading dash, a newline, the
# characters that add or override a token entry, uppercase, non-ASCII, too long; exit 2 with
# deploy/.env, openshell, systemctl and docker untouched.
bad_names=("../x" "a/b" "a b" "x\$(touch $WORK/pwned6)" "x\`id\`" "x;id" "x|id" "x&" "x>y" $'a\nb'
  "x=y" "a,b" "-a" "--dry-run-x" "a-" "a--b" "A" "Docs" "é" "a.b" "a_b" "" "x*" "x?" "[a]"
  "abcdefghijklmnopqrstuvwxyz-123456")
bot_env
cp "$WORK/stack/deploy/.env" "$WORK/env.before"
for name in "${bad_names[@]}"; do
  for cmd in add-bot sandbox pause resume; do
    code=0
    run_bot "$cmd" "$name" >/dev/null 2>&1 || code=$?
    [ "$code" = 2 ] || fail "$cmd $(printf '%q' "$name") exits $code, not 2"
  done
  code=0
  run_bot sandbox "$name" --dry-run >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "sandbox $(printf '%q' "$name") --dry-run exits $code, not 2"
done
cmp -s "$WORK/env.before" "$WORK/stack/deploy/.env" || fail "a refused name changed deploy/.env"
[ ! -s "$DOCKER_LOG" ] && [ ! -s "$OPENSHELL_LOG" ] && [ ! -s "$SYSTEMCTL_LOG" ] ||
  fail "a refused name reached docker, openshell or systemctl"
[ ! -e "$WORK/pwned6" ] && [ ! -e "$UNIT" ] || fail "a refused name ran or installed something"
# Valid names pass: the Docs bot, a personal bot, one of exactly 32 characters.
run_bot add-bot docs >/dev/null || fail "add-bot docs was refused"
run_bot add-bot pm-ana >/dev/null || fail "add-bot pm-ana was refused"
run_bot add-bot abcdefghijklmnopqrstuvwxyz-12345 >/dev/null || fail "a 32-character name was refused"
grep -qE '^PMOS_RT_WORKER_TOKENS=worker-pm-ana=[0-9a-f]{48},worker-abcdefghijklmnopqrstuvwxyz-12345=[0-9a-f]{48}$' \
  "$WORK/stack/deploy/.env" || fail "add-bot didn't write the worker tokens: $(grep WORKER "$WORK/stack/deploy/.env")"
grep -qE '^PMOS_GW_BOT_TOKENS=bot-pm-ana=[0-9a-f]{48},bot-abcdefghijklmnopqrstuvwxyz-12345=[0-9a-f]{48}$' \
  "$WORK/stack/deploy/.env" || fail "add-bot didn't write the gateway tokens"
out=$(run_bot add-bot pm-ana) && [[ "$out" == *"already has tokens"* ]] || fail "add-bot pm-ana twice: $out"

# The lookup is an exact match of the whole key: `pm` is neither `pm-ana` nor `xpm`, and a
# similar key before the right one is never taken.
bot_env "PMOS_RT_WORKER_TOKENS=worker-xpm-ana=rt-wrong1,worker-pm-ana-2=rt-wrong2,worker-pm-ana=rt-right" \
  "PMOS_GW_BOT_TOKENS=bot-pm-ana-2=gw-wrong2,bot-pm-ana=gw-right,bot-pm-anaa=gw-wrong3"
run_bot sandbox pm-ana >/dev/null || fail "sandbox pm-ana failed: $(cat "$OPENSHELL_LOG")"
grep -q 'provider create --name pmos-pm-ana-worker .* worker=rt-right gateway=$' "$OPENSHELL_LOG" ||
  fail "the worker provider didn't get the exact token: $(cat "$OPENSHELL_LOG")"
grep -q 'provider create --name pmos-pm-ana-gateway .* worker= gateway=gw-right$' "$OPENSHELL_LOG" ||
  fail "the gateway provider didn't get the exact token: $(cat "$OPENSHELL_LOG")"
bot_env "PMOS_RT_WORKER_TOKENS=worker-pm-ana=rt-1" "PMOS_GW_BOT_TOKENS=bot-pm-ana=gw-1"
code=0
out=$(run_bot sandbox pm 2>&1) || code=$?
[ "$code" = 1 ] && [[ "$out" == *"add-bot pm first"* ]] || fail "sandbox pm took pm-ana's tokens ($code): $out"
[ ! -s "$OPENSHELL_LOG" ] || fail "sandbox pm without tokens called openshell"
run_bot add-bot pm >/dev/null
grep -q '^PMOS_RT_WORKER_TOKENS=worker-pm-ana=rt-1,worker-pm=' "$WORK/stack/deploy/.env" ||
  fail "add-bot pm thought pm-ana's token was its own"

# `sandbox`: `--from`, no `&`, the unit installed and started, tokens nowhere but the providers.
bot_env "PMOS_RT_WORKER_TOKENS=worker-pm-ana=rt-secret-1" "PMOS_GW_BOT_TOKENS=bot-pm-ana=gw-secret-1"
out=$(run_bot sandbox pm-ana --dry-run) || fail "sandbox --dry-run failed"
[[ "$out" == *"openshell sandbox create --name pmos-pm-ana --from pmos-worker:local --policy $checkout/policies/bots/pm-ana.yaml --provider pmos-pm-ana-gateway --provider pmos-pm-ana-worker --upload "* ]] ||
  fail "sandbox --dry-run doesn't create with --from: $out"
[[ "$out" != *"--image"* && "$out" != *"&"* && "$out" != *secret* ]] ||
  fail "sandbox --dry-run has --image, an & or a token: $out"
[[ "$out" == *'systemctl --user enable --now pmos-worker@pm\x2dana.service'* ]] ||
  fail "sandbox --dry-run doesn't start the systemd-escaped unit: $out"
[ ! -s "$OPENSHELL_LOG" ] && [ ! -s "$SYSTEMCTL_LOG" ] && [ ! -e "$UNIT" ] || fail "sandbox --dry-run ran something"
run_bot sandbox pm-ana >/dev/null || fail "sandbox pm-ana failed"
grep -q '^args=sandbox create --name pmos-pm-ana --from pmos-worker:local ' "$OPENSHELL_LOG" ||
  fail "sandbox didn't create with --from: $(cat "$OPENSHELL_LOG")"
! grep -q -- '--image\|sandbox exec' "$OPENSHELL_LOG" || fail "sandbox used --image or ran sandbox exec itself"
[ "$(grep -c '^args=sandbox create .* worker= gateway=$' "$OPENSHELL_LOG")" = 1 ] ||
  fail "sandbox create got a token in its environment"
[ "$(cat "$SYSTEMCTL_LOG")" = $'--user daemon-reload\n--user enable --now pmos-worker@pm\\x2dana.service' ] ||
  fail "sandbox didn't start the unit: $(cat "$SYSTEMCTL_LOG")"
# The installed unit: the template with only openshell's path changed.
[ -f "$UNIT" ] || fail "sandbox didn't install the unit"
[ "$(diff "$DEPLOY_SRC/systemd/pmos-worker@.service" "$UNIT" | grep -c '^[<>]')" = 4 ] ||
  fail "the installed unit differs from the template in more than the ExecStart and ExecStartPre lines"
# Slice H1: the quick preflight before every start, by the checkout's path, openshell's directory
# on its PATH; before ExecStart.
grep -qxF "ExecStartPre=/usr/bin/env PATH=$SBIN:/usr/local/bin:/usr/bin:/bin $WORK/stack/deploy/preflight.sh --stage quick" "$UNIT" ||
  fail "the unit's ExecStartPre isn't the quick preflight: $(grep ExecStartPre "$UNIT")"
[ "$(grep -n '^ExecStartPre=' "$UNIT" | cut -d: -f1)" -lt "$(grep -n '^ExecStart=' "$UNIT" | cut -d: -f1)" ] ||
  fail "ExecStartPre comes after ExecStart"
grep -qxF 'ExecStartPre=/opt/pmos/deploy/preflight.sh --stage quick' "$DEPLOY_SRC/systemd/pmos-worker@.service" ||
  fail "the template has no ExecStartPre placeholder"
grep -qxF "ExecStart=$SBIN/openshell sandbox exec -n pmos-%I --workdir /sandbox/workspace -- \\" "$UNIT" ||
  fail "the unit's ExecStart isn't openshell's path and sandbox exec: $(grep ExecStart "$UNIT")"
for want in 'PMOS_WORKER_BOT=%I' '/usr/local/bin/node /opt/pmos/harness/dist/main.js' 'Restart=on-failure' \
  'RestartSec=10' 'StartLimitIntervalSec=600' 'StartLimitBurst=5' 'After=openshell-gateway.service' \
  'Wants=openshell-gateway.service' 'WantedBy=default.target' 'Type=exec'; do
  grep -qF -- "$want" "$UNIT" || fail "the unit has no $want"
done
! grep -v '^#' "$UNIT" | grep -q '%i' || fail "the unit uses %i (escaped) instead of %I"
! grep -qE 'secret|TOKEN=|&$' "$UNIT" || fail "the unit holds a token or backgrounds the worker"
# systemd itself accepts it, as an instance for pm-ana.
cp "$UNIT" "$WORK/pmos-worker@pm\\x2dana.service"
verify=$(systemd-analyze verify "$WORK/pmos-worker@pm\\x2dana.service" 2>&1) ||
  fail "systemd-analyze verify refused the unit: $verify"
# It also reports on the host's own units (a CI runner's warn about options systemd dropped), so
# only what it says about this unit counts.
verify=$(printf '%s\n' "$verify" | grep -F 'pmos-worker@' || true)
[ -z "$verify" ] || fail "systemd-analyze verify warned about the unit: $verify"
# The Docs bot uses its built-in tokens.
bot_env
run_bot sandbox docs >/dev/null || fail "sandbox docs failed"
grep -q 'provider create --name pmos-docs-worker .* worker=rt-docs-token ' "$OPENSHELL_LOG" ||
  fail "sandbox docs didn't use the built-in worker token"
grep -qF -- '--user enable --now pmos-worker@docs.service' "$SYSTEMCTL_LOG" || fail "sandbox docs didn't start its unit"

# Lingering off, or running as root: refused before any sandbox, provider or unit is made.
for case in "FAKE_LINGER=no" "FAKE_LINGER=" "FAKE_UID=0"; do
  bot_env "PMOS_RT_WORKER_TOKENS=worker-pm-ana=rt-1" "PMOS_GW_BOT_TOKENS=bot-pm-ana=gw-1"
  for cmd in sandbox resume; do
    code=0
    out=$(env "$case" PATH="$SBIN:$WORK/bin:$PATH" "${stack[@]}" "$cmd" pm-ana 2>&1) || code=$?
    [ "$code" = 1 ] || fail "$cmd with $case exits $code, not 1"
    [ ! -s "$OPENSHELL_LOG" ] && [ ! -s "$SYSTEMCTL_LOG" ] && [ ! -e "$UNIT" ] ||
      fail "$cmd with $case created something"
    if [ "$case" != FAKE_UID=0 ]; then
      [[ "$out" == *"sudo loginctl enable-linger pmosop"* ]] || fail "$cmd with $case doesn't say how to turn lingering on: $out"
    fi
  done
done

# pause disables and stops the unit, then the sandbox (even when systemctl fails); resume starts
# the sandbox, reinstalls the unit, clears a start-limit failure and starts it.
bot_env
run_bot resume pm-ana >/dev/null || fail "resume failed"
[ "$(cat "$OPENSHELL_LOG")" = "args=sandbox start -n pmos-pm-ana worker= gateway=" ] ||
  fail "resume didn't start the sandbox: $(cat "$OPENSHELL_LOG")"
[ "$(cat "$SYSTEMCTL_LOG")" = $'--user daemon-reload\n--user reset-failed pmos-worker@pm\\x2dana.service\n--user enable --now pmos-worker@pm\\x2dana.service' ] ||
  fail "resume didn't start the unit: $(cat "$SYSTEMCTL_LOG")"
[ -f "$UNIT" ] || fail "resume didn't install the unit"
: >"$OPENSHELL_LOG"
: >"$SYSTEMCTL_LOG"
run_bot pause pm-ana >/dev/null || fail "pause failed"
[ "$(cat "$SYSTEMCTL_LOG")" = '--user disable --now pmos-worker@pm\x2dana.service' ] ||
  fail "pause didn't stop the unit: $(cat "$SYSTEMCTL_LOG")"
[ "$(cat "$OPENSHELL_LOG")" = "args=sandbox stop -n pmos-pm-ana worker= gateway=" ] || fail "pause didn't stop the sandbox"
# When systemctl fails, the sandbox is still stopped, and pause says what is left and exits 1
# (S2: it used to exit 0, so a script calling it couldn't tell).
: >"$OPENSHELL_LOG"
code=0
out=$(FAKE_SYSTEMCTL_FAIL=1 run_bot pause pm-ana 2>&1) || code=$?
[ "$code" = 1 ] || fail "pause exits $code, not 1, when systemctl failed"
[ "$(cat "$OPENSHELL_LOG")" = "args=sandbox stop -n pmos-pm-ana worker= gateway=" ] ||
  fail "pause didn't stop the sandbox when systemctl failed"
[[ "$out" == *"NOT complete"* && "$out" == *"systemctl --user disable --now 'pmos-worker@pm\x2dana.service'"* ]] ||
  fail "pause doesn't say what is left when systemctl failed: $out"

# --- pause in an incident (the security review of 6bb806f, S2 and S3) -------------------------
# As root (sudo): refused before anything is stopped, with the way to do it as the right user.
bot_env
run_bot resume pm-ana >/dev/null
: >"$OPENSHELL_LOG"
: >"$SYSTEMCTL_LOG"
code=0
out=$(FAKE_UID=0 run_bot pause pm-ana 2>&1) || code=$?
[ "$code" = 1 ] || fail "pause as root exits $code, not 1"
[[ "$out" == *"not as root"* && "$out" == *"openshell sandbox stop -n pmos-pm-ana"* ]] ||
  fail "pause as root doesn't say how to pause: $out"
# ... naming the real unit, not a placeholder (the review of eb49991).
[[ "$out" == *"systemctl --user disable --now 'pmos-worker@pm\x2dana.service'"* && "$out" != *"<systemd-escape"* ]] ||
  fail "pause as root doesn't name the unit: $out"
[ ! -s "$OPENSHELL_LOG" ] && [ ! -s "$SYSTEMCTL_LOG" ] || fail "pause as root stopped something"
# No unit template under this caller's XDG_CONFIG_HOME: the unit is disabled and stopped all the
# same (the user's systemd may have it from elsewhere; the review of eb49991), and the sandbox.
rm -rf "$XDG_CONFIG_HOME"
out=$(run_bot pause pm-ana 2>&1) || fail "pause with the unit installed elsewhere failed: $out"
[ "$(cat "$SYSTEMCTL_LOG")" = '--user disable --now pmos-worker@pm\x2dana.service' ] ||
  fail "pause without the template under XDG_CONFIG_HOME didn't try the unit: $(cat "$SYSTEMCTL_LOG")"
[ "$(cat "$OPENSHELL_LOG")" = "args=sandbox stop -n pmos-pm-ana worker= gateway=" ] ||
  fail "pause without the template under XDG_CONFIG_HOME didn't stop the sandbox"
# No unit installed at all (systemctl fails, as it does for a unit that doesn't exist): said
# plainly, the sandbox stopped all the same, exit 1.
: >"$OPENSHELL_LOG"
: >"$SYSTEMCTL_LOG"
code=0
out=$(FAKE_SYSTEMCTL_FAIL=1 run_bot pause pm-ana 2>&1) || code=$?
[ "$code" = 1 ] || fail "pause with no unit installed exits $code, not 1"
[[ "$out" == *"no worker unit is installed for pmosop"* && "$out" == *"pgrep -af 'openshell sandbox exec -n pmos-pm-ana'"* ]] ||
  fail "pause with no unit doesn't say so: $out"
[ "$(cat "$OPENSHELL_LOG")" = "args=sandbox stop -n pmos-pm-ana worker= gateway=" ] ||
  fail "pause with no unit didn't stop the sandbox"
[ "$(cat "$SYSTEMCTL_LOG")" = '--user disable --now pmos-worker@pm\x2dana.service' ] ||
  fail "pause with no unit didn't try the unit: $(cat "$SYSTEMCTL_LOG")"
# A deploy/.env the caller can't read (root's, mode 0600, after `sudo deploy/setup.sh`; the
# review of eb49991, T2): pause never reads it, so it still stops the unit and the sandbox. Root
# reads every file, so as root this runs pause as nobody (setpriv), with what it needs opened up.
bot_env
run_bot resume pm-ana >/dev/null
: >"$OPENSHELL_LOG"
: >"$SYSTEMCTL_LOG"
envf="$WORK/stack/deploy/.env"
code=0
if [ "$(/usr/bin/id -u)" != 0 ]; then
  chmod 000 "$envf"
  out=$(run_bot pause pm-ana 2>&1) || code=$?
  chmod 600 "$envf"
elif command -v setpriv >/dev/null; then
  chmod 711 "$WORK" "$WORK/stack" "$WORK/stack/deploy"
  chmod 755 "$SBIN"
  chmod 666 "$OPENSHELL_LOG" "$SYSTEMCTL_LOG"
  out=$(PATH="$SBIN:$WORK/bin:$PATH" setpriv --reuid=65534 --regid=65534 --clear-groups "${stack[@]}" pause pm-ana 2>&1) || code=$?
  chmod 700 "$WORK" "$WORK/stack" "$WORK/stack/deploy" "$SBIN"
else
  fail "these checks run as root and setpriv isn't installed to run pause as another user"
fi
[ "$code" = 0 ] || fail "pause with an unreadable deploy/.env exits $code: $out"
[ "$(cat "$SYSTEMCTL_LOG")" = '--user disable --now pmos-worker@pm\x2dana.service' ] ||
  fail "pause with an unreadable deploy/.env didn't stop the unit: $(cat "$SYSTEMCTL_LOG")"
[ "$(cat "$OPENSHELL_LOG")" = "args=sandbox stop -n pmos-pm-ana worker= gateway=" ] ||
  fail "pause with an unreadable deploy/.env didn't stop the sandbox: $(cat "$OPENSHELL_LOG")"
# The sandbox can't be stopped: said, exit 1, after the unit was stopped.
run_bot resume pm-ana >/dev/null
: >"$SYSTEMCTL_LOG"
code=0
out=$(FAKE_OPENSHELL_FAIL=1 run_bot pause pm-ana 2>&1) || code=$?
[ "$code" = 1 ] && [[ "$out" == *"pmos-pm-ana was NOT stopped"* ]] || fail "a failed sandbox stop isn't reported ($code): $out"
grep -qF -- '--user disable --now pmos-worker@pm\x2dana.service' "$SYSTEMCTL_LOG" || fail "pause didn't stop the unit first"
# A deploy/.env that can't be parsed, or is readable by everyone, or is missing: pause and down
# warn and go on (they need nothing pmos-admin reads from it); every other command stops.
for broken in "not an assignment" "MODE644" "MISSING"; do
  bot_env
  run_bot resume pm-ana >/dev/null
  case "$broken" in
    MODE644) chmod 644 "$WORK/stack/deploy/.env" ;;
    MISSING) mv "$WORK/stack/deploy/.env" "$WORK/env.aside" ;;
    *) echo "$broken" >>"$WORK/stack/deploy/.env" ;;
  esac
  : >"$OPENSHELL_LOG"
  : >"$DOCKER_LOG"
  out=$(run_bot pause pm-ana 2>&1) || fail "pause with deploy/.env $broken failed: $out"
  if [ "$broken" = "not an assignment" ]; then
    # pause never reads the file (T2), so it has no line to complain about.
    [[ "$out" != *"isn't KEY=VALUE"* ]] || fail "pause read deploy/.env: $out"
  else
    [[ "$out" == *WARNING* ]] || fail "pause with deploy/.env $broken didn't warn: $out"
  fi
  [ "$(cat "$OPENSHELL_LOG")" = "args=sandbox stop -n pmos-pm-ana worker= gateway=" ] ||
    fail "pause with deploy/.env $broken didn't stop the sandbox"
  if [ "$broken" != MISSING ]; then
    out=$(run_bot down 2>&1) || fail "down with deploy/.env $broken failed: $out"
    [[ "$out" == *WARNING* ]] || fail "down with deploy/.env $broken didn't warn: $out"
    grep -q ' down$' "$DOCKER_LOG" || fail "down with deploy/.env $broken didn't run compose down"
    : >"$DOCKER_LOG"
    for cmd in up "logs bots" "restart bots" "add-bot scout" "sandbox pm-ana"; do
      code=0
      # shellcheck disable=SC2086
      run_bot $cmd >/dev/null 2>&1 || code=$?
      [ "$code" = 2 ] || fail "$cmd with deploy/.env $broken exits $code, not 2"
    done
    [ ! -s "$DOCKER_LOG" ] || fail "a command refused over deploy/.env $broken called docker"
  else
    # down with no deploy/.env at all: the manual advice, exit 1, nothing read.
    code=0
    out=$(run_bot down 2>&1) || code=$?
    [ "$code" = 1 ] && [[ "$out" == *"docker ps -q --filter label=com.docker.compose.project=pmos | xargs -r docker stop"* ]] ||
      fail "down with no deploy/.env exits $code, or gives no manual advice: $out"
    [[ "$out" != *"No such file"* && "$out" != *"Permission denied"* ]] || fail "down with no deploy/.env died on a shell error: $out"
    [ ! -s "$DOCKER_LOG" ] || fail "down with no deploy/.env called docker"
    mv "$WORK/env.aside" "$WORK/stack/deploy/.env"
  fi
done
# down with a deploy/.env the caller can't read (root's, 0600, after a `sudo deploy/setup.sh`):
# it doesn't die reading it, and when Compose can't read it either, prints the manual advice.
if [ "$(/usr/bin/id -u)" != 0 ]; then
  bot_env
  chmod 000 "$WORK/stack/deploy/.env"
  code=0
  out=$(FAKE_DOCKER_FAIL=1 run_bot down 2>&1) || code=$?
  chmod 600 "$WORK/stack/deploy/.env"
elif command -v setpriv >/dev/null; then
  bot_env
  chmod 711 "$WORK" "$WORK/stack" "$WORK/stack/deploy"
  chmod 755 "$SBIN" "$WORK/bin"
  chmod 666 "$DOCKER_LOG" "$DOCKER_ENV_LOG"
  code=0
  out=$(FAKE_DOCKER_FAIL=1 PATH="$SBIN:$WORK/bin:$PATH" setpriv --reuid=65534 --regid=65534 --clear-groups "${stack[@]}" down 2>&1) || code=$?
  chmod 700 "$WORK" "$WORK/stack" "$WORK/stack/deploy" "$SBIN" "$WORK/bin"
else
  fail "these checks run as root and setpriv isn't installed to run down as another user"
fi
[ "$code" = 1 ] && [[ "$out" == *"docker ps -q --filter label=com.docker.compose.project=pmos | xargs -r docker stop"* ]] ||
  fail "down with an unreadable deploy/.env exits $code, or gives no manual advice: $out"
[[ "$out" != *"Permission denied"* ]] || fail "down with an unreadable deploy/.env died on a shell error: $out"
grep -q ' down$' "$DOCKER_LOG" || fail "down with an unreadable deploy/.env didn't try compose down"
# down when compose itself can't: says how to stop the containers directly.
bot_env
code=0
out=$(FAKE_DOCKER_FAIL=1 run_bot down 2>&1) || code=$?
[ "$code" = 1 ] && [[ "$out" == *"label=com.docker.compose.project=pmos"* ]] || fail "down's fallback isn't given ($code): $out"

# --- deploy/.env's mode and owner (the security review of 6bb806f) ----------------------------
# Group-writable: refused. Group-readable (as the eval runner's ACL shows): warned. Owned by
# another account: warned. 0600 and yours: silent.
bot_env
chmod 660 "$WORK/stack/deploy/.env"
code=0
out=$(run_bot up 2>&1) || code=$?
[ "$code" = 2 ] && [[ "$out" == *"written by its group"* ]] || fail "a group-writable deploy/.env isn't refused ($code): $out"
[ ! -s "$DOCKER_LOG" ] || fail "up with a group-writable deploy/.env called docker"
chmod 640 "$WORK/stack/deploy/.env"
out=$(run_bot up 2>&1) || fail "up with a group-readable deploy/.env was refused: $out"
[[ "$out" == *"WARNING: deploy/.env (mode 640) is readable by its group"* ]] || fail "group read isn't warned about: $out"
chmod 600 "$WORK/stack/deploy/.env"
out=$(FAKE_UID=4242 run_bot up 2>&1) || fail "up with another owner was refused: $out"
[[ "$out" == *"not by you (uid 4242)"* ]] || fail "another owner isn't warned about: $out"
out=$(PATH="$WORK/bin:$PATH" "${stack[@]}" up 2>&1) || fail "up with a private deploy/.env failed: $out"
[[ "$out" != *WARNING* ]] || fail "a private deploy/.env of the caller's is warned about: $out"

# --- No token on a command line (the security review of 6bb806f, S1) --------------------------
# Distinctive tokens; the stand-in curl and docker record their arguments, and must never see
# one there. curl gets them on its standard input (--config -), docker in its environment.
tok_env() {
  bot_env "PMOS_SERVICE_TOKEN=svc0SECRET0123456789abcdef0123456789abcdef" \
    "PMOS_RT_SLACK_TOKEN=slk0SECRET0123456789abcdef0123456789abcdef" \
    "PMOS_RT_EVALS_TOKEN=evl0SECRET0123456789abcdef0123456789abcdef" \
    "HTTPS_PROXY=http://proxyuser:prx0SECRET@proxy.example:3128" "$@"
}
no_secret_in_args() { # what ran: no SECRET in the arguments curl, docker, openshell and the
  # recorded tools (awk, grep, …: ARGV_LOG) got (the docker and openshell logs also record some
  # of their environment, which may hold one)
  local args
  args=$(cat "$CURL_LOG" "$ARGV_LOG"; sed 's/^.* args=//' "$DOCKER_LOG"; sed 's/^args=//; s/ worker=.*$//' "$OPENSHELL_LOG")
  [[ "$args" != *SECRET* ]] || fail "$1 put a token on a command line: $(grep SECRET <<<"$args")"
}
tok_env
run_stack reindex >/dev/null || fail "reindex failed"
grep -qx -- '--config - -fsS --noproxy \* -X POST http://127.0.0.1:8710/internal/reindex -H content-type: application/json' "$CURL_LOG" ||
  fail "reindex didn't call curl with --config -: $(cat "$CURL_LOG")"
grep -qxF 'header = "Authorization: Bearer svc0SECRET0123456789abcdef0123456789abcdef"' "$CURL_CONFIG_LOG" ||
  fail "reindex didn't give curl the service token on stdin"
no_secret_in_args reindex
run_stack revoke ana slack >/dev/null || fail "revoke ana slack failed"
grep -q -- '-X DELETE http://127.0.0.1:8710/internal/credentials/ana/slack ' "$CURL_LOG" || fail "revoke didn't call the route: $(cat "$CURL_LOG")"
no_secret_in_args revoke
out=$(run_stack smoke ana.b 2>&1) || fail "smoke failed: $out"
[[ "$out" == *"smoke test passed"* ]] || fail "smoke didn't pass: $out"
[ "$(grep -c '^header = "Authorization: Bearer slk0SECRET0123456789abcdef0123456789abcdef"$' "$CURL_CONFIG_LOG")" = 3 ] ||
  fail "smoke didn't give every curl the Slack runtime token on stdin: $(sed 's/SECRET.*/…/' "$CURL_CONFIG_LOG")"
[ "$(grep -c '^header = "X-Pmos-User: ana.b"$' "$CURL_CONFIG_LOG")" = 3 ] || fail "smoke didn't send X-Pmos-User"
python3 -I -c 'import json, sys
b = json.loads(open(sys.argv[1]).read())
assert b == {"bot": "docs", "requested_by": "ana.b", "text": "What does the docs site say about getting started?", "source": "slack", "slack": {"channel": "smoke", "thread_ts": "0.0"}}, b' "$CURL_BODY_LOG" ||
  fail "smoke's body isn't the task's JSON: $(cat "$CURL_BODY_LOG")"
no_secret_in_args smoke
run_stack evals run --category docs-qa >/dev/null || fail "evals failed"
grep -q -- '-e PMOS_EVALS_TOKEN -e PMOS_EVALS_ROOT=evals ' "$DOCKER_LOG" || fail "evals didn't pass the token by name: $(cat "$DOCKER_LOG")"
grep -qx 'evals_token=evl0SECRET0123456789abcdef0123456789abcdef https_proxy=.*' "$DOCKER_ENV_LOG" ||
  fail "evals didn't give docker the token in its environment"
no_secret_in_args evals
: >"$DOCKER_LOG"
: >"$DOCKER_ENV_LOG"
run_stack build >/dev/null || fail "build failed"
grep -q -- '--build-arg HTTPS_PROXY --build-arg NO_PROXY ' "$DOCKER_LOG" || fail "build didn't pass the proxy by name: $(cat "$DOCKER_LOG")"
grep -q 'https_proxy=http://proxyuser:prx0SECRET@proxy.example:3128 personal_rt=' "$DOCKER_ENV_LOG" || fail "build didn't give docker the proxy in its environment"
no_secret_in_args build
# sandbox: the provider tokens only in openshell's environment; an existing provider (one that
# survived `sandbox delete`, or a rotated token) is updated, not created again.
tok_env "PMOS_RT_WORKER_TOKENS=worker-pm-ana=rt0SECRET1" "PMOS_GW_BOT_TOKENS=bot-pm-ana=gw0SECRET1"
FAKE_PROVIDERS_EXIST=1 run_bot sandbox pm-ana >/dev/null || fail "sandbox with existing providers failed"
grep -qx 'args=provider update pmos-pm-ana-gateway --credential PMOS_GATEWAY_TOKEN worker= gateway=gw0SECRET1' "$OPENSHELL_LOG" ||
  fail "an existing gateway provider wasn't updated: $(cat "$OPENSHELL_LOG")"
grep -qx 'args=provider update pmos-pm-ana-worker --credential PMOS_WORKER_TOKEN worker=rt0SECRET1 gateway=' "$OPENSHELL_LOG" ||
  fail "an existing worker provider wasn't updated"
! grep -q 'provider create' "$OPENSHELL_LOG" || fail "an existing provider was created again"
no_secret_in_args "sandbox (update)"
: >"$OPENSHELL_LOG"
rm -rf "$XDG_CONFIG_HOME"
run_bot sandbox pm-ana >/dev/null || fail "sandbox with new providers failed"
grep -qx 'args=provider create --name pmos-pm-ana-gateway --type pmos-gateway --credential PMOS_GATEWAY_TOKEN worker= gateway=gw0SECRET1' "$OPENSHELL_LOG" ||
  fail "a missing provider wasn't created: $(cat "$OPENSHELL_LOG")"
no_secret_in_args sandbox
# add-bot: the whole token lists (every bot's tokens) and the new tokens reach awk through its
# environment, never its arguments (the review of eb49991, T1).
tok_env "PMOS_RT_WORKER_TOKENS=worker-pm-ana=rt0SECRET1" "PMOS_GW_BOT_TOKENS=bot-pm-ana=gw0SECRET1"
PATH="$ARGV_BIN:$SBIN:$WORK/bin:$PATH" "${stack[@]}" add-bot scout >/dev/null || fail "add-bot scout failed"
grep -q '^awk .*\$1==k{print k, ' "$ARGV_LOG" || fail "add-bot's awk didn't run through the stand-in"
grep -qE '^PMOS_RT_WORKER_TOKENS=worker-pm-ana=rt0SECRET1,worker-scout=[0-9a-f]{48}$' "$WORK/stack/deploy/.env" &&
  grep -qE '^PMOS_GW_BOT_TOKENS=bot-pm-ana=gw0SECRET1,bot-scout=[0-9a-f]{48}$' "$WORK/stack/deploy/.env" ||
  fail "add-bot didn't keep the old tokens and add the new ones"
no_secret_in_args add-bot
for k in PMOS_RT_WORKER_TOKENS PMOS_GW_BOT_TOKENS; do
  # (stack_env's deploy/.env has each list twice, blank then set: set_env rewrites both lines.)
  new=$(sed -n "s/^$k=.*-scout=//p" "$WORK/stack/deploy/.env" | head -1)
  [ ${#new} = 48 ] && ! grep -qF -- "$new" "$ARGV_LOG" || fail "add-bot put scout's new token from $k on a command line"
done
# A token curl's config can't hold as it is: refused before curl runs. A quote, a backslash or a
# carriage return in deploy/.env; a line feed from the caller's environment (deploy/.env's lines
# can't hold one).
for bad in 'abc"def' 'abc\def' 'abc\' $'abc\rdef'; do
  tok_env "PMOS_SERVICE_TOKEN=$bad"
  code=0
  run_stack reindex >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] && [ ! -s "$CURL_LOG" ] || fail "a token $(printf '%q' "$bad") reached curl ($code)"
  tok_env "PMOS_RT_SLACK_TOKEN=$bad"
  code=0
  run_stack smoke ana >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] && [ ! -s "$CURL_LOG" ] || fail "a Slack token $(printf '%q' "$bad") reached curl ($code)"
done
bot_env
for bad in $'abc\ndef' $'abc\rdef' 'abc\def'; do
  : >"$CURL_LOG"
  code=0
  PMOS_SERVICE_TOKEN=$bad run_stack reindex >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] && [ ! -s "$CURL_LOG" ] || fail "a token $(printf '%q' "$bad") from the environment reached curl ($code)"
  code=0
  PMOS_RT_SLACK_TOKEN=$bad run_stack smoke ana >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] && [ ! -s "$CURL_LOG" ] || fail "a Slack token $(printf '%q' "$bad") from the environment reached curl ($code)"
done

# --- revoke, smoke, logs and restart take only what they name (the review of 6bb806f) ---------
tok_env
for args in ".. reindex" "../x slack" ". slack" "ana reindex" "ana ../reindex" "-x slack" "Ana slack" \
  "a/b slack" "ana slack/x" "'' slack"; do
  code=0
  eval "run_stack revoke $args" >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "revoke $args exits $code, not 2"
done
for h in ".." "." "-x" 'a"b' "x\$(id)" "A" "a b" "a/b" "" "$(printf 'a%.0s' {1..65})"; do
  code=0
  run_stack smoke "$h" >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "smoke $(printf '%q' "$h") exits $code, not 2"
done
[ ! -s "$CURL_LOG" ] || fail "a refused revoke or smoke called curl: $(cat "$CURL_LOG")"
for args in "-f" "--tail=1" "nosuch" "bots;id" "bots --since=1h" "bots -f x"; do
  code=0
  # shellcheck disable=SC2086
  run_stack logs $args >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "logs $args exits $code, not 2"
done
for svc in "-x" "--help" "nosuch" "bots;id" ""; do
  code=0
  run_stack restart "$svc" >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "restart $(printf '%q' "$svc") exits $code, not 2"
done
! grep -qE ' (logs|restart) ' "$DOCKER_LOG" || fail "a refused logs or restart reached compose: $(cat "$DOCKER_LOG")"
run_stack logs bots -f >/dev/null || fail "logs bots -f was refused"
grep -q ' logs --tail 200 -f bots$' "$DOCKER_LOG" || fail "logs bots -f didn't run: $(cat "$DOCKER_LOG")"
run_stack restart worker-docs >/dev/null || fail "restart worker-docs was refused"
grep -q ' restart worker-docs$' "$DOCKER_LOG" || fail "restart worker-docs didn't run"

# --- deploy/.env is rewritten atomically, under a lock (the review of 6bb806f) ----------------
bot_env
envf="$WORK/stack/deploy/.env"
acl=0
if command -v setfacl >/dev/null && command -v getfacl >/dev/null && setfacl -m u:65534:r "$envf" 2>/dev/null; then
  acl=1
fi
# A hard link to the old file: a rename leaves it as it was (and deploy/.env with one link).
ln "$envf" "$WORK/env.old"
cp "$envf" "$WORK/env.copy"
out=$(run_bot add-bot scout 2>&1) || fail "add-bot scout failed: $out"
cmp -s "$WORK/env.old" "$WORK/env.copy" && [ "$(stat -c %h "$envf")" = 1 ] ||
  fail "add-bot rewrote deploy/.env in place, not by a rename"
grep -qE '^PMOS_RT_WORKER_TOKENS=worker-scout=[0-9a-f]{48}$' "$envf" || fail "add-bot scout didn't write its token"
rm -f "$WORK/env.old"
if [ "$acl" = 1 ]; then
  getfacl -pn "$envf" 2>/dev/null | grep -q '^user:65534:r--' || fail "add-bot lost deploy/.env's ACL: $(getfacl -p "$envf" 2>&1)"
  [ "$(stat -c %a "$envf")" = 640 ] || fail "deploy/.env with an ACL isn't 640 after add-bot: $(stat -c %a "$envf")"
  # Without the ACL again, for the mode check below.
  setfacl -b "$envf"
  chmod 600 "$envf"
else
  echo "the ACL check skipped: setfacl isn't installed here, or this filesystem has no ACLs"
fi
# A second add-bot, with no ACL: still a rename, whether or not this host has ACLs.
ln "$envf" "$WORK/env.old"
run_bot add-bot scout2 >/dev/null 2>&1 || fail "add-bot scout2 failed"
[ "$(stat -c %h "$envf")" = 1 ] || fail "add-bot scout2 rewrote deploy/.env in place, not by a rename"
rm -f "$WORK/env.old"
# The mode of the file add-bot made (a new file, renamed into place, checked above), as it made it.
[ "$(stat -c %a "$envf")" = 600 ] || fail "deploy/.env isn't 0600 after add-bot: $(stat -c %a "$envf")"
[ -e "$WORK/stack/deploy/.env.lock" ] || fail "add-bot took no lock"
# A lock file the caller can't write (root's, 0644, after a `sudo deploy/setup.sh`) doesn't stop
# add-bot: flock needs only a read-only descriptor.
bot_env
lock="$WORK/stack/deploy/.env.lock"
chmod 644 "$lock"
if [ "$(/usr/bin/id -u)" != 0 ]; then
  chmod 444 "$lock"
  code=0
  out=$(run_bot add-bot lockbot 2>&1) || code=$?
  chmod 600 "$lock"
elif command -v setpriv >/dev/null; then
  chown 65534:65534 "$WORK/stack/deploy" "$envf"
  chmod 711 "$WORK" "$WORK/stack"
  chmod 755 "$WORK/stack/deploy" "$SBIN" "$WORK/bin"
  chmod 666 "$DOCKER_LOG" "$DOCKER_ENV_LOG" "$OPENSHELL_LOG" "$SYSTEMCTL_LOG"
  if setpriv --reuid=65534 --regid=65534 --clear-groups sh -c ": >>'$lock'" 2>/dev/null; then
    fail "the test's lock file is writable by the other account"
  fi
  code=0
  out=$(PATH="$SBIN:$WORK/bin:$PATH" setpriv --reuid=65534 --regid=65534 --clear-groups "${stack[@]}" add-bot lockbot 2>&1) || code=$?
  chown root:root "$WORK/stack/deploy" "$envf"
  chmod 700 "$WORK" "$WORK/stack" "$WORK/stack/deploy" "$SBIN" "$WORK/bin"
else
  fail "these checks run as root and setpriv isn't installed to run add-bot as another user"
fi
[ "$code" = 0 ] || fail "add-bot with a lock file the caller can't write exits $code: $out"
grep -qE '^PMOS_RT_WORKER_TOKENS=.*worker-lockbot=[0-9a-f]{48}' "$envf" || fail "add-bot with a read-only lock file didn't write its token"
[ -z "$(find "$WORK/stack/deploy" -maxdepth 1 -name '.env.??????')" ] || fail "add-bot left a temporary file"
# Stopped (a kill, Ctrl-C) while the new deploy/.env is written: the new file is removed and
# deploy/.env is as it was (the review of eb49991). The stand-in mv sends TERM to pmos-admin.
bot_env
cp "$envf" "$WORK/env.before"
code=0
PATH="$WORK/killmv:$SBIN:$WORK/bin:$PATH" "${stack[@]}" add-bot scout >/dev/null 2>&1 || code=$?
[ "$code" = 143 ] || fail "add-bot stopped by TERM exits $code, not 143"
cmp -s "$WORK/env.before" "$envf" || fail "add-bot stopped by TERM changed deploy/.env"
[ -z "$(find "$WORK/stack/deploy" -maxdepth 1 -name '.env.??????')" ] || fail "add-bot stopped by TERM left its new deploy/.env"
[ ! -s "$DOCKER_LOG" ] || fail "add-bot stopped by TERM went on to restart the runtime"
# Four add-bots at once all keep their tokens.
bot_env
pids=()
for b in pa pb pc pd; do
  run_bot add-bot "$b" >/dev/null 2>&1 &
  pids+=($!)
done
for p in "${pids[@]}"; do wait "$p" || fail "a concurrent add-bot failed"; done
for b in pa pb pc pd; do
  grep -qE "^PMOS_RT_WORKER_TOKENS=.*worker-$b=[0-9a-f]{48}" "$envf" || fail "a concurrent add-bot lost $b's token: $(grep WORKER "$envf")"
  grep -qE "^PMOS_GW_BOT_TOKENS=.*bot-$b=[0-9a-f]{48}" "$envf" || fail "a concurrent add-bot lost $b's gateway token"
done
# Where flock isn't installed (the review of eb49991: there was no lock at all), deploy/.env
# isn't changed at all. A PATH of links to every tool on this one but flock.
NOFLOCK="$WORK/noflock"
mkdir -p "$NOFLOCK"
IFS=: read -ra path_dirs <<<"$PATH"
for d in "${path_dirs[@]}"; do
  [ -d "$d" ] || continue
  for f in "$d"/*; do
    n=${f##*/}
    [ "$n" != flock ] && [ -x "$f" ] && [ ! -e "$NOFLOCK/$n" ] || continue
    ln -s "$f" "$NOFLOCK/$n"
  done
done
! PATH="$NOFLOCK" command -v flock >/dev/null || fail "the PATH without flock has flock"
bot_env
cp "$envf" "$WORK/env.before"
code=0
out=$(PATH="$SBIN:$WORK/bin:$NOFLOCK" "${stack[@]}" add-bot pa 2>&1) || code=$?
[ "$code" = 1 ] && [[ "$out" == *"flock (util-linux) isn't installed"* ]] || fail "add-bot without flock exits $code: $out"
cmp -s "$WORK/env.before" "$envf" || fail "add-bot without flock changed deploy/.env"
[ ! -s "$DOCKER_LOG" ] || fail "add-bot without flock went on to restart the runtime"
# setup.sh too.
sed -i 's/^PMOS_SERVICE_TOKEN=.*/PMOS_SERVICE_TOKEN=generated/' "$senv"
cp "$senv" "$WORK/senv.before"
code=0
out=$(PATH="$WORK/bin:$NOFLOCK" bash "$WORK/setup/deploy/setup.sh" --content "$WORK/setup-content" \
  --state "$WORK/setup-state" 2>&1) || code=$?
[ "$code" != 0 ] && [[ "$out" == *"flock is not installed"* ]] || fail "setup.sh without flock exits $code: $out"
cmp -s "$WORK/senv.before" "$senv" || fail "setup.sh without flock changed deploy/.env"

# --- Pinned addresses and the managed host names (slice H2) ----------------------------------
# docs/openshell-any-host-spec.md §3.3, §3.5 and §6 ("The managed hosts block", "Compose").
# The literals: deploy/compose.yaml's five ipv4_address values, its ip_range default and bridge
# name are the ones pmos-admin checks and writes, and deploy/env.example says the same.
compose_src="$DEPLOY_SRC/compose.yaml"
for pair in toolserver:10.201.77.10 gateway:10.201.77.11 bots:10.201.77.12 vllm-default:10.201.77.13 \
  vllm-extraction:10.201.77.14; do
  grep -qF "pmos: { aliases: [${pair%%:*}.pmos.internal], ipv4_address: ${pair#*:} }" "$compose_src" ||
    fail "deploy/compose.yaml doesn't pin ${pair%%:*} at ${pair#*:}"
done
[ "$(grep -c 'ipv4_address:' "$compose_src")" = 5 ] || fail "deploy/compose.yaml pins other than the five addresses"
grep -qxF 'PINNED_ADDRS=(10.201.77.10 10.201.77.11 10.201.77.12 10.201.77.13 10.201.77.14)' "$SRC" &&
  grep -qxF 'PINNED_SERVICE_ADDRS=(10.201.77.10 10.201.77.11 10.201.77.12)' "$SRC" &&
  grep -qxF 'PINNED_NAMES=(toolserver.pmos.internal gateway.pmos.internal bots.pmos.internal)' "$SRC" ||
  fail "pmos-admin's pinned addresses aren't deploy/compose.yaml's"
grep -qF 'ip_range: ${PMOS_NETWORK_IP_RANGE:-10.201.77.128/25}' "$compose_src" &&
  grep -qxF 'NETWORK_IP_RANGE_DEFAULT=10.201.77.128/25' "$SRC" &&
  grep -qxF 'PMOS_NETWORK_IP_RANGE=10.201.77.128/25' "$DEPLOY_SRC/env.example" ||
  fail "the ip_range default differs between compose.yaml, pmos-admin and env.example"
grep -qxF '      com.docker.network.bridge.name: pmos0' "$compose_src" && grep -qxF 'NETWORK_BRIDGE=pmos0' "$SRC" ||
  fail "the bridge isn't named pmos0 in both compose.yaml and pmos-admin"
grep -qF 'subnet: ${PMOS_NETWORK_SUBNET:-10.201.77.0/24}' "$compose_src" && grep -qxF 'NETWORK_SUBNET_DEFAULT=10.201.77.0/24' "$SRC" &&
  grep -qF 'gateway: ${PMOS_NETWORK_GATEWAY:-10.201.77.1}' "$compose_src" && grep -qxF 'NETWORK_GATEWAY_DEFAULT=10.201.77.1' "$SRC" ||
  fail "the subnet or gateway default differs between compose.yaml and pmos-admin"

hn() { PATH="$WORK/bin:$PATH" "${stack[@]}" host-names "$@"; }
# A good block: check passes, naming each address.
good_hosts
out=$(hn check) || fail "host-names check fails on a good block: $out"
for want in 'toolserver.pmos.internal 10.201.77.10' 'gateway.pmos.internal 10.201.77.11' 'bots.pmos.internal 10.201.77.12'; do
  [[ "$out" == *"ok"*"$want"* ]] || fail "host-names check doesn't show $want: $out"
done
# No override: anything but `apply` or `check` alone is refused with exit 2 and the file untouched.
printf '127.0.0.1 localhost\n' >"$WORK/hosts"
cp "$WORK/hosts" "$WORK/hosts.before"
for args in "" "apply 10.201.77.99" "apply --force" "apply toolserver.pmos.internal=10.0.0.1" "check extra" \
  "bogus" "apply apply" "--help"; do
  code=0
  # shellcheck disable=SC2086 # the words are the arguments
  hn $args >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "host-names $args exits $code, not 2"
  cmp -s "$WORK/hosts" "$WORK/hosts.before" || fail "host-names $args changed the hosts file"
done
# Nor through the environment: the file is a constant, not a setting.
good_hosts
cp "$WORK/hosts" "$WORK/hosts.good"
printf '127.0.0.1 localhost\n' >"$WORK/hosts"
code=0
HOSTS_FILE="$WORK/hosts.good" PMOS_HOSTS_FILE="$WORK/hosts.good" hn check >/dev/null 2>&1 || code=$?
[ "$code" = 1 ] || fail "host-names check read another file from the environment ($code)"

# apply on a file without a block: every other line kept, in order, the block added at the end;
# then check passes. The file keeps its mode.
printf '127.0.0.1 localhost\n# a comment\n\n192.0.2.7 printer.example.com printer\n10.1.2.3 other.pmos.internal' >"$WORK/hosts"
chmod 644 "$WORK/hosts"
out=$(hn apply) || fail "host-names apply failed: $out"
[ "$(cat "$WORK/hosts")" = $'127.0.0.1 localhost\n# a comment\n\n192.0.2.7 printer.example.com printer\n10.1.2.3 other.pmos.internal\n'"$GOOD_BLOCK" ] ||
  fail "apply didn't keep the other lines and add the block: $(cat "$WORK/hosts")"
[ "$(stat -c %a "$WORK/hosts")" = 644 ] || fail "apply changed the hosts file's mode"
hn check >/dev/null || fail "check fails after apply"
# Idempotent: a second apply writes nothing (same content, same file).
cp "$WORK/hosts" "$WORK/hosts.once"
ino=$(stat -c %i "$WORK/hosts")
out=$(hn apply) || fail "a second apply failed: $out"
[[ "$out" == *"already up to date"* ]] || fail "a second apply doesn't say it changed nothing: $out"
cmp -s "$WORK/hosts" "$WORK/hosts.once" && [ "$(stat -c %i "$WORK/hosts")" = "$ino" ] || fail "a second apply rewrote the file"
[ -z "$(find "$WORK" -maxdepth 1 -name '.hosts.pmos.*')" ] || fail "apply left a temporary file behind"
# A stale block (a wrong address, a hand-spaced marker, a loopback line inside) is replaced where
# it stands; the lines around it are kept.
for stale in $'10.201.77.99 toolserver.pmos.internal\n10.201.77.11 gateway.pmos.internal\n10.201.77.12 bots.pmos.internal' \
  $'127.0.0.1 toolserver.pmos.internal gateway.pmos.internal bots.pmos.internal' \
  $'10.201.77.10 toolserver.pmos.internal\n10.201.77.11 gateway.pmos.internal\n10.201.77.12 bots.pmos.internal\n10.0.0.9 evil.pmos.internal' \
  $'10.201.77.11 toolserver.pmos.internal\n10.201.77.10 gateway.pmos.internal\n10.201.77.12 bots.pmos.internal'; do
  for begin in '# BEGIN pmos (managed by pmos-admin)' '#BEGIN   pmos'; do
    printf '127.0.0.1 localhost\n%s\n%s\n# END pmos\n192.0.2.7 printer\n' "$begin" "$stale" >"$WORK/hosts"
    code=0
    hn check >/dev/null 2>&1 || code=$?
    [ "$code" = 1 ] || fail "check passes a stale block ($code): $(cat "$WORK/hosts")"
    code=0
    run_bot sandbox docs --dry-run >/dev/null 2>&1 || code=$?
    [ "$code" = 2 ] || fail "sandbox --dry-run with a stale block exits $code, not 2"
    hn apply >/dev/null || fail "apply didn't fix a stale block: $(cat "$WORK/hosts")"
    [ "$(cat "$WORK/hosts")" = $'127.0.0.1 localhost\n'"$GOOD_BLOCK"$'\n192.0.2.7 printer' ] ||
      fail "apply didn't replace the stale block in place: $(cat "$WORK/hosts")"
  done
done
# Two blocks become one, where the first stood.
printf '%s\n127.0.0.1 localhost\n%s\n' "$GOOD_BLOCK" "$GOOD_BLOCK" >"$WORK/hosts"
code=0
hn check >/dev/null 2>&1 || code=$?
[ "$code" = 1 ] || fail "check passes two blocks"
hn apply >/dev/null || fail "apply didn't fix two blocks"
[ "$(cat "$WORK/hosts")" = "$GOOD_BLOCK"$'\n127.0.0.1 localhost' ] || fail "apply left two blocks: $(cat "$WORK/hosts")"
# Markers that don't pair up: which lines are the block's isn't clear, so apply writes nothing
# and exits 1; check fails.
for broken in $'# BEGIN pmos (managed by pmos-admin)\n10.201.77.10 toolserver.pmos.internal\n192.0.2.7 printer' \
  $'192.0.2.7 printer\n# END pmos' \
  $'# BEGIN pmos (managed by pmos-admin)\n# BEGIN pmos (managed by pmos-admin)\n# END pmos'; do
  printf '127.0.0.1 localhost\n%s\n' "$broken" >"$WORK/hosts"
  cp "$WORK/hosts" "$WORK/hosts.before"
  code=0
  out=$(hn apply 2>&1) || code=$?
  [ "$code" = 1 ] && [[ "$out" == *"broken pmos markers"* ]] || fail "apply on broken markers exits $code: $out"
  cmp -s "$WORK/hosts" "$WORK/hosts.before" || fail "apply on broken markers changed the file"
  code=0
  hn check >/dev/null 2>&1 || code=$?
  [ "$code" = 1 ] || fail "check passes broken markers"
done
# check fails on each wrong state outside the block too, naming it: the old install runbook's
# loopback line, a second line for a name (any case, a trailing dot, IPv6), a missing block, a
# resolver that answers elsewhere (consulted before the file) or not at all. apply keeps those
# lines (they aren't its own) and so exits 1, saying what is left.
for extra in '127.0.0.1 toolserver.pmos.internal gateway.pmos.internal bots.pmos.internal' \
  '127.0.1.1 bots.pmos.internal' '::1 gateway.pmos.internal' '10.0.0.5 BOTS.pmos.internal.' \
  '10.201.77.10 toolserver.pmos.internal # a copy by hand' 'fd00::5 toolserver.pmos.internal'; do
  printf '127.0.0.1 localhost\n%s\n%s\n' "$extra" "$GOOD_BLOCK" >"$WORK/hosts"
  code=0
  out=$(hn check 2>&1) || code=$?
  [ "$code" = 1 ] && [[ "$out" == *"outside the managed block"* ]] || fail "check passes the line $extra ($code): $out"
  case "$extra" in 127.* | ::1*) [[ "$out" == *"to loopback"* ]] || fail "check doesn't call $extra loopback: $out" ;; esac
  code=0
  out=$(hn apply 2>&1) || code=$?
  [ "$code" = 1 ] && [[ "$out" == *"still fails"* ]] || fail "apply with the line $extra exits $code: $out"
  grep -qxF "$extra" "$WORK/hosts" || fail "apply removed a line that isn't its own: $extra"
done
printf '127.0.0.1 localhost\n' >"$WORK/hosts"
code=0
out=$(hn check 2>&1) || code=$?
[ "$code" = 1 ] && [[ "$out" == *"no managed pmos block"* && "$out" == *"doesn't resolve"* ]] || fail "check passes no block ($code): $out"
good_hosts
code=0
out=$(FAKE_GETENT_ANSWER=10.9.9.9 hn check 2>&1) || code=$?
[ "$code" = 1 ] && [[ "$out" == *"resolves on this host to 10.9.9.9, not 10.201.77.10"* ]] ||
  fail "check passes a resolver answering elsewhere ($code): $out"
code=0
FAKE_GETENT_ANSWER=127.0.0.1 hn check >/dev/null 2>&1 || code=$?
[ "$code" = 1 ] || fail "check passes a resolver answering loopback"
# Two answers, one of them the pinned address: fails, naming both.
for two in "10.201.77.10 10.9.9.9" "10.9.9.9 10.201.77.10"; do
  code=0
  out=$(FAKE_GETENT_ANSWER="$two" hn check 2>&1) || code=$?
  [ "$code" = 1 ] && [[ "$out" == *"toolserver.pmos.internal resolves on this host to 10.201.77.10 10.9.9.9, not 10.201.77.10"* ]] ||
    fail "check passes two answers '$two' ($code): $out"
done
# A hosts file that is a symbolic link is never replaced; one this user can't write isn't
# either, and apply says to use sudo.
mv "$WORK/hosts" "$WORK/hosts.real"
ln -s "$WORK/hosts.real" "$WORK/hosts"
printf '127.0.0.1 localhost\n' >"$WORK/hosts.real"
code=0
out=$(hn apply 2>&1) || code=$?
[ "$code" = 1 ] && [[ "$out" == *"isn't a regular file"* ]] || fail "apply on a symbolic link exits $code: $out"
[ -L "$WORK/hosts" ] && [ "$(cat "$WORK/hosts.real")" = "127.0.0.1 localhost" ] || fail "apply replaced a symbolic link"
rm -f "$WORK/hosts" "$WORK/hosts.real"
printf '127.0.0.1 localhost\n' >"$WORK/hosts"
chmod 644 "$WORK/hosts"
code=0
if [ "$(/usr/bin/id -u)" != 0 ]; then
  chmod 444 "$WORK/hosts"
  out=$(hn apply 2>&1) || code=$?
  chmod 644 "$WORK/hosts"
elif command -v setpriv >/dev/null; then
  chmod 711 "$WORK" "$WORK/stack" "$WORK/stack/deploy"
  chmod 755 "$WORK/bin"
  out=$(PATH="$WORK/bin:$PATH" setpriv --reuid=65534 --regid=65534 --clear-groups "${stack[@]}" host-names apply 2>&1) || code=$?
  chmod 700 "$WORK" "$WORK/stack" "$WORK/stack/deploy" "$WORK/bin"
else
  fail "these checks run as root and setpriv isn't installed to run apply as another user"
fi
[ "$code" = 1 ] && [[ "$out" == *"run it with sudo"* ]] || fail "apply without write access exits $code: $out"
[ "$(cat "$WORK/hosts")" = "127.0.0.1 localhost" ] || fail "apply without write access changed the file"
# host-names needs nothing from deploy/.env: it works with none (and never reads one).
good_hosts
mv "$WORK/stack/deploy/.env" "$WORK/env.aside"
out=$(hn check 2>&1) || fail "host-names check needs deploy/.env: $out"
mv "$WORK/env.aside" "$WORK/stack/deploy/.env"
chmod 644 "$WORK/stack/deploy/.env"
out=$(hn check 2>&1) || fail "host-names check refuses over deploy/.env's mode: $out"
[[ "$out" != *"deploy/.env"* ]] || fail "host-names check looked at deploy/.env: $out"
chmod 600 "$WORK/stack/deploy/.env"

# `sandbox` refuses while the check fails (with --dry-run too), before openshell or systemctl;
# after apply it goes on.
bot_env "PMOS_RT_WORKER_TOKENS=worker-pm-ana=rt-secret-1" "PMOS_GW_BOT_TOKENS=bot-pm-ana=gw-secret-1"
for state in none loopback stale resolver; do
  good_hosts
  case "$state" in
    none) printf '127.0.0.1 localhost\n' >"$WORK/hosts" ;;
    loopback) printf '127.0.0.1 localhost toolserver.pmos.internal\n%s\n' "$GOOD_BLOCK" >"$WORK/hosts" ;;
    stale) sed -i 's/^10.201.77.12 /10.201.77.13 /' "$WORK/hosts" ;;
  esac
  for dry in "" --dry-run; do
    code=0
    # shellcheck disable=SC2086 # empty or one flag
    out=$( { [ "$state" != resolver ] || export FAKE_GETENT_ANSWER=10.201.77.12; run_bot sandbox pm-ana $dry; } 2>&1) || code=$?
    [ "$code" = 2 ] && [[ "$out" == *"host-names apply"* ]] || fail "sandbox $dry with names $state exits $code: $out"
    [[ "$out" != *secret* ]] || fail "sandbox's refusal printed a token: $out"
  done
  [ ! -s "$OPENSHELL_LOG" ] && [ ! -s "$SYSTEMCTL_LOG" ] && [ ! -e "$UNIT" ] || fail "sandbox with names $state ran something"
done
good_hosts
run_bot sandbox pm-ana >/dev/null || fail "sandbox with good names failed"
grep -q '^args=sandbox create --name pmos-pm-ana ' "$OPENSHELL_LOG" || fail "sandbox with good names didn't create"

# `up` and the network's settings (§3.3 step 2, §3.5): refused with exit 2, before anything
# starts: a subnet shorter than a /24 (holding the pinned addresses or not), one that doesn't
# hold them, a malformed one; a gateway outside the subnet or on a pinned address; an ip_range
# outside the subnet or holding a pinned address (.10 or .13 included).
net_refused=(
  "PMOS_NETWORK_SUBNET=10.201.0.0/16|shorter than a /24"
  "PMOS_NETWORK_SUBNET=10.0.0.0/8|shorter than a /24"
  "PMOS_NETWORK_SUBNET=10.201.76.0/23|shorter than a /24"
  "PMOS_NETWORK_SUBNET=10.50.0.0/24|POL-2"
  "PMOS_NETWORK_SUBNET=10.201.77.0/29|doesn't hold 10.201.77.10"
  "PMOS_NETWORK_SUBNET=10.201.77.5/24|host bits clear"
  "PMOS_NETWORK_SUBNET=10.201.077.0/24|host bits clear"
  "PMOS_NETWORK_SUBNET=10.201.77.0|host bits clear"
  "PMOS_NETWORK_GATEWAY=10.201.78.1|isn't an address inside"
  "PMOS_NETWORK_GATEWAY=10.201.77.10|is a pinned address"
  "PMOS_NETWORK_GATEWAY=10.201.77.255|isn't an address inside"
  "PMOS_NETWORK_IP_RANGE=10.201.77.0/24|holds 10.201.77.10"
  "PMOS_NETWORK_IP_RANGE=10.201.77.0/25|holds 10.201.77.10"
  "PMOS_NETWORK_IP_RANGE=10.201.77.12/30|holds 10.201.77.12"
  "PMOS_NETWORK_IP_RANGE=10.201.77.13/32|holds 10.201.77.13"
  "PMOS_NETWORK_IP_RANGE=10.201.78.128/25|isn't inside"
  "PMOS_NETWORK_IP_RANGE=10.201.77.130/25|host bits clear"
)
for case_ in "${net_refused[@]}"; do
  stack_env require "${case_%%|*}"
  code=0
  out=$(run_stack up 2>&1) || code=$?
  [ "$code" = 2 ] && [[ "$out" == *"${case_#*|}"* ]] || fail "up with ${case_%%|*} exits $code, or doesn't say '${case_#*|}': $out"
  ! grep -q ' up -d' "$DOCKER_LOG" || fail "up with ${case_%%|*} started something"
done
# Accepted: the defaults (blank or written out), and a longer prefix that holds the pinned
# addresses with an ip_range that holds none of them.
for ok in "" "PMOS_NETWORK_SUBNET=10.201.77.0/24 PMOS_NETWORK_GATEWAY=10.201.77.1 PMOS_NETWORK_IP_RANGE=10.201.77.128/25" \
  "PMOS_NETWORK_SUBNET= PMOS_NETWORK_IP_RANGE=" \
  "PMOS_NETWORK_SUBNET=10.201.77.0/25 PMOS_NETWORK_IP_RANGE=10.201.77.64/26" \
  "PMOS_NETWORK_IP_RANGE=10.201.77.16/28"; do
  # shellcheck disable=SC2086 # the words are deploy/.env lines
  stack_env require $ok
  out=$(run_stack up 2>&1) || fail "up with '$ok' was refused: $out"
  grep -q ' up -d' "$DOCKER_LOG" || fail "up with '$ok' didn't start"
done
# The live network (§3.3 step 3): one made before H2 (no ip_range, Docker's bridge name) or with
# other settings is refused with the fix; one as deploy/.env and compose.yaml say starts, and so
# does none at all (Compose makes it). "invalid Prefix" is how Docker 29 prints a network with no
# ip_range (seen on the test machine's own pre-H2 pmos_pmos network, 2026-11-23).
stack_env require
for live in "10.201.77.0/24 10.201.77.1 invalid Prefix;" "10.201.77.0/24 10.201.77.1 ;""10.201.77.0/24 10.201.77.1 ;pmos0" "10.201.77.0/24 10.201.77.1 10.201.77.128/25;" \
  "10.201.77.0/24 10.201.77.1 10.201.77.0/25;pmos0" "10.201.0.0/16 10.201.77.1 10.201.77.128/25;pmos0" \
  "10.201.77.0/24 10.201.77.1 10.201.77.128/25;pmos0;10.201.78.0/24 10.201.78.1 ;"; do
  : >"$DOCKER_LOG"
  code=0
  out=$(FAKE_NETWORK="$live" run_stack up 2>&1) || code=$?
  [ "$code" = 2 ] && [[ "$out" == *"deploy/pmos-admin down && deploy/pmos-admin up"* && "$out" == *"$live"* ]] ||
    fail "up on the live network '$live' exits $code, or doesn't give the fix: $out"
  ! grep -q ' up -d' "$DOCKER_LOG" || fail "up on the live network '$live' started something"
done
out=$(FAKE_NETWORK="10.201.77.0/24 10.201.77.1 10.201.77.128/25;pmos0" run_stack up 2>&1) || fail "up on a network as pinned was refused: $out"
grep -q ' up -d' "$DOCKER_LOG" || fail "up on a network as pinned didn't start"
grep -qF 'args=network inspect pmos_pmos --format {{range .IPAM.Config}}{{.Subnet}} {{.Gateway}} {{.IPRange}};{{end}}{{index .Options "com.docker.network.bridge.name"}}' "$DOCKER_LOG" ||
  fail "up didn't inspect the live network: $(cat "$DOCKER_LOG")"
stack_env require "PMOS_NETWORK_SUBNET=10.201.77.0/25" "PMOS_NETWORK_IP_RANGE=10.201.77.64/26"
out=$(FAKE_NETWORK="10.201.77.0/25 10.201.77.1 10.201.77.64/26;pmos0" run_stack up 2>&1) || fail "up on an overridden network as set was refused: $out"
code=0
FAKE_NETWORK="10.201.77.0/24 10.201.77.1 10.201.77.128/25;pmos0" run_stack up >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "up on a network made before the override exits $code, not 2"
# `status` shows the network and the names: ok rows when all is well, FAIL rows otherwise.
stack_env require
out=$(FAKE_NETWORK="10.201.77.0/24 10.201.77.1 10.201.77.128/25;pmos0" run_stack status 2>&1)
[[ "$out" == *"network"*"ok"*"pmos_pmos: 10.201.77.0/24 10.201.77.1 10.201.77.128/25;pmos0"* ]] || fail "status doesn't show the network ok: $out"
[[ "$out" == *"names"*"ok"*"toolserver.pmos.internal 10.201.77.10"* && "$out" == *"bots.pmos.internal 10.201.77.12"* ]] ||
  fail "status doesn't show the names ok: $out"
[[ "$out" != *FAIL* ]] || fail "status fails with a good network and names: $out"
out=$(run_stack status 2>&1)
[[ "$out" == *"network"*"not found"* ]] || fail "status doesn't say the network isn't made yet: $out"
out=$(FAKE_NETWORK="10.201.77.0/24 10.201.77.1 ;" run_stack status 2>&1)
[[ "$out" == *"network"*"FAIL"*"deploy/pmos-admin down && deploy/pmos-admin up"* ]] || fail "status doesn't fail a stale network: $out"
printf '127.0.0.1 localhost toolserver.pmos.internal\n' >"$WORK/hosts"
out=$(run_stack status 2>&1)
[[ "$out" == *"names"*"FAIL"*"no managed pmos block"* && "$out" == *"names"*"FAIL"*"to loopback"* ]] ||
  fail "status doesn't fail the names: $out"
stack_env require "PMOS_NETWORK_IP_RANGE=10.201.77.0/25"
out=$(run_stack status 2>&1)
[[ "$out" == *"network"*"FAIL"*"holds 10.201.77.10"* ]] || fail "status doesn't fail an ip_range holding a pinned address: $out"
good_hosts

# The one-off service for `users` has no pinned address, alias or published port (CI at cf621b5:
# a second container can't take the tool server's pinned address while the first holds it).
awk '/^  toolserver-cli:$/{on=1; next} on && /^  [a-z]/{on=0} on' "$DEPLOY_SRC/compose.yaml" >"$WORK/cli-service"
grep -qF 'extends: { service: toolserver }' "$WORK/cli-service" || fail "toolserver-cli doesn't extend toolserver"
grep -qF 'networks: !override [pmos]' "$WORK/cli-service" || fail "toolserver-cli keeps the pinned network settings"
grep -qF 'ports: !reset []' "$WORK/cli-service" || fail "toolserver-cli keeps the published port"
! grep -q 'ipv4_address\|aliases' "$WORK/cli-service" || fail "toolserver-cli has a pinned address or an alias"
# --- The host preflight at every start (slice H1) -----------------------------------------------
# docs/openshell-any-host-spec.md §3.2: `up`, `restore` and `add-bot` run the `run` stage (with the
# stack about to start), `sandbox` and `resume` add the OpenShell checks, `status` reads it as JSON;
# each refuses with exit 2 on a failing host, before anything starts or is written.
ENVP="$WORK/stack/deploy/.env"
bot_env "PMOS_RT_WORKER_TOKENS=worker-pm-ana=rt-secret-1" "PMOS_GW_BOT_TOKENS=bot-pm-ana=gw-secret-1"
run_stack up >/dev/null || fail "up on a good host failed"
[ "$(cat "$PF_LOG")" = "--env-file $ENVP --stage run --starting" ] || fail "up ran the preflight as: $(cat "$PF_LOG")"
: >"$PF_LOG"
run_stack up --web >/dev/null || fail "up --web on a good host failed"
[ "$(cat "$PF_LOG")" = "--env-file $ENVP --stage run --starting --web" ] || fail "up --web ran the preflight as: $(cat "$PF_LOG")"
: >"$PF_LOG"
run_bot add-bot scout >/dev/null || fail "add-bot on a good host failed"
[ "$(cat "$PF_LOG")" = "--env-file $ENVP --stage run --starting" ] || fail "add-bot ran the preflight as: $(cat "$PF_LOG")"
: >"$PF_LOG"
run_stack restore "$WORK/backup" </dev/null >/dev/null 2>&1 || true # stops at the RESTORE question
[ "$(cat "$PF_LOG")" = "--env-file $ENVP --stage run --starting" ] || fail "restore ran the preflight as: $(cat "$PF_LOG")"
: >"$PF_LOG"
run_bot sandbox pm-ana >/dev/null || fail "sandbox on a good host failed"
[ "$(cat "$PF_LOG")" = "--env-file $ENVP --stage run --openshell" ] || fail "sandbox ran the preflight as: $(cat "$PF_LOG")"
: >"$PF_LOG"
run_bot sandbox pm-ana --dry-run >/dev/null || fail "sandbox --dry-run on a good host failed"
[ "$(cat "$PF_LOG")" = "--env-file $ENVP --stage run" ] || fail "sandbox --dry-run ran the preflight as: $(cat "$PF_LOG")"
: >"$PF_LOG"
run_bot resume pm-ana >/dev/null || fail "resume on a good host failed"
[ "$(cat "$PF_LOG")" = "--env-file $ENVP --stage run --openshell" ] || fail "resume ran the preflight as: $(cat "$PF_LOG")"
: >"$PF_LOG"
out=$(run_stack status 2>&1)
[ "$(cat "$PF_LOG")" = "--env-file $ENVP --stage run --json" ] || fail "status ran the preflight as: $(cat "$PF_LOG")"
[[ "$out" == *"== host"*"preflight"*"ok"*"checks pass"* ]] || fail "status doesn't show the preflight passing: $out"
# A failing host (a 5.15 kernel): each refuses with exit 2, saying which check failed, and starts,
# writes or creates nothing.
fail_host() { cp "$PF_FIXTURE/../kernel-5.15/cmd/uname-r" "$PFROOT/cmd/uname-r"; }
bot_env "PMOS_RT_WORKER_TOKENS=worker-pm-ana=rt-secret-1" "PMOS_GW_BOT_TOKENS=bot-pm-ana=gw-secret-1"
fail_host
cp "$ENVP" "$WORK/env.before"
for cmd in "up" "up --web" "add-bot scout" "sandbox pm-ana" "resume pm-ana"; do
  : >"$DOCKER_LOG"
  : >"$OPENSHELL_LOG"
  : >"$SYSTEMCTL_LOG"
  rm -rf "$XDG_CONFIG_HOME"
  code=0
  # shellcheck disable=SC2086 # the command and its arguments
  out=$(run_bot $cmd 2>&1) || code=$?
  [ "$code" = 2 ] || fail "$cmd on a failing host exits $code, not 2: $out"
  [[ "$out" == *"H-KERNEL"*"fail"*"5.15.0"* && "$out" == *"the host preflight failed"* ]] ||
    fail "$cmd on a failing host doesn't name the check: $out"
  ! grep -q ' up -d' "$DOCKER_LOG" || fail "$cmd on a failing host started something"
  [ ! -s "$OPENSHELL_LOG" ] && [ ! -s "$SYSTEMCTL_LOG" ] && [ ! -e "$UNIT" ] || fail "$cmd on a failing host ran openshell or systemctl"
  cmp -s "$WORK/env.before" "$ENVP" || fail "$cmd on a failing host changed deploy/.env"
  [[ "$out" != *secret* ]] || fail "$cmd's refusal printed a token"
done
: >"$DOCKER_LOG"
code=0
run_stack restore "$WORK/backup" </dev/null >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] && [ ! -s "$DOCKER_LOG" ] || fail "restore on a failing host exits $code, or called docker"
out=$(run_stack status 2>&1) || fail "status on a failing host exits non-zero"
[[ "$out" == *"H-KERNEL"*"FAIL"*"5.15.0"* ]] || fail "status doesn't show the failed check: $out"
# The OpenShell checks are run by sandbox and resume only: a canary that doesn't start refuses them.
bot_env "PMOS_RT_WORKER_TOKENS=worker-pm-ana=rt-secret-1" "PMOS_GW_BOT_TOKENS=bot-pm-ana=gw-secret-1"
for cmd in sandbox resume; do
  code=0
  out=$(FAKE_CANARY=nolandlock run_bot "$cmd" pm-ana 2>&1) || code=$?
  [ "$code" = 2 ] && [[ "$out" == *"O-CANARY"*"fail"* ]] || fail "$cmd with Landlock not applied in the canary exits $code: $out"
  [ ! -s "$OPENSHELL_LOG" ] || fail "$cmd went on after the canary failed"
done
FAKE_CANARY=nolandlock run_stack up >/dev/null || fail "up runs the OpenShell checks"
# resume checks the service names first (H2 left it to H1).
bot_env
printf '127.0.0.1 localhost\n' >"$WORK/hosts"
code=0
out=$(run_bot resume pm-ana 2>&1) || code=$?
[ "$code" = 2 ] && [[ "$out" == *"host-names apply"* ]] || fail "resume with no managed block exits $code: $out"
[ ! -s "$OPENSHELL_LOG" ] && [ ! -s "$SYSTEMCTL_LOG" ] || fail "resume with no managed block ran something"
# pause never runs it: an incident is never held up by the host (S2).
bot_env
fail_host
run_bot pause pm-ana >/dev/null || fail "pause on a failing host failed"
[ ! -s "$PF_LOG" ] || fail "pause ran the preflight"
# `preflight` by hand: its arguments pass through, and its exit status is the preflight's.
bot_env
out=$(run_stack preflight --stage quick) || fail "preflight --stage quick by hand failed: $out"
[ "$(cat "$PF_LOG")" = "--env-file $ENVP --stage quick" ] || fail "preflight by hand ran as: $(cat "$PF_LOG")"
fail_host
code=0
run_stack preflight >/dev/null 2>&1 || code=$?
[ "$code" = 1 ] || fail "preflight by hand on a failing host exits $code, not 1"

# --- The metadata block: host-firewall apply, check, remove (slice H1) ----------------------------
# Stand-ins: iptables and ip6tables keep their rules in files ($FW_DIR); chown (root-owned files
# on a test machine that may not be root) and chattr record their arguments.
FWBIN="$WORK/fwbin"
export FW_DIR="$WORK/fw" CHATTR_LOG="$WORK/chattr.log"
mkdir -p "$FWBIN" "$FW_DIR"
cat >"$FWBIN/iptables" <<'EOF'
#!/usr/bin/env bash
tool=$(basename "$0")
[ "$tool" != ip6tables ] || [ "${FAKE_IP6_CHAIN:-0}" = 1 ] || exit 1
rules="$FW_DIR/$tool.rules"
touch "$rules"
args=("$@")
[ "${args[0]}" != -w ] || args=("${args[@]:1}")
op=${args[0]}
[ "${args[1]:-}" = DOCKER-USER ] || exit 2
rest="${args[*]:2}"
case "$op" in
  -S) [ -e "$FW_DIR/$tool.chain" ] || exit 1; sed 's/^/-A DOCKER-USER /' "$rules" ;;
  -N) touch "$FW_DIR/$tool.chain" ;;
  -C) grep -qxF -- "$rest" "$rules" ;;
  -I) [ "${args[2]}" = 1 ] || exit 2; { printf '%s\n' "${args[*]:3}"; cat "$rules"; } >"$rules.t"; mv "$rules.t" "$rules" ;;
  -D) awk -v r="$rest" '$0 == r && !d { d = 1; next } { print }' "$rules" >"$rules.t"; mv "$rules.t" "$rules" ;;
  *) exit 2 ;;
esac
EOF
cp "$FWBIN/iptables" "$FWBIN/ip6tables"
printf '#!/bin/sh\nprintf "%%s\\n" "$*" >>"$CHATTR_LOG"\n' >"$FWBIN/chattr"
printf '#!/bin/sh\nexit 0\n' >"$FWBIN/chown"
chmod +x "$FWBIN"/*
fw_reset() { rm -rf "$FW_DIR"; mkdir -p "$FW_DIR"; touch "$FW_DIR/iptables.chain"; : >"$CHATTR_LOG"; }
run_fw() { PATH="$FWBIN:$SBIN:$WORK/bin:$PATH" "${stack[@]}" "$@"; }
bot_env
fw_reset
# Not root: refused, nothing written.
code=0
out=$(run_fw host-firewall apply 2>&1) || code=$?
[ "$code" = 1 ] && [[ "$out" == *"sudo"* ]] || fail "host-firewall apply as a user exits $code: $out"
[ ! -e "$PFROOT/etc/pmos/metadata-block.conf" ] && [ ! -s "$FW_DIR/iptables.rules" ] || fail "host-firewall apply as a user wrote something"
# Anything but apply, check or remove alone: exit 2.
for args in "" "apply extra" "bogus" "apply --subnet 10.0.0.0/8"; do
  code=0
  # shellcheck disable=SC2086
  FAKE_UID=0 run_fw host-firewall $args >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "host-firewall $args exits $code, not 2"
done
# As root: the rules for pmos0 (with the subnet) and docker0, to the three addresses; the
# root-owned script and its subnet file; the drop-in, the service and the timer; daemon-reload and
# the timer enabled. Then check passes, and the preflight's H-METADATA sees the block.
: >"$SYSTEMCTL_LOG"
out=$(FAKE_UID=0 run_fw host-firewall apply 2>&1) || fail "host-firewall apply failed: $out"
want_rules=""
for t in 100.100.100.200 168.63.129.16 169.254.169.254; do
  want_rules+="-i docker0 -d $t/32 -m comment --comment pmos-metadata-block -j DROP"$'\n'
  want_rules+="-i pmos0 -s 10.201.77.0/24 -d $t/32 -m comment --comment pmos-metadata-block -j DROP"$'\n'
done
[ "$(sort "$FW_DIR/iptables.rules")" = "$(printf '%s' "$want_rules" | sort)" ] || fail "the rules: $(cat "$FW_DIR/iptables.rules")"
[ ! -s "$FW_DIR/ip6tables.rules" ] || fail "IPv6 rules without an ip6tables DOCKER-USER chain"
[ "$(cat "$PFROOT/etc/pmos/metadata-block.conf")" = "SUBNET=10.201.77.0/24" ] || fail "the subnet file"
[ "$(stat -c %a "$PFROOT/usr/local/sbin/pmos-metadata-block")" = 755 ] || fail "the script isn't 0755"
cmp -s "$PFROOT/usr/local/sbin/pmos-metadata-block" "$WORK/stack/deploy/pmos-metadata-block" || fail "the installed script isn't the kit's"
[ "$(cat "$PFROOT/etc/systemd/system/docker.service.d/pmos-metadata-block.conf")" = $'[Service]\nExecStartPost=-/usr/local/sbin/pmos-metadata-block apply' ] ||
  fail "the docker.service drop-in"
grep -qx 'ExecStart=/usr/local/sbin/pmos-metadata-block apply' "$PFROOT/etc/systemd/system/pmos-metadata-block.service" || fail "the service"
grep -qx 'OnUnitActiveSec=1min' "$PFROOT/etc/systemd/system/pmos-metadata-block.timer" || fail "the timer doesn't run every minute"
grep -qx 'daemon-reload' "$SYSTEMCTL_LOG" && grep -qx 'enable --now pmos-metadata-block.timer' "$SYSTEMCTL_LOG" || fail "the timer wasn't enabled: $(cat "$SYSTEMCTL_LOG")"
[[ "$(cat "$PFROOT/run/pmos/metadata-block.status")" == "ok "* ]] || fail "apply recorded no status"
if command -v systemd-analyze >/dev/null; then
  for u in pmos-metadata-block.service pmos-metadata-block.timer; do
    cp "$PFROOT/etc/systemd/system/$u" "$WORK/$u"
  done
  v=$(systemd-analyze verify "$WORK/pmos-metadata-block.timer" 2>&1 | grep -F 'pmos-metadata-block.timer' | grep -v 'is not executable\|No such file' || true)
  [ -z "$v" ] || fail "systemd-analyze verify warned about the timer: $v"
fi
# Idempotent: a second apply adds nothing.
FAKE_UID=0 run_fw host-firewall apply >/dev/null 2>&1 || fail "a second apply failed"
[ "$(wc -l <"$FW_DIR/iptables.rules")" = 6 ] || fail "a second apply doubled rules: $(cat "$FW_DIR/iptables.rules")"
out=$(FAKE_UID=0 run_fw host-firewall check 2>&1) || fail "host-firewall check fails after apply: $out"
# The preflight (not root: the timer's status) sees the block in place and passes H-METADATA.
printf 'enabled\n' >"$PFROOT/cmd/timer-enabled"
printf 'active\n' >"$PFROOT/cmd/timer-active"
out=$(run_stack preflight --only H-METADATA 2>&1) || fail "H-METADATA fails with the block in place: $out"
# Each piece missing: check fails (and says which); the preflight fails H-METADATA.
cp "$FW_DIR/iptables.rules" "$WORK/rules.good"
sed -i '/-i docker0 -d 169.254.169.254/d' "$FW_DIR/iptables.rules"
code=0
out=$(FAKE_UID=0 run_fw host-firewall check 2>&1) || code=$?
[ "$code" = 1 ] && [[ "$out" == *"missing: iptables -i docker0 -d 169.254.169.254/32"* ]] || fail "check with a rule missing exits $code: $out"
cp "$WORK/rules.good" "$FW_DIR/iptables.rules"
mv "$PFROOT/etc/systemd/system/docker.service.d/pmos-metadata-block.conf" "$WORK/dropin.aside"
code=0
out=$(FAKE_UID=0 run_fw host-firewall check 2>&1) || code=$?
[ "$code" = 1 ] && [[ "$out" == *"drop-in"* ]] || fail "check with the drop-in missing exits $code: $out"
code=0
run_stack preflight --only H-METADATA >/dev/null 2>&1 || code=$?
[ "$code" = 1 ] || fail "H-METADATA passes with the drop-in missing"
mv "$WORK/dropin.aside" "$PFROOT/etc/systemd/system/docker.service.d/pmos-metadata-block.conf"
code=0
out=$(FAKE_TIMER_ACTIVE=inactive FAKE_UID=0 run_fw host-firewall check 2>&1) || code=$?
[ "$code" = 1 ] && [[ "$out" == *"timer isn't active"* ]] || fail "check with the timer stopped exits $code: $out"
rm "$PFROOT/etc/systemd/system/pmos-metadata-block.timer"
code=0
FAKE_UID=0 run_fw host-firewall check >/dev/null 2>&1 || code=$?
[ "$code" = 1 ] || fail "check with the timer unit missing exits $code"
FAKE_UID=0 run_fw host-firewall apply >/dev/null 2>&1 || fail "apply didn't put the timer back"
# Not root, check reads the timer's status: a stale one fails.
echo "ok 1000" >"$PFROOT/run/pmos/metadata-block.status"
code=0
out=$(run_fw host-firewall check 2>&1) || code=$?
[ "$code" = 1 ] && [[ "$out" == *"weren't confirmed"* ]] || fail "check as a user with a stale status exits $code: $out"
# With an ip6tables DOCKER-USER chain (Docker made one), the IPv6 rules too.
touch "$FW_DIR/ip6tables.chain"
FAKE_IP6_CHAIN=1 FAKE_UID=0 run_fw host-firewall apply >/dev/null 2>&1 || fail "apply with IPv6 failed"
[ "$(sort "$FW_DIR/ip6tables.rules")" = "$(printf '%s\n' '-i docker0 -d fd00:ec2::254/128 -m comment --comment pmos-metadata-block -j DROP' '-i pmos0 -d fd00:ec2::254/128 -m comment --comment pmos-metadata-block -j DROP' | sort)" ] ||
  fail "the IPv6 rules: $(cat "$FW_DIR/ip6tables.rules")"
# remove takes all three away (and is refused as a user).
code=0
run_fw host-firewall remove >/dev/null 2>&1 || code=$?
[ "$code" = 1 ] && [ -s "$FW_DIR/iptables.rules" ] || fail "remove as a user exits $code, or removed rules"
: >"$SYSTEMCTL_LOG"
FAKE_IP6_CHAIN=1 FAKE_UID=0 run_fw host-firewall remove >/dev/null 2>&1 || fail "remove failed"
[ ! -s "$FW_DIR/iptables.rules" ] && [ ! -s "$FW_DIR/ip6tables.rules" ] || fail "remove left rules"
for f in etc/pmos/metadata-block.conf usr/local/sbin/pmos-metadata-block etc/systemd/system/docker.service.d/pmos-metadata-block.conf \
  etc/systemd/system/pmos-metadata-block.service etc/systemd/system/pmos-metadata-block.timer run/pmos/metadata-block.status; do
  [ ! -e "$PFROOT/$f" ] || fail "remove left /$f"
done
grep -qx 'disable --now pmos-metadata-block.timer' "$SYSTEMCTL_LOG" || fail "remove didn't stop the timer"
code=0
out=$(FAKE_UID=0 run_fw host-firewall check 2>&1) || code=$?
[ "$code" = 1 ] && [[ "$out" == *"never applied"* ]] || fail "check after remove exits $code: $out"
# A subnet the network checks refuse isn't written into a rule.
bot_env PMOS_NETWORK_SUBNET=10.0.0.0/8
fw_reset
code=0
FAKE_UID=0 run_fw host-firewall apply >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] && [ ! -s "$FW_DIR/iptables.rules" ] || fail "apply with a /8 subnet exits $code, or wrote rules"

# --- host acknowledge (H-PRIVATE only) and host attest local-gpu (slice H1) ------------------------
ACKLOG="$PFROOT/var/log/pmos/host-acknowledgements.log"
public_host() { # the fixture host with a public address
  python3 -I -c '
import json, sys
d = json.load(open(sys.argv[1]))
d[1]["addr_info"].append({"family": "inet", "local": "203.0.113.7", "prefixlen": 24})
json.dump(d, open(sys.argv[1], "w"))
' "$PFROOT/cmd/ip-addr"
}
bot_env PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=0
fw_reset
public_host
cp "$ENVP" "$WORK/env.before"
# Every check but H-PRIVATE: exit 2, nothing written or logged (P-8), even as root.
for id in H-METADATA H-EXPOSURE H-DEVLOGIN h-private O-CANARY ""; do
  code=0
  SUDO_USER=ana FAKE_UID=0 run_fw host acknowledge "$id" --reason test >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "host acknowledge '$id' exits $code, not 2"
done
cmp -s "$WORK/env.before" "$ENVP" && [ ! -e "$ACKLOG" ] || fail "a refused acknowledgement wrote or logged something"
# Not root: refused; no reason, a reason with a line break or a tab: refused; not private: refused.
code=0
run_fw host acknowledge H-PRIVATE --reason test >/dev/null 2>&1 || code=$?
[ "$code" = 1 ] || fail "host acknowledge as a user exits $code, not 1"
for r in "" "   " $'two\nlines' $'a\ttab'; do
  code=0
  SUDO_USER=ana FAKE_UID=0 run_fw host acknowledge H-PRIVATE --reason "$r" >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "host acknowledge with the reason $(printf '%q' "$r") exits $code, not 2"
done
code=0
SUDO_USER=ana FAKE_UID=0 run_fw host acknowledge H-PRIVATE >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "host acknowledge without --reason exits $code, not 2"
cmp -s "$WORK/env.before" "$ENVP" && [ ! -e "$ACKLOG" ] || fail "a refused acknowledgement wrote or logged something"
# Before it: up refuses (H-PRIVATE fails on the public address).
code=0
out=$(run_stack up 2>&1) || code=$?
[ "$code" = 2 ] && [[ "$out" == *"H-PRIVATE"*"fail"*"203.0.113.7"* ]] || fail "up on a private declaration with a public address exits $code: $out"
# The acknowledgement: deploy/.env's line and the log's, with the login and what was seen.
out=$(SUDO_USER=ana FAKE_UID=0 run_fw host acknowledge H-PRIVATE --reason "closed test, inbound blocked at the security group" 2>&1) ||
  fail "host acknowledge H-PRIVATE failed: $out"
fp=$(sed -n 's/^PMOS_HOST_ACK_H_PRIVATE=//p' "$ENVP")
[[ "$fp" =~ ^[0-9a-f]{64}$ ]] || fail "no fingerprint in deploy/.env: $(grep HOST_ACK "$ENVP")"
IFS=$'\t' read -r at who chk act saw lfp reason <"$ACKLOG"
[[ "$at" =~ ^20[0-9-]+T[0-9:]+Z$ ]] && [ "$who" = ana ] && [ "$chk" = H-PRIVATE ] && [ "$act" = acknowledge ] &&
  [[ "$saw" == *203.0.113.7* ]] && [ "$lfp" = "$fp" ] && [ "$reason" = "closed test, inbound blocked at the security group" ] ||
  fail "the log line: $(cat "$ACKLOG")"
[ "$(stat -c %a "$ACKLOG")" = 644 ] || fail "the log isn't 0644"
grep -qx -- "+a -- $ACKLOG" "$CHATTR_LOG" || fail "the log wasn't made append-only: $(cat "$CHATTR_LOG")"
# Now up starts (H-PRIVATE warns), and status prints the acknowledgement on its own line.
out=$(run_stack up 2>&1) || fail "up with the acknowledgement was refused: $out"
[[ "$out" == *"H-PRIVATE"*"warn"* ]] || fail "up doesn't show H-PRIVATE's warning: $out"
out=$(run_stack status 2>&1)
[[ "$out" == *'host: PMOS_EXPOSURE=private acknowledged on '*' by ana: public addresses: 203.0.113.7; metadata probe answered: no; "closed test, inbound blocked at the security group"'* ]] ||
  fail "status doesn't print the acknowledgement: $out"
# Development sign-in is then held to public strength.
sed -i 's/^PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=0$/PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=1/' "$ENVP"
code=0
out=$(run_stack up 2>&1) || code=$?
[ "$code" = 2 ] && [[ "$out" == *"H-DEVLOGIN"*"fail"* ]] || fail "dev sign-in on an acknowledged private host exits $code: $out"
sed -i 's/^PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=1$/PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=0/' "$ENVP"
# A hand-written acknowledgement for another check fails the preflight (P-8).
echo "PMOS_HOST_ACK_H_EXPOSURE=$fp" >>"$ENVP"
code=0
out=$(run_stack up 2>&1) || code=$?
[ "$code" = 2 ] && [[ "$out" == *"PMOS_HOST_ACK_H_EXPOSURE"* ]] || fail "a hand-written PMOS_HOST_ACK_H_EXPOSURE exits $code: $out"
sed -i '/^PMOS_HOST_ACK_H_EXPOSURE=/d' "$ENVP"
# --clear: the line goes, the log says so, and up refuses again.
SUDO_USER=ana FAKE_UID=0 run_fw host acknowledge --clear H-PRIVATE >/dev/null 2>&1 || fail "host acknowledge --clear failed"
! grep -q '^PMOS_HOST_ACK_H_PRIVATE=' "$ENVP" || fail "--clear left the deploy/.env line"
[ "$(tail -1 "$ACKLOG" | cut -f3,4)" = $'H-PRIVATE\tclear' ] || fail "--clear wasn't logged: $(tail -1 "$ACKLOG")"
code=0
run_stack up >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "up after --clear exits $code, not 2"
# Writing the deploy/.env line back by hand doesn't bring it back: the log's last word is a clear.
echo "PMOS_HOST_ACK_H_PRIVATE=$fp" >>"$ENVP"
code=0
run_stack up >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "a hand-restored acknowledgement exits $code, not 2"
sed -i '/^PMOS_HOST_ACK_H_PRIVATE=/d' "$ENVP"
# A host that looks private: nothing to acknowledge, nothing written.
cp "$PF_FIXTURE/cmd/ip-addr" "$PFROOT/cmd/ip-addr"
cp "$ENVP" "$WORK/env.before"
lines=$(wc -l <"$ACKLOG")
code=0
SUDO_USER=ana FAKE_UID=0 run_fw host acknowledge H-PRIVATE --reason test >/dev/null 2>&1 || code=$?
[ "$code" = 1 ] && cmp -s "$WORK/env.before" "$ENVP" && [ "$(wc -l <"$ACKLOG")" = "$lines" ] ||
  fail "acknowledging a private-looking host exits $code, or wrote something"
# A public declaration: nothing to acknowledge.
public_host
sed -i 's/^PMOS_EXPOSURE=private$/PMOS_EXPOSURE=public/' "$ENVP"
code=0
SUDO_USER=ana FAKE_UID=0 run_fw host acknowledge H-PRIVATE --reason test >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "acknowledging on a public declaration exits $code, not 2"

# host attest local-gpu: refused without --owner-controls, a reason, root, or PMOS_MODEL_SOURCE=local.
bot_env PMOS_CONNECTORS=live
fw_reset
printf 'GPU 0: NVIDIA L4 (UUID: GPU-11111111-2222-3333-4444-555555555555)\n' >"$PFROOT/cmd/nvidia-smi-L"
# Live connectors under local without the attestation: up refuses with exit 2 (H-LOCAL).
code=0
out=$(run_stack up 2>&1) || code=$?
[ "$code" = 2 ] && [[ "$out" == *"H-LOCAL"*"fail"*"PMOS_CONNECTORS=live with no attestation"* ]] || fail "up with live connectors and no attestation exits $code: $out"
! grep -q ' up -d' "$DOCKER_LOG" || fail "up with live connectors and no attestation started something"
for args in "local-gpu --reason rack" "local-gpu --owner-controls" "local-gpu --owner-controls --reason ''" "other --owner-controls --reason rack" ""; do
  code=0
  eval "SUDO_USER=ana FAKE_UID=0 run_fw host attest $args" >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "host attest $args exits $code, not 2"
done
code=0
SUDO_USER=ana FAKE_UID=0 run_fw host attest local-gpu --owner-controls --reason rack >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "host attest without PMOS_MODEL_SOURCE=local in deploy/.env exits $code, not 2"
echo PMOS_MODEL_SOURCE=hosted-test >>"$ENVP"
code=0
SUDO_USER=ana FAKE_UID=0 run_fw host attest local-gpu --owner-controls --reason rack >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] || fail "host attest under hosted-test exits $code, not 2"
sed -i 's/^PMOS_MODEL_SOURCE=hosted-test$/PMOS_MODEL_SOURCE=local/' "$ENVP"
code=0
run_fw host attest local-gpu --owner-controls --reason rack >/dev/null 2>&1 || code=$?
[ "$code" = 1 ] || fail "host attest as a user exits $code, not 1"
[ ! -e "$ACKLOG" ] && ! grep -q '^PMOS_HOST_ATTEST' "$ENVP" || fail "a refused attestation wrote or logged something"
out=$(SUDO_USER=ana FAKE_UID=0 run_fw host attest local-gpu --owner-controls --reason "the owner's own rack" 2>&1) || fail "host attest failed: $out"
afp=$(sed -n 's/^PMOS_HOST_ATTEST_LOCAL_GPU=//p' "$ENVP")
[[ "$afp" =~ ^[0-9a-f]{64}$ ]] || fail "no attestation in deploy/.env"
[ "$(cut -f2-4,5 "$ACKLOG")" = $'ana\tlocal-gpu\tattest\t1x NVIDIA L4' ] || fail "the attestation's log line: $(cat "$ACKLOG")"
out=$(run_stack up 2>&1) || fail "up with live connectors and the attestation was refused: $out"
out=$(run_stack status 2>&1)
[[ "$out" == *"host: PMOS_MODEL_SOURCE=local attested (local-gpu) on "*" by ana: 1x NVIDIA L4; \"the owner's own rack\""* ]] ||
  fail "status doesn't print the attestation: $out"
# Another GPU (or disk in another machine): the attestation no longer matches; up refuses.
printf 'GPU 0: NVIDIA L4 (UUID: GPU-99999999-2222-3333-4444-555555555555)\n' >"$PFROOT/cmd/nvidia-smi-L"
code=0
out=$(run_stack up 2>&1) || code=$?
[ "$code" = 2 ] && [[ "$out" == *"H-LOCAL"*"no longer matches"* ]] || fail "up with another GPU exits $code: $out"
SUDO_USER=ana FAKE_UID=0 run_fw host attest local-gpu --clear >/dev/null 2>&1 || fail "host attest --clear failed"
! grep -q '^PMOS_HOST_ATTEST_LOCAL_GPU=' "$ENVP" && [ "$(tail -1 "$ACKLOG" | cut -f3,4)" = $'local-gpu\tclear' ] ||
  fail "--clear didn't remove and log the attestation"
# No secret reached the log or a command line in all of this.
! grep -q 'secret\|rt-web-\|ts-web-' "$ACKLOG" || fail "a token reached the acknowledgement log"

# --- compose renders (C9, C14): without docker compose, say so and skip ---------------------
if docker compose version >/dev/null 2>&1; then
  mkdir -p "$WORK/checkout" "$WORK/state"
  render() { # env file, profiles...
    local env=$1
    shift
    docker compose --project-directory "$DEPLOY_SRC" -f "$DEPLOY_SRC/compose.yaml" --env-file "$env" "$@" config
  }
  # A filled deploy/.env, as setup.sh makes it (every `generated` value replaced).
  sed -e "s|=generated\$|=test-$RANDOM-0123456789abcdef0123456789abcdef|" \
    -e "s|^PMOS_CONTENT_CHECKOUT=.*|PMOS_CONTENT_CHECKOUT=$WORK/checkout|" \
    -e "s|^PMOS_STATE_DIR=.*|PMOS_STATE_DIR=$WORK/state|" \
    -e "s|^PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=.*|PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=1|" \
    "$DEPLOY_SRC/env.example" >"$WORK/full.env"
  # `up --web --console`, with Slack: a valid configuration.
  full=$(render "$WORK/full.env" --profile web --profile console --profile slack) ||
    fail "compose doesn't render up --web --console with a filled deploy/.env"
  docker compose --project-directory "$DEPLOY_SRC" -f "$DEPLOY_SRC/compose.yaml" \
    --env-file "$WORK/full.env" --profile web --profile console --profile slack config -q ||
    fail "compose config -q refused up --web"
  for want in 'PMOS_ASSERTION_MODE: require' 'web:bots:' 'console:bots:' 'web:toolserver:' \
    'console:toolserver:' 'PMOS_ONBOARDING_V2: "1"' 'PMOS_NUDGES: "1"'; do
    [[ "$full" == *"$want"* ]] || fail "the rendered up --web has no $want"
  done
  [[ "$full" == *'console=test-'* && "$full" == *'web=test-'* ]] ||
    fail "the runtime's tokens lack web= or console="
  # M1: the pmos network's subnet and gateway are pinned, and can be overridden (within what
  # slice H2 allows: a longer prefix that still holds the pinned addresses, which pmos-admin up
  # checks; tested above).
  [[ "$full" == *'subnet: 10.201.77.0/24'* && "$full" == *'gateway: 10.201.77.1'* ]] ||
    fail "the pmos network's subnet and gateway aren't pinned"
  sed -e 's|^PMOS_NETWORK_SUBNET=.*|PMOS_NETWORK_SUBNET=10.201.77.0/25|' \
    -e 's|^PMOS_NETWORK_GATEWAY=.*|PMOS_NETWORK_GATEWAY=10.201.77.126|' \
    -e 's|^PMOS_NETWORK_IP_RANGE=.*|PMOS_NETWORK_IP_RANGE=10.201.77.64/26|' "$WORK/full.env" >"$WORK/net.env"
  net=$(render "$WORK/net.env") || fail "compose doesn't render with another subnet"
  [[ "$net" == *'subnet: 10.201.77.0/25'* && "$net" == *'gateway: 10.201.77.126'* && "$net" == *'ip_range: 10.201.77.64/26'* ]] ||
    fail "the pmos network's subnet, gateway and ip_range can't be overridden"
  # Slice H2: the five pinned addresses, the ip_range and the pmos0 bridge, with every profile;
  # nothing else pinned; `config -q` accepts them.
  docker compose --project-directory "$DEPLOY_SRC" -f "$DEPLOY_SRC/compose.yaml" --env-file "$WORK/full.env" \
    --profile web --profile console --profile slack --profile models --profile bringup --profile bringup-personal --profile cli \
    config --format json >"$WORK/pinned.json" || fail "compose doesn't render every profile"
  docker compose --project-directory "$DEPLOY_SRC" -f "$DEPLOY_SRC/compose.yaml" --env-file "$WORK/full.env" \
    --profile models config -q || fail "compose config -q refused the pinned addresses"
  python3 -I - "$WORK/pinned.json" <<'PY' || fail "the pinned addresses, ip_range or bridge render wrong"
import json, sys
c = json.load(open(sys.argv[1]))
want = {"toolserver": "10.201.77.10", "gateway": "10.201.77.11", "bots": "10.201.77.12",
        "vllm-default": "10.201.77.13", "vllm-extraction": "10.201.77.14"}
for name, svc in c["services"].items():
    got = ((svc.get("networks") or {}).get("pmos") or {}).get("ipv4_address")
    assert got == want.get(name), (name, got)
for name in want:
    aliases = c["services"][name]["networks"]["pmos"]["aliases"]
    assert aliases == [name + ".pmos.internal"], (name, aliases)
cli = c["services"]["toolserver-cli"]
assert cli["networks"] == {"pmos": None}, cli["networks"]
assert not cli.get("ports"), cli.get("ports")
net = c["networks"]["pmos"]
assert net["driver_opts"] == {"com.docker.network.bridge.name": "pmos0"}, net
assert net["ipam"]["config"] == [{"subnet": "10.201.77.0/24", "gateway": "10.201.77.1",
                                  "ip_range": "10.201.77.128/25"}], net["ipam"]
PY
  # The first Microsoft sign-in (U1): the tool server gets the web app's tenant and client id
  # (it validates the ID token itself), never its client secret.
  entra_env="$WORK/entra.env"
  sed -e 's|^PMOS_WEB_ENTRA_TENANT_ID=.*|PMOS_WEB_ENTRA_TENANT_ID=11111111-2222-3333-4444-555555555555|' \
    -e 's|^PMOS_WEB_ENTRA_CLIENT_ID=.*|PMOS_WEB_ENTRA_CLIENT_ID=0b7f0c6e-1111-4222-8333-444455556666|' \
    -e 's|^PMOS_WEB_ENTRA_CLIENT_SECRET=.*|PMOS_WEB_ENTRA_CLIENT_SECRET=entra-secret-for-the-render-check|' \
    "$WORK/full.env" >"$entra_env"
  entra=$(render "$entra_env" --profile web) || fail "compose doesn't render with Entra settings"
  # Only the tool server's environment has PMOS_ENTRA_* (the web app's is PMOS_WEB_ENTRA_*).
  [[ "$entra" == *'PMOS_ENTRA_TENANT_ID: 11111111-2222-3333-4444-555555555555'* ]] ||
    fail "the tool server doesn't get the Entra tenant id"
  [[ "$entra" == *'PMOS_ENTRA_CLIENT_ID: 0b7f0c6e-1111-4222-8333-444455556666'* ]] ||
    fail "the tool server doesn't get the Entra client id"
  [ "$(grep -c 'entra-secret-for-the-render-check' <<<"$entra")" = 1 ] ||
    fail "the Entra client secret reaches more than the web app"
  [[ "$entra" != *PMOS_ENTRA_CLIENT_SECRET* ]] || fail "the tool server gets the Entra client secret"
  # Production compose never passes the development override (R3), even when set.
  echo "PMOS_ASSERTION_INSECURE_OFF=1" >>"$WORK/full.env"
  full=$(render "$WORK/full.env" --profile web --profile console --profile slack) ||
    fail "compose doesn't render with the override in deploy/.env"
  [[ "$full" != *"PMOS_ASSERTION_INSECURE_OFF"* ]] ||
    fail "deploy/compose.yaml passes PMOS_ASSERTION_INSECURE_OFF"
  sed -i '/^PMOS_ASSERTION_INSECURE_OFF=/d' "$WORK/full.env"
  # The default profile with only the core settings: no web, console or assertion settings
  # are needed, and none is half there (C9).
  grep -vE '^PMOS_(RT_WEB_TOKEN|TS_WEB_TOKEN|RT_CONSOLE_TOKEN|CONSOLE_TS_TOKEN|AK_[A-Z_]+|ASSERTION_MODE|WEB_BRIEF_TOKEN|CONSOLE_PROXY_SECRET)=' \
    "$WORK/full.env" >"$WORK/core.env"
  core=$(render "$WORK/core.env") || fail "the default profile needs web or assertion settings"
  for unwanted in 'web=' 'console=' 'web:bots:' 'console:bots:' 'web:toolserver:' 'console:toolserver:'; do
    [[ "$core" != *"$unwanted"* ]] || fail "the default profile without web settings still has $unwanted"
  done
  # The published ports follow PMOS_*_PORT; the hosted-test overlays give only the gateway the
  # remote key, the proxy, the test config and (with its overlay) the CA bundle.
  ports=$(render "$WORK/core.env" --profile web) || fail "compose doesn't render the default ports"
  [[ "$ports" == *'host_ip: 127.0.0.1'*'published: "8770"'* ]] || fail "the web app isn't on 127.0.0.1:8770 by default"
  { cat "$WORK/full.env"; echo "PMOS_WEB_PORT=18770"; echo "PMOS_GATEWAY_REMOTE_KEY=remote-key-for-the-render-check"; echo "PMOS_GATEWAY_CA_FILE=$WORK/ca.crt"; } >"$WORK/ht.env"
  ht_render=$(docker compose --project-directory "$DEPLOY_SRC" -f "$DEPLOY_SRC/compose.yaml" \
    -f "$DEPLOY_SRC/compose.hosted-test.yaml" -f "$DEPLOY_SRC/compose.gateway-ca.yaml" \
    --env-file "$WORK/ht.env" --profile web config --format json) || fail "the hosted-test overlays don't render"
  printf '%s\n' "$ht_render" >"$WORK/ht.json"
  python3 -I - "$WORK/ht.json" "$WORK/ca.crt" "$WORK/state" <<'PY' || fail "the hosted-test overlays render wrong"
import json, sys
c = json.load(open(sys.argv[1]))["services"]
ca, state = sys.argv[2], sys.argv[3]
gw = c["gateway"]
assert gw["environment"]["PMOS_GATEWAY_CLOUD_TEST"] == "1"
assert gw["environment"]["SSL_CERT_FILE"] == "/etc/pmos/ca-bundle.crt"
vols = {v["target"]: v["source"] for v in gw["volumes"]}
assert vols["/etc/pmos/gateway.yaml"] == state + "/gateway.hosted-test.yaml", vols
assert vols["/etc/pmos/ca-bundle.crt"] == ca, vols
for name, svc in c.items():
    env = json.dumps(svc.get("environment", {}))
    if name != "gateway":
        assert "remote-key-for-the-render-check" not in env, name
        assert "SSL_CERT_FILE" not in env, name
assert c["web"]["ports"][0]["published"] == "18770" and c["web"]["ports"][0]["host_ip"] == "127.0.0.1"
PY
  # The internal route (H9): the web app's second listener is set, on 8771, and nothing publishes
  # it; the Slack service's brief URL is the kit's own, from deploy/.env, and it is not the
  # browser-facing port by default.
  internal=$(docker compose --project-directory "$DEPLOY_SRC" -f "$DEPLOY_SRC/compose.yaml" \
    --env-file "$WORK/full.env" --profile web --profile slack config --format json) ||
    fail "compose doesn't render the web app's internal listener"
  printf '%s\n' "$internal" >"$WORK/internal.json"
  python3 -I - "$WORK/internal.json" <<'PY' || fail "the web app's internal listener renders wrong"
import json, sys
c = json.load(open(sys.argv[1]))["services"]
web = c["web"]
assert web["environment"]["PMOS_WEB_BIND"] == "0.0.0.0:8770", web["environment"]
assert web["environment"]["PMOS_WEB_INTERNAL_BIND"] == "0.0.0.0:8771", web["environment"]
assert len(web["ports"]) == 1 and web["ports"][0]["target"] == 8770 and web["ports"][0]["host_ip"] == "127.0.0.1", web["ports"]
for name, svc in c.items():
    for p in svc.get("ports", []):
        assert p["target"] != 8771 and str(p.get("published")) != "8771", (name, p)
    if name != "web":
        assert "PMOS_WEB_INTERNAL_BIND" not in svc.get("environment", {}), name
assert "web.pmos.internal" in web["networks"]["pmos"]["aliases"], web["networks"]
PY
  # PMOS_WEB_BRIEF_URL reaches the Slack service as PMOS_WEB_URL, unchanged.
  sed -i 's|^PMOS_WEB_BRIEF_URL=.*|PMOS_WEB_BRIEF_URL=http://web.pmos.internal:8771|' "$WORK/full.env"
  brief=$(render "$WORK/full.env" --profile web --profile slack) || fail "compose doesn't render with the brief URL"
  [[ "$brief" == *'PMOS_WEB_URL: http://web.pmos.internal:8771'* ]] || fail "the Slack service doesn't get PMOS_WEB_BRIEF_URL as PMOS_WEB_URL"
  # env.example documents 8771 as the brief URL and 8770 only as the route that answers 404.
  grep -qF 'PMOS_WEB_BRIEF_URL=http://web.pmos.internal:8771' "$DEPLOY_SRC/env.example" || fail "env.example doesn't document the brief URL on 8771"
  ! grep -qF 'PMOS_WEB_BRIEF_URL=http://web.pmos.internal:8770' "$DEPLOY_SRC/env.example" || fail "env.example still documents the brief URL on 8770"
  echo "compose renders checked"
else
  echo "compose renders skipped: no docker compose here (CI's deploy job has it)"
fi

echo "pmos-admin checks passed"
