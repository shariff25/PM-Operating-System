#!/usr/bin/env bash
# Checks deploy/pmos-admin without Docker or a stack (CI's deploy job runs it):
# - `evals-gate` parses `--checkout` and `--base`, defaults to the script's own repo, and
#   refuses unknown options (a dry parse, on a copy of the script with a throwaway deploy/.env);
# - the eval gate treats a checkout as data: every line of the gate and of eval_stack_up that
#   touches the checkout is one of the reviewed forms (git on it, a copy of its content/, a
#   read-only check, the runner's mount). A new use fails here until it is reviewed.
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
mkdir -p "$WORK/bin" "$WORK/stack/deploy"
cp "$SRC" "$WORK/stack/deploy/pmos-admin"
cat >"$WORK/bin/docker" <<'EOF'
#!/usr/bin/env bash
envfile=""
prev=""
for a in "$@"; do [ "$prev" = --env-file ] && envfile=$a; prev=$a; done
filemode=$(grep -E '^PMOS_ASSERTION_MODE=' "$envfile" 2>/dev/null | cut -d= -f2-)
printf 'mode=%s file=%s workers=%s args=%s\n' "${PMOS_ASSERTION_MODE:-}" "$filemode" \
  "${PMOS_RT_WORKER_TOKENS:-}" "$*" >>"$DOCKER_LOG"
EOF
chmod +x "$WORK/bin/docker"
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
printf 'members: []\n' >"$WORK/team.yaml"
: >"$DOCKER_LOG"
run_stack users import --commit abc1234 --from "$WORK/team.yaml" --bootstrap --apply </dev/null >/dev/null
grep -qF -- "-v $WORK/team.yaml:/run/pmos-import/team.yaml:ro toolserver pmos-toolserver users import --commit abc1234 --bootstrap --apply --from /run/pmos-import/team.yaml" "$DOCKER_LOG" ||
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
