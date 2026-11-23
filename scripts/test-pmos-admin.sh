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
set -euo pipefail

SRC="$(cd "$(dirname "$0")/.." && pwd)/deploy/pmos-admin"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/pmos-admin-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

bash -n "$SRC"

mkdir -p "$WORK/repo/deploy" "$WORK/pr"
cp "$SRC" "$WORK/repo/deploy/pmos-admin"
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
cp "$SRC" "$WORK/stack/deploy/pmos-admin"
cp "$DEPLOY_SRC/systemd/pmos-worker@.service" "$WORK/stack/deploy/systemd/"
cat >"$WORK/bin/docker" <<'EOF'
#!/usr/bin/env bash
envfile=""
prev=""
for a in "$@"; do [ "$prev" = --env-file ] && envfile=$a; prev=$a; done
filemode=$(grep -E '^PMOS_ASSERTION_MODE=' "$envfile" 2>/dev/null | cut -d= -f2-)
printf 'mode=%s file=%s workers=%s args=%s\n' "${PMOS_ASSERTION_MODE:-}" "$filemode" \
  "${PMOS_RT_WORKER_TOKENS:-}" "$*" >>"$DOCKER_LOG"
# `docker network inspect`: the pmos network's gateway, when the test gives one.
if [ "${1:-}" = network ] && [ -n "${FAKE_GATEWAY:-}" ]; then echo "$FAKE_GATEWAY"; fi
# `compose … ps --services`: the running services, when the test gives them.
if [[ " $* " == *" ps "*"--services"* ]] && [ -n "${FAKE_PS:-}" ]; then echo "$FAKE_PS"; fi
# `compose … config --services`: the stack's services (logs and restart check against them).
if [[ " $* " == *" config --services "* ]]; then
  printf '%s\n' ${FAKE_SERVICES:-postgres docs toolserver gateway bots slack console web worker-docs vllm-default vllm-extraction}
fi
# What reached docker through its environment, never its arguments (S1).
printf 'evals_token=%s https_proxy=%s\n' "${PMOS_EVALS_TOKEN-unset}" "${HTTPS_PROXY-unset}" >>"${DOCKER_ENV_LOG:-/dev/null}"
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
    printf '%s\n' "$@"
  } >"$WORK/stack/deploy/.env"
  chmod 600 "$WORK/stack/deploy/.env"
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
stack_env require "PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=0" "PMOS_WEB_PASSWORD_LOGIN=1" \
  "PMOS_NETWORK_SUBNET=10.50.0.0/24" "PMOS_NETWORK_GATEWAY=10.50.0.1" \
  "PMOS_WEB_TRUSTED_PROXIES=10.50.0.1" "PMOS_WEB_PUBLIC_URL=https://pmos.example.com"
FAKE_GATEWAY=10.50.0.1 run_stack up --web >/dev/null || fail "an overridden network was refused"
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
grep -qF -- "run --rm --no-deps -e PMOS_HOST_USER=$me -T toolserver pmos-toolserver users list" "$DOCKER_LOG" ||
  fail "users list didn't pass PMOS_HOST_USER=$me: $(cat "$DOCKER_LOG")"
: >"$DOCKER_LOG"
# A password setup code (docs/password-sign-in.md): the host's own command, with its host user.
run_stack users password-setup raj </dev/null >/dev/null
grep -qF -- "-e PMOS_HOST_USER=$me -T toolserver pmos-toolserver users password-setup raj" "$DOCKER_LOG" ||
  fail "users password-setup didn't run with PMOS_HOST_USER: $(cat "$DOCKER_LOG")"
SUDO_USER=alice run_stack users deactivate ana </dev/null >/dev/null
grep -qF -- "-e PMOS_HOST_USER=alice -T toolserver pmos-toolserver users deactivate ana" "$DOCKER_LOG" ||
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
grep -qF -- "-v $WORK/old-members.yaml:/run/pmos-import/members.yaml:ro toolserver pmos-toolserver users import --commit abc1234 --bootstrap --apply --from /run/pmos-import/members.yaml" "$DOCKER_LOG" ||
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
for bad in PMOS_UID=0 PMOS_GID=0 PMOS_UID=00 PMOS_UID=abc PMOS_UID=-1 "PMOS_GID=10 01" PMOS_UID=12345678901; do
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
[ "$(diff "$DEPLOY_SRC/systemd/pmos-worker@.service" "$UNIT" | grep -c '^[<>]')" = 2 ] ||
  fail "the installed unit differs from the template in more than the ExecStart line"
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
    mv "$WORK/env.aside" "$WORK/stack/deploy/.env"
  fi
done
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
grep -q 'https_proxy=http://proxyuser:prx0SECRET@proxy.example:3128$' "$DOCKER_ENV_LOG" || fail "build didn't give docker the proxy in its environment"
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
  ln "$envf" "$WORK/env.old"
  run_bot add-bot scout2 >/dev/null 2>&1 || fail "add-bot scout2 failed"
  [ "$(stat -c %h "$envf")" = 1 ] || fail "add-bot scout2 rewrote deploy/.env in place, not by a rename"
  rm -f "$WORK/env.old"
else
  echo "the ACL check skipped: setfacl isn't installed here, or this filesystem has no ACLs"
fi
# The mode of the file add-bot made (a new file, renamed into place, checked above), as it made it.
[ "$(stat -c %a "$envf")" = 600 ] || fail "deploy/.env isn't 0600 after add-bot: $(stat -c %a "$envf")"
[ -e "$WORK/stack/deploy/.env.lock" ] || fail "add-bot took no lock"
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
[ "$code" != 0 ] && [[ "$out" == *"flock (util-linux) isn't installed"* ]] || fail "setup.sh without flock exits $code: $out"
cmp -s "$WORK/senv.before" "$senv" || fail "setup.sh without flock changed deploy/.env"

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
  # M1: the pmos network's subnet and gateway are pinned, and can be overridden.
  [[ "$full" == *'subnet: 10.201.77.0/24'* && "$full" == *'gateway: 10.201.77.1'* ]] ||
    fail "the pmos network's subnet and gateway aren't pinned"
  sed -e 's|^PMOS_NETWORK_SUBNET=.*|PMOS_NETWORK_SUBNET=10.50.0.0/24|' \
    -e 's|^PMOS_NETWORK_GATEWAY=.*|PMOS_NETWORK_GATEWAY=10.50.0.1|' "$WORK/full.env" >"$WORK/net.env"
  net=$(render "$WORK/net.env") || fail "compose doesn't render with another subnet"
  [[ "$net" == *'subnet: 10.50.0.0/24'* && "$net" == *'gateway: 10.50.0.1'* ]] ||
    fail "the pmos network's subnet and gateway can't be overridden"
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
  echo "compose renders checked"
else
  echo "compose renders skipped: no docker compose here (CI's deploy job has it)"
fi

echo "pmos-admin checks passed"
