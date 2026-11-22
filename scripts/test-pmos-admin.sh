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
set -euo pipefail

SRC="$(cd "$(dirname "$0")/.." && pwd)/deploy/pmos-admin"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/pmos-admin-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

bash -n "$SRC"

mkdir -p "$WORK/repo/deploy" "$WORK/pr"
cp "$SRC" "$WORK/repo/deploy/pmos-admin"
: >"$WORK/repo/deploy/.env"
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
  'changed=$(git -C "$checkout" diff --name-only "$base"...HEAD)'
  'eval_stack_up "$checkout"'
  'git -C "$checkout" archive "$base" evals/reports | tar -x -C "$PMOS_EVAL_BASELINE"'
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
EOF
chmod +x "$WORK/bin/docker"
# A stand-in `curl` for `status`: every /readyz answers FAKE_READYZ with 200.
cat >"$WORK/bin/curl" <<'EOF'
#!/usr/bin/env bash
printf '%s\n200' "${FAKE_READYZ:-{\}}"
EOF
chmod +x "$WORK/bin/curl"
export DOCKER_LOG="$WORK/docker.log"
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
  : >"$DOCKER_LOG"
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
case "$*" in -u) echo "${FAKE_UID:-1000}" ;; -un) echo pmosop ;; *) exec /usr/bin/id "$@" ;; esac
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
[ -z "$verify" ] || fail "systemd-analyze verify warned: $verify"
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
: >"$OPENSHELL_LOG"
FAKE_SYSTEMCTL_FAIL=1 run_bot pause pm-ana >/dev/null 2>&1 || fail "pause failed when systemctl did"
[ "$(cat "$OPENSHELL_LOG")" = "args=sandbox stop -n pmos-pm-ana worker= gateway=" ] ||
  fail "pause didn't stop the sandbox when systemctl failed"

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
  echo "compose renders checked"
else
  echo "compose renders skipped: no docker compose here (CI's deploy job has it)"
fi

echo "pmos-admin checks passed"
