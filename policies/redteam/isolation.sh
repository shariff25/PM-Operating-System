#!/usr/bin/env bash
# PM OS sandbox red-team: isolation between two bots' OpenShell sandboxes (D3, POL-*), and what
# a sandbox can reach on PM OS's host (slice H7, docs/openshell-any-host-spec.md §5).
#
# Run this on the host, never on a machine shared with other work. It can't run in CI: it
# needs a live OpenShell gateway, both bots' sandboxes, and the PM OS tool server, gateway and
# bot runtime behind *.pmos.internal.
#
#   policies/redteam/isolation.sh [--dry-run] [--allow-host-changes] [--only RT-n,…] <bot-a> <bot-b>
#       e.g. isolation.sh docs pm-ana
#
#   --dry-run             print every probe and command, run nothing in a sandbox, change
#                         nothing, read no key; needs neither OpenShell nor Docker
#   --allow-host-changes  let RT-13b change the host's /etc/hosts for a moment (below); without
#                         it RT-13b is skipped, and the run is not a pass
#   --only RT-1,RT-11     run only these checks (RT-10 runs with RT-9, whose denials it reads)
#
# It checks, from inside bot A's sandbox, that A:
#   - runs under the committed policy, non-root, with no_new_privs and no capabilities;
#   - can write only its workspace, and can't write its skills or master context;
#   - can't read bot B's files, see B's processes, or hold B's tokens;
#   - can't use B's credential placeholders against the gateway or the bot runtime;
#   - can't reach anything but toolserver.pmos.internal:8710/mcp, gateway.pmos.internal:8740
#     (chat) and bots.pmos.internal:8750 (the worker routes), and each blocked call is logged
#     with its destination and the policy that blocked it (RT-1 to RT-10);
#   - can't reach a cloud metadata service (RT-11), the host's own SSH, OpenShell gateway,
#     Docker API or Docker socket (RT-12), the console, Postgres, the network's gateway address
#     or a pinned address on another service's port (RT-13);
#   - can't reach the tool server's name once it resolves to another address, because each name
#     is pinned to its own address by `allowed_ips` (RT-13b, which changes /etc/hosts: below);
#   - can't reach the hosted model's provider, by name or by address, and holds no provider key
#     (RT-18).
# It never claims a task: the runtime controls use a heartbeat on a made-up task id.
# The checklist in checklist.md gives each check's id, and the items to do by hand.
#
# RT-13b, the only step that changes the host (with --allow-host-changes and sudo): it needs
# `deploy/pmos-admin host-names check` to pass first; it then rewrites the tool server's line in
# the managed block of /etc/hosts (by a rename), first to 10.201.77.11, then to 10.201.77.10's
# neighbour network 10.201.78.10. pmos-admin has no override for this: the script writes the line
# itself, and a trap runs `sudo deploy/pmos-admin host-names apply` on every exit (an error and
# Ctrl-C included), which puts the managed block back. While the line is there `host-names check`
# fails, so `pmos-admin sandbox` refuses to start a sandbox.
#
# RT-18 reads the hosted provider's host from deploy/.env (PMOS_HOSTED_TEST_URL under
# hosted-test; PMOS_GATEWAY_HOSTED_URL under hosted, slice H12) and its key (PMOS_GATEWAY_REMOTE_KEY;
# under hosted the key file deploy/secrets/hosted-model.key). Neither the host nor the key is
# printed: the key's first 8 characters go to the sandbox on standard input, a grep there reads
# them as its pattern (never as an argument), and the script prints only "found" or "not found".
#
# What it expects from OpenShell (from its docs at github.com/NVIDIA/OpenShell, main at
# 2e5589b; anything marked VERIFY is an assumption to confirm on a real host):
#   - `openshell sandbox exec -n <name> --no-login-shell -- <cmd>` runs <cmd> in the sandbox
#     as the workload identity, under its Landlock rules, with stdin passed through.
#   - `openshell policy get <name> --base|--full --output json` prints {"policy": {...}}.
#   - `openshell logs <name> --since <d> --source sandbox` prints OCSF shorthand lines such as
#       OCSF NET:OPEN [MED] DENIED /usr/local/bin/node(64) -> slack.com:443 [policy:- engine:opa] [reason:no matching policy]
#       OCSF HTTP:POST [MED] DENIED POST http://toolserver.pmos.internal:8710/mcp [policy:pmos_toolserver engine:opa]
#     A connection no rule matches is logged with `policy:-` (the sandbox's default deny); a
#     request an endpoint's rules refuse names that rule. Either way the policy is the
#     sandbox's active revision, which this script records from `openshell policy list`.
#   - Provider credentials appear in the sandbox environment as opaque placeholders
#     (`openshell:resolve:env:...`); the proxy swaps in the real value only for the endpoint
#     the provider is bound to, and an unknown or foreign placeholder fails closed.
#   - Denied requests get `403` with a JSON body whose `error` is `policy_denied` (or
#     `credential_endpoint_mismatch` for a credential bound elsewhere).
#   VERIFY: the transparent proxy covers Node's built-in fetch with no proxy settings.
#   VERIFY: a sandbox resolves the three names through the host's /etc/hosts (RT-13b).
#   For RT-11 to RT-13b and RT-18 a probe passes on a 403 policy_denied, or on no connection
#   together with a DENIED log line for that destination; anything that answers fails, and no
#   connection without a log line is inconclusive, which fails.
#
# Environment:
#   OPENSHELL       openshell CLI (default: openshell)
#   POLICIES        the policies directory (default: this script's ../)
#   WORKER_BINARY   node in the worker image (default: /usr/local/bin/node; VERIFY)
#   CHAT_MODEL      a gateway model alias for the positive control (default: pmos/default)
#   DENIED_TOOL     an MCP tool bot A's role lacks (default: jira_search)
#   B_REAL_TOKEN    optional: bot B's real gateway token, to search A's sandbox for it
#   RT13B_SETTLE    seconds to wait after each hosts change (default 5)
#
# Exit status (lib.sh): 0 all checks passed, 1 a check failed, 2 the script couldn't run,
# 3 nothing failed but a check was skipped (not a pass). A dry run exits 0.

set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=policies/redteam/lib.sh
. "$HERE/lib.sh"

OPENSHELL=${OPENSHELL:-openshell}
POLICIES=${POLICIES:-$(cd "$HERE/.." && pwd)}
WORKER_BINARY=${WORKER_BINARY:-/usr/local/bin/node}
CHAT_MODEL=${CHAT_MODEL:-pmos/default}
DENIED_TOOL=${DENIED_TOOL:-jira_search}
RT13B_SETTLE=${RT13B_SETTLE:-5}
# Your public docs site (PMOS_DOCS_BASE_URL in deploy/.env); any outside host works if unset.
DOCS_SITE=${PMOS_DOCS_BASE_URL:-https://docs.example.com}
TOOLSERVER=http://toolserver.pmos.internal:8710
GATEWAY=http://gateway.pmos.internal:8740
RUNTIME=http://bots.pmos.internal:8750
WORKSPACE=/sandbox/workspace
# A task id that can't exist, so worker calls with a valid token get the runtime's 404.
FAKE_TASK=tsk_00000000000000000000000000000000
# The host's hosts file. The one line that names it; nothing (no setting, no argument) points
# the script elsewhere. scripts/test-redteam.sh changes this line in its copy, and only there.
readonly HOSTS_FILE=/etc/hosts
PMOS_ADMIN="$REPO/deploy/pmos-admin"
# The pinned addresses (deploy/compose.yaml; crates/policy/src/lib.rs).
TOOLSERVER_NAME=toolserver.pmos.internal
TOOLSERVER_ADDR=10.201.77.10
GATEWAY_ADDR=10.201.77.11
NETWORK_GATEWAY=10.201.77.1
OUTSIDE_ADDR=10.201.78.10
NETWORK_NAME=pmos_pmos

usage() {
  echo "usage: $0 [--dry-run] [--allow-host-changes] [--only RT-n,…] <bot-a> <bot-b>" >&2
  exit 2
}
args=()
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY=1; shift ;;
    --allow-host-changes) ALLOW_CHANGES=1; shift ;;
    --only) [ $# -ge 2 ] || usage; ONLY=$2; shift 2 ;;
    -h | --help) usage ;;
    -*) usage ;;
    *) args+=("$1"); shift ;;
  esac
done
[ ${#args[@]} -eq 2 ] || usage
A=${args[0]}
B=${args[1]}
# PM OS's bot names (deploy/pmos-admin valid_bot_name), checked before they reach a command.
valid_bot_name() {
  [[ "$1" =~ ^[a-z0-9]+(-[a-z0-9]+)*$ ]] && [ ${#1} -le 32 ]
}
for n in "$A" "$B"; do
  valid_bot_name "$n" || { echo "$(printf '%q' "$n") isn't a bot name (lowercase letters, digits and single hyphens, at most 32 characters)" >&2; exit 2; }
done
[ "$A" != "$B" ] || { echo "bot A and bot B must differ" >&2; exit 2; }
if [ -n "$ONLY" ] && ! [[ "$ONLY" =~ ^RT-[0-9]+b?(,RT-[0-9]+b?)*$ ]]; then
  echo "--only takes check ids such as RT-11,RT-13b" >&2
  exit 2
fi
SA=pmos-$A
SB=pmos-$B

# --- putting the host back ------------------------------------------------------------------

SWAPPED=0
SLEEPER=""
SCRATCH=$(mktemp -d "${TMPDIR:-/tmp}/pmos-redteam.XXXXXX") || exit 2
# shellcheck disable=SC2329 # run by the EXIT trap
on_exit() {
  local code=$?
  if [ -n "$SLEEPER" ]; then
    kill "$SLEEPER" >/dev/null 2>&1
    wait "$SLEEPER" 2>/dev/null
  fi
  if [ "$SWAPPED" = 1 ]; then
    echo "putting the managed hosts block back: sudo $PMOS_ADMIN host-names apply" >&2
    if ! sudo "$PMOS_ADMIN" host-names apply >&2; then
      echo "COULDN'T put /etc/hosts back: run sudo deploy/pmos-admin host-names apply now; until then pmos-admin sandbox refuses" >&2
      code=1
    fi
  fi
  rm -rf "$SCRATCH"
  exit "$code"
}
trap on_exit EXIT
trap 'exit 130' INT TERM HUP

# --- running things in a sandbox --------------------------------------------------------------

# openshell, or in a dry run the command it would be.
os() {
  if [ "$DRY" = 1 ]; then
    dry openshell "$*"
    return 0
  fi
  "$OPENSHELL" "$@"
}

# Run a command in a sandbox. Never put secrets in its arguments: pass them on stdin.
sx() {
  local sandbox=$1 c
  shift
  if [ "$DRY" = 1 ]; then
    c="$*"
    c=${c%%$'\n'*}
    dry exec "in $sandbox: ${c:0:110}"
    return 0
  fi
  "$OPENSHELL" sandbox exec -n "$sandbox" --no-login-shell -- "$@"
}

# An HTTP request from inside a sandbox, made by the worker binary (the only one allowed out).
# Prints "STATUS <code> <body...>" or "ERROR <reason>". The request travels on stdin; its
# parts reach jq through the environment, not its arguments. HTTP_SHOWN, when set, is what a dry
# run prints instead of the URL.
http_from() {
  local sandbox=$1 method=$2 url=$3 token=${4:-} body=${5:-}
  local req
  if [ "$DRY" = 1 ]; then
    dry http "from $sandbox: ${HTTP_SHOWN:-$method $url}"
    return 0
  fi
  req=$(M=$method U=$url T=$token Q=$body jq -cn '{method: env.M, url: env.U, token: env.T, body: env.Q}')
  printf '%s\n' "const q = $req;
const h = {'content-type': 'application/json', 'accept': 'application/json, text/event-stream',
           'mcp-protocol-version': '2025-06-18'};
if (q.token) h.authorization = 'Bearer ' + q.token;
fetch(q.url, {method: q.method, headers: h, body: q.body || undefined, redirect: 'manual'})
  .then(async r => { const t = await r.text(); console.log('STATUS ' + r.status + ' ' + t.slice(0, 300).replace(/\s+/g, ' ')); })
  .catch(e => console.log('ERROR ' + ((e.cause && e.cause.code) || e.message)));" |
    sx "$sandbox" "$WORKER_BINARY" -
}

status_of() { awk '/^STATUS/ {print $2; exit} /^ERROR/ {print "error"; exit}'; }

mcp_call() { # id method params-json
  I=$1 M=$2 P=$3 jq -cn '{jsonrpc: "2.0", id: (env.I | tonumber), method: env.M, params: (env.P | fromjson)}'
}

chat_body() {
  M=$CHAT_MODEL jq -cn '{model: env.M, max_tokens: 1, messages: [{role: "user", content: "ping"}]}'
}

# --- preflight -------------------------------------------------------------------------------

need=(jq)
[ "$DRY" = 1 ] || need+=("$OPENSHELL")
for tool in "${need[@]}"; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "missing $tool" >&2
    exit 2
  fi
done
for f in "$POLICIES/bots/$A.yaml" "$POLICIES/bots/$A.sandbox.yaml"; do
  if [ ! -f "$f" ]; then
    echo "missing $f (run \`pmos policy generate\` and commit it)" >&2
    exit 2
  fi
done
if [ "$DRY" = 0 ]; then
  for sb in "$SA" "$SB"; do
    if ! "$OPENSHELL" sandbox get "$sb" >/dev/null 2>&1; then
      echo "sandbox $sb isn't running; create it from policies/bots/ first (see docs/sandbox-policies.md)" >&2
      exit 2
    fi
  done
fi

echo "PM OS sandbox red-team: A=$SA B=$SB ($(date -u +%Y-%m-%dT%H:%M:%SZ))${ONLY:+, only $ONLY}"
[ "$DRY" = 0 ] || echo "DRY RUN: the probes below are printed, not run"
commit_line
REVISION=$(os policy list "$SA" 2>/dev/null | head -5)
note "A's policy revisions (newest first):"
[ "$DRY" = 1 ] || printf '%s\n' "$REVISION" | sed 's/^/       /'

# --- RT-1: A runs under the committed policy, and nothing else adds to it --------------------

rt_1() {
  section RT-1 "A's base and effective policies: only the three PM OS rules, bound to A's providers"
  local base full want_rules got_rules got_full hosts bound want_bound compat
  base=$(os policy get "$SA" --base --output json 2>/dev/null | jq -c '.policy // empty' 2>/dev/null)
  full=$(os policy get "$SA" --full --output json 2>/dev/null | jq -c '.policy // empty' 2>/dev/null)
  [ "$DRY" = 0 ] || return 0
  if [ -z "$base" ]; then
    fail RT-1 "couldn't read A's base policy"
    return
  fi
  want_rules='["pmos_gateway","pmos_runtime","pmos_toolserver"]'
  got_rules=$(jq -c '.network_policies | keys' <<<"$base")
  got_full=$(jq -c '.network_policies | keys' <<<"${full:-$base}")
  hosts=$(jq -c '[.network_policies[].endpoints[] | "\(.host):\(.port)"] | sort' <<<"$base")
  # Per rule, the providers its endpoints bind (the tool server binds none in v1).
  bound=$(jq -cS '.network_policies | map_values([.endpoints[].credential_binding.provider // empty])' <<<"$base")
  want_bound=$(jq -cSn --arg a "$A" \
    '{pmos_gateway: ["pmos-\($a)-gateway"], pmos_runtime: ["pmos-\($a)-worker"], pmos_toolserver: []}')
  compat=$(jq -r '.landlock.compatibility // "missing"' <<<"$base")
  if [ "$got_rules" = "$want_rules" ] && [ "$got_full" = "$want_rules" ] &&
    [ "$hosts" = '["bots.pmos.internal:8750","gateway.pmos.internal:8740","toolserver.pmos.internal:8710"]' ] &&
    [ "$bound" = "$want_bound" ] && [ "$compat" = hard_requirement ]; then
    pass RT-1 "A's base and effective policies have only the three PM OS rules, bound to A's own providers"
  else
    fail RT-1 "A's policy differs from policies/bots/$A.yaml: rules=$got_rules effective=$got_full hosts=$hosts bound=$bound landlock=$compat"
  fi
}

# --- RT-2: agents can't propose policy changes -------------------------------------------------

rt_2() {
  section RT-2 "the policy advisor is off for A"
  local settings
  settings=$(os settings get "$SA" 2>&1 || true)
  if grep -Eiq 'agent_policy_proposals_enabled[^a-z]*true' <<<"$settings"; then
    fail RT-2 "the policy advisor is on for $SA: agents could propose network rules outside POL-2 review"
  else
    pass RT-2 "the policy advisor is off for $SA (VERIFY the settings output format)"
  fi
}

# --- RT-3: non-root, no_new_privs, no capabilities --------------------------------------------

rt_3() {
  section RT-3 "A runs non-root, with no_new_privs, no capabilities and no working sudo"
  local ident uid
  ident=$(sx "$SA" sh -c 'id -u; grep -E "^(NoNewPrivs|CapEff):" /proc/self/status' 2>&1)
  uid=$(head -1 <<<"$ident")
  if [ "$uid" != 0 ] && grep -Eq '^NoNewPrivs:[[:space:]]+1' <<<"$ident" &&
    grep -Eq '^CapEff:[[:space:]]+0+$' <<<"$ident"; then
    pass RT-3 "A runs as uid $uid with no_new_privs and no effective capabilities"
  else
    fail RT-3 "A's identity: $(tr '\n' ' ' <<<"$ident")"
  fi
  if sx "$SA" sh -c 'command -v sudo >/dev/null && sudo -n true' >/dev/null 2>&1 && [ "$DRY" = 0 ]; then
    fail RT-3 "sudo works in A"
  else
    pass RT-3 "sudo is absent or refused in A"
  fi
}

# --- RT-4: A writes only its workspace ------------------------------------------------------

# Non-destructive write probe: in a directory, create a uniquely named file and remove it at
# once; an existing file is opened for append and nothing is written. Prints writable, denied
# or absent.
# shellcheck disable=SC2016 # run by sh in the sandbox
PROBE='p=$1
if [ -d "$p" ]; then
  f="$p/.pmos-redteam-probe-$$"
  if ( : > "$f" ) 2>/dev/null; then rm -f "$f"; echo writable; else echo denied; fi
elif [ -e "$p" ]; then
  if ( : >> "$p" ) 2>/dev/null; then echo writable; else echo denied; fi
else
  echo absent
fi'
probe_write() {
  if [ "$DRY" = 1 ]; then dry exec "in $1: the write probe on $2"; return 0; fi
  sx "$1" sh -c "$PROBE" probe "$2" 2>/dev/null | tail -1
}

rt_4() {
  section RT-4 "A writes only its workspace, and reads its own skills and master context"
  local content path
  if [ "$(probe_write "$SA" "$WORKSPACE")" = writable ]; then
    pass RT-4 "A can write its workspace"
  else
    fail RT-4 "A can't write its own workspace ($WORKSPACE): the bot can't work"
  fi
  mapfile -t content < <(sed -n 's/^    to: //p' "$POLICIES/bots/$A.sandbox.yaml")
  for path in "${content[@]}" /sandbox /etc/passwd /usr/local /home /root /app; do
    case "$(probe_write "$SA" "$path")" in
      writable) fail RT-4 "A can write $path" ;;
      denied) pass RT-4 "A can't write $path" ;;
      *) note "skipped $path: not present in A" ;;
    esac
  done
  for path in "${content[@]}"; do
    if sx "$SA" sh -c "ls -d '$path' && { [ -d '$path' ] && ls '$path' || head -c1 '$path'; }" >/dev/null 2>&1; then
      pass RT-4 "A can read its own $path"
    else
      fail RT-4 "A can't read its own $path (upload or Landlock problem)"
    fi
  done
  if sx "$SA" ls /sandbox >/dev/null 2>&1 && [ "$DRY" = 0 ]; then
    fail RT-4 "A can list /sandbox; only the listed paths under it should be reachable"
  else
    pass RT-4 "A can't list /sandbox"
  fi
}

# --- RT-5: A can't read B's files ---------------------------------------------------------

rt_5() {
  section RT-5 "a canary written in B's workspace is nowhere in A's readable filesystem"
  local canary found
  canary="pmos-redteam-$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')"
  if printf '%s\n' "$canary" | sx "$SB" sh -c "read c; printf '%s' \"\$c\" > $WORKSPACE/.redteam-canary" >/dev/null 2>&1; then
    found=$(printf '%s\n' "$canary" |
      sx "$SA" sh -c 'grep -rlsIF --exclude-dir=proc -f - / 2>/dev/null | head -5' 2>/dev/null)
    if [ -z "$found" ]; then
      pass RT-5 "B's canary file is nowhere in A's readable filesystem"
    else
      fail RT-5 "A can read B's canary at: $(tr '\n' ' ' <<<"$found")"
    fi
    sx "$SB" rm -f "$WORKSPACE/.redteam-canary" >/dev/null 2>&1
  else
    fail RT-5 "couldn't plant a canary in B's workspace (inconclusive)"
  fi
}

# --- RT-6: A can't see B's processes --------------------------------------------------------

rt_6() {
  section RT-6 "A can't see B's processes"
  local marker seen
  marker=$((RANDOM * 32768 + RANDOM + 100000))
  sx "$SB" sleep "$marker" >/dev/null 2>&1 &
  SLEEPER=$!
  pause_for 3
  # The marker goes on stdin so no command line in A contains it; one line per process.
  # shellcheck disable=SC2016 # run by sh in the sandbox
  seen=$(printf '%s\n' "$marker" | sx "$SA" sh -c 'read m
    for f in /proc/[0-9]*/cmdline; do tr "\000" " " < "$f" 2>/dev/null; echo; done | grep -c "^sleep $m "' 2>/dev/null)
  if [ "${seen:-0}" = 0 ]; then
    pass RT-6 "A can't see B's processes"
  else
    fail RT-6 "A sees B's process 'sleep $marker'"
  fi
  kill "$SLEEPER" >/dev/null 2>&1
  wait "$SLEEPER" 2>/dev/null
  SLEEPER=""
}

# --- RT-7: A holds only placeholders, and only its own ---------------------------------------

TOKENS_LOADED=0
a_env="" b_gateway="" b_worker="" a_gateway="" a_worker=""
load_tokens() {
  [ "$TOKENS_LOADED" = 0 ] || return 0
  TOKENS_LOADED=1
  a_env=$(sx "$SA" env 2>/dev/null)
  b_gateway=$(sx "$SB" printenv PMOS_GATEWAY_TOKEN 2>/dev/null)
  b_worker=$(sx "$SB" printenv PMOS_WORKER_TOKEN 2>/dev/null)
  a_gateway=$(sed -n 's/^PMOS_GATEWAY_TOKEN=//p' <<<"$a_env")
  a_worker=$(sed -n 's/^PMOS_WORKER_TOKEN=//p' <<<"$a_env")
}

rt_7() {
  section RT-7 "A holds only OpenShell placeholders, and none of B's"
  local raw name leak t
  load_tokens
  raw=$(grep -E '^PMOS_[A-Z_]*TOKEN=' <<<"$a_env" | grep -v '=openshell:resolve:env:' | cut -d= -f1)
  if [ -z "$raw" ]; then
    pass RT-7 "every PMOS_*TOKEN in A is an OpenShell placeholder"
  else
    fail RT-7 "A holds raw tokens in: $(tr '\n' ' ' <<<"$raw")"
  fi
  for name in PMOS_GATEWAY_TOKEN PMOS_WORKER_TOKEN; do
    if grep -q "^$name=openshell:resolve:env:" <<<"$a_env"; then
      pass RT-7 "A's $name is a placeholder"
    else
      fail RT-7 "A's $name is missing or not a placeholder: the bot can't work, or holds a raw token"
    fi
  done
  # Service and Slack-bot tokens never; and no session provider in v1 (the per-task session
  # token comes in the claim response, in the worker's memory only).
  for name in PMOS_SERVICE_TOKEN PMOS_BOT_TOKEN PMOS_SESSION_TOKEN; do
    if grep -q "^$name=" <<<"$a_env"; then
      fail RT-7 "A has $name, which no bot sandbox may hold"
    else
      pass RT-7 "A has no $name"
    fi
  done
  leak=0
  for t in "$b_gateway" "$b_worker" "${B_REAL_TOKEN:-}"; do
    [ -n "$t" ] || continue
    if grep -qF -- "$t" <<<"$a_env"; then leak=1; fi
    if printf '%s\n' "$t" | sx "$SA" sh -c 'grep -rlsqIF --exclude-dir=proc -f - /' >/dev/null 2>&1; then leak=1; fi
  done
  if [ "$leak" = 0 ]; then
    pass RT-7 "none of B's tokens or placeholders is in A's environment or files"
  else
    fail RT-7 "one of B's tokens or placeholders is visible in A"
  fi
}

# --- RT-8: B's placeholders don't work from A -------------------------------------------------

rt_8() {
  section RT-8 "A's own credentials work (controls); B's placeholders don't, from A"
  local control got heartbeat
  load_tokens
  control=$(http_from "$SA" POST "$GATEWAY/v1/chat/completions" "$a_gateway" "$(chat_body)" | status_of)
  if [ "$control" = 200 ]; then
    pass RT-8 "control: A's own gateway credential works"
  else
    fail RT-8 "control: A's own gateway call returned ${control:-nothing}; the B checks below are inconclusive"
  fi
  if [ -n "$b_gateway" ] || [ "$DRY" = 1 ]; then
    got=$(http_from "$SA" POST "$GATEWAY/v1/chat/completions" "$b_gateway" "$(chat_body)" | status_of)
    if [ "$got" != 200 ]; then
      pass RT-8 "B's gateway placeholder is refused from A ($got)"
    else
      fail RT-8 "B's gateway placeholder works from A"
    fi
  fi
  # The runtime: a heartbeat on a task that can't exist. With a valid worker token the runtime
  # answers 404 (so the credential resolved), and nothing is claimed or changed.
  heartbeat="$RUNTIME/v1/worker/tasks/$FAKE_TASK/heartbeat"
  control=$(http_from "$SA" POST "$heartbeat" "$a_worker" '{}' | status_of)
  if [ "$control" = 404 ]; then
    pass RT-8 "control: A's own worker credential reaches the runtime (404 for a made-up task)"
  else
    fail RT-8 "control: A's own worker heartbeat returned ${control:-nothing}, not 404; the B check below is inconclusive"
  fi
  if [ -n "$b_worker" ] || [ "$DRY" = 1 ]; then
    got=$(http_from "$SA" POST "$heartbeat" "$b_worker" '{}' | status_of)
    if [ "$got" != 404 ] && [ "$got" != 200 ]; then
      pass RT-8 "B's worker placeholder is refused from A ($got)"
    else
      fail RT-8 "B's worker placeholder is accepted by the runtime from A ($got)"
    fi
  fi
}

# --- RT-9 and RT-10: blocked calls are blocked, and logged with destination and policy --------

# Blocked means OpenShell refused it (403 policy_denied) or the connection never opened. A 4xx
# from the service itself means the request got through the sandbox, which is a failure.
expect_denied() { # id url method token body description
  local id=$1 url=$2 method=$3 token=$4 body=$5 what=$6 got
  got=$(http_from "$SA" "$method" "$url" "$token" "$body")
  [ "$DRY" = 0 ] || return 0
  if grep -q '^ERROR' <<<"$got" || grep -Eq '^STATUS 403 .*(policy_denied|ssrf_denied)' <<<"$got"; then
    pass "$id" "$what is blocked ($(cut -c1-120 <<<"$got"))"
  else
    fail "$id" "$what got through the sandbox: $(cut -c1-200 <<<"$got")"
  fi
}
# The sandbox's log since a time (seconds since the epoch), with 30 s to spare.
logs_since() { os logs "$SA" --since "$(($(date -u +%s) - $1 + 30))s" --source sandbox 2>/dev/null; }

rt_9_10() {
  section RT-9 "from A, everything but the three PM OS endpoints is blocked"
  local since init call logs
  load_tokens
  init=$(mcp_call 1 initialize '{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"redteam","version":"0"}}')
  since=$(date -u +%s)
  expect_denied RT-9 https://slack.com/api/api.test GET "" "" "direct Slack from A"
  expect_denied RT-9 https://api.atlassian.com/me GET "" "" "direct Atlassian (Jira, Confluence) from A"
  expect_denied RT-9 "${DOCS_SITE%/}/" GET "" "" "the public docs site from A"
  expect_denied RT-9 http://docs.pmos.internal:8730/mcp POST "" "$init" "the docs server, bypassing the tool server"
  expect_denied RT-9 http://169.254.169.254/latest/meta-data/ GET "" "" "the cloud metadata address"
  expect_denied RT-9 "$TOOLSERVER/internal/call" POST "" '{"tool":"docs.search","args":{}}' "the tool server's /internal routes"
  expect_denied RT-9 "$GATEWAY/v1/models" GET "$a_gateway" "" "a gateway route other than chat"
  expect_denied RT-9 "$RUNTIME/v1/tasks" GET "$a_worker" "" "the runtime's task list"
  expect_denied RT-9 "$RUNTIME/v1/routines" GET "$a_worker" "" "the runtime's routines"
  expect_denied RT-9 "$RUNTIME/v1/notifications" GET "$a_worker" "" "the runtime's notification feed"
  expect_denied RT-9 "$RUNTIME/v1/worker/tasks/$FAKE_TASK/x/heartbeat" POST "$a_worker" '{}' "a worker route with an extra path segment"
  expect_denied RT-9 "$RUNTIME/v1/worker/claim" GET "$a_worker" "" "GET on the claim route"
  # No session token is needed: OpenShell refuses the tool name before the request reaches the
  # tool server.
  call=$(mcp_call 2 tools/call "$(T=$DENIED_TOOL jq -cn '{name: env.T, arguments: {}}')")
  expect_denied RT-9 "$TOOLSERVER/mcp" POST "" "$call" "tools/call $DENIED_TOOL, outside A's role"
  if sx "$SA" sh -c 'command -v curl' >/dev/null 2>&1; then
    if sx "$SA" curl -s -o /dev/null -m 10 "$GATEWAY/v1/chat/completions" >/dev/null 2>&1 && [ "$DRY" = 0 ]; then
      fail RT-9 "curl (not the worker binary) reached the gateway"
    else
      pass RT-9 "curl is not allowed to reach the gateway"
    fi
  fi

  section RT-10 "each RT-9 denial is in A's sandbox log as DENIED, with its destination and [policy:…]"
  pause_for 5
  logs=$(logs_since "$since")
  [ "$DRY" = 0 ] || return 0
  check_log() { # id destination-regex description
    local line
    line=$(grep -E 'DENIED' <<<"$logs" | grep -E -- "$2" | grep -E '\[policy:[^ ]+' | head -1)
    if [ -n "$line" ]; then
      pass "$1" "logged: $3"
      note "$line"
    else
      fail "$1" "no DENIED log line with destination and [policy:...] for $3"
    fi
  }
  check_log RT-10 'slack\.com(:443)?' "slack.com"
  check_log RT-10 'api\.atlassian\.com(:443)?' "api.atlassian.com"
  check_log RT-10 'docs\.pmos\.internal:8730' "docs.pmos.internal:8730"
  check_log RT-10 'toolserver\.pmos\.internal:8710/internal' "the tool server's /internal route"
  check_log RT-10 'bots\.pmos\.internal:8750/v1/tasks' "the runtime's task list"
  check_log RT-10 'bots\.pmos\.internal:8750/v1/routines' "the runtime's routines"
  check_log RT-10 'toolserver\.pmos\.internal:8710/mcp.*policy:pmos_toolserver|policy:pmos_toolserver.*toolserver\.pmos\.internal' "the out-of-role tool call, naming rule pmos_toolserver"
  note "The blocking policy is $SA's active revision above; 'policy:-' means no rule matched (default deny)."
}

# --- RT-11 to RT-13b and RT-18: probes judged together with A's log ---------------------------

# What a probe's answer means: denied (OpenShell's 403), noconn (no connection opened),
# answered (something on the other side spoke: a status, an HTTP parse error, a TLS certificate
# error) or none (the probe itself didn't run).
blocked_kind() {
  case "$1" in
    "STATUS 403 "*policy_denied* | "STATUS 403 "*ssrf_denied*) echo denied ;;
    STATUS*) echo answered ;;
    "ERROR HPE_"* | "ERROR "*CERT* | "ERROR ERR_TLS"* | "ERROR "*SELF_SIGNED* | "ERROR UNABLE_TO_"*) echo answered ;;
    ERROR*) echo noconn ;;
    *) echo none ;;
  esac
}
# A batch of probes: each is sent at once, then all are judged against the log read once after
# the batch. Only the status code or the error code is ever printed, never a body.
P_ID=() P_WHAT=() P_KIND=() P_HOST=() P_PORT=() P_SHORT=() BATCH_T0=0
batch_begin() { BATCH_T0=$(date -u +%s); P_ID=() P_WHAT=() P_KIND=() P_HOST=() P_PORT=() P_SHORT=(); }
# A GET, or with PROBE_BODY set a POST of that body (RT-13b sends the request A's policy allows,
# so that only allowed_ips can refuse it).
probe() { # id what url host port [hide]: hide=1 never prints the url, host or answer
  local got short method=GET
  [ -z "${PROBE_BODY:-}" ] || method=POST
  got=$(http_from "$SA" "$method" "$3" "" "${PROBE_BODY:-}")
  [ "$DRY" = 0 ] || return 0
  short=$(awk 'NR == 1 { print $1, $2 }' <<<"$got")
  [ "${6:-0}" = 0 ] || short=""
  P_ID+=("$1") P_WHAT+=("$2") P_KIND+=("$(blocked_kind "$got")") P_HOST+=("$4") P_PORT+=("$5") P_SHORT+=("$short")
}
# A DENIED line for this destination in the log (never printed for a hidden probe).
# Either "<host>:<port>" (or "[<host>]:<port>"), or, for the scheme's default port, a URL with
# no port ("http://<host>/" for 80, "https://<host>/" for 443). VERIFY the log's forms.
logged_denied() { # logs host port
  local lines l h=$2 p=$3
  lines=$(grep -F 'DENIED' <<<"$1" | grep -F -- "$h") || return 1
  grep -Fq -e "$h:$p " -e "$h]:$p " -e "$h:$p/" -e "$h]:$p/" -e "$h:$p]" <<<"$lines" && return 0
  while IFS= read -r l; do [[ "$l" == *"$h:$p" || "$l" == *"$h]:$p" ]] && return 0; done <<<"$lines"
  case "$p" in
    80) grep -Fq -e "http://$h/" -e "http://[$h]/" <<<"$lines" ;;
    443) grep -Fq -e "https://$h/" -e "https://[$h]/" <<<"$lines" ;;
    *) return 1 ;;
  esac
}
batch_judge() {
  local logs i
  [ "$DRY" = 0 ] || return 0
  sleep 5
  logs=$(logs_since "$BATCH_T0")
  for i in "${!P_ID[@]}"; do
    case "${P_KIND[$i]}" in
      denied) pass "${P_ID[$i]}" "${P_WHAT[$i]}: refused by OpenShell (403 policy_denied)" ;;
      noconn)
        if logged_denied "$logs" "${P_HOST[$i]}" "${P_PORT[$i]}"; then
          pass "${P_ID[$i]}" "${P_WHAT[$i]}: no connection, and logged DENIED"
        else
          fail "${P_ID[$i]}" "${P_WHAT[$i]}: no connection, but no DENIED line in A's log for it (inconclusive${P_SHORT[$i]:+: ${P_SHORT[$i]}})"
        fi
        ;;
      answered) fail "${P_ID[$i]}" "${P_WHAT[$i]}: something answered through the sandbox${P_SHORT[$i]:+ (${P_SHORT[$i]})}" ;;
      *) fail "${P_ID[$i]}" "${P_WHAT[$i]}: the probe didn't run in A (inconclusive)" ;;
    esac
  done
}

rt_11() {
  section RT-11 "from A, the cloud metadata services on 80 and 443, each blocked and logged"
  local h p
  batch_begin
  for h in 169.254.169.254 metadata.google.internal fd00:ec2::254 168.63.129.16 100.100.100.200; do
    for p in 80 443; do
      # Plain HTTP on both ports: anything that answers shows as a status, not a TLS error.
      if [[ "$h" == *:* ]]; then
        probe RT-11 "[$h]:$p" "http://[$h]:$p/" "$h" "$p"
      else
        probe RT-11 "$h:$p" "http://$h:$p/" "$h" "$p"
      fi
    done
  done
  batch_judge
}

# The host's own addresses (IPv4 and IPv6, scope global: its interfaces, pmos0's and docker0's).
host_addresses() {
  if command -v ip >/dev/null; then
    ip -o addr show scope global 2>/dev/null | awk '{ sub(/\/.*/, "", $4); print $4 }' | sort -u
  else
    hostname -I 2>/dev/null | tr ' ' '\n' | sed '/^$/d' | sort -u
  fi
}

rt_12() {
  section RT-12 "from A, the host's own addresses on 22, 17670, 2375 and 2376; no Docker socket in A"
  local addrs a p sock
  mapfile -t addrs < <(host_addresses)
  if [ ${#addrs[@]} -eq 0 ]; then
    fail RT-12 "couldn't list the host's addresses (ip -o addr show scope global)"
  fi
  batch_begin
  for a in "${addrs[@]}"; do
    for p in 22 17670 2375 2376; do
      if [[ "$a" == *:* ]]; then
        probe RT-12 "the host's [$a]:$p" "http://[$a]:$p/" "$a" "$p"
      else
        probe RT-12 "the host's $a:$p" "http://$a:$p/" "$a" "$p"
      fi
    done
  done
  batch_judge
  for sock in /var/run/docker.sock /run/docker.sock; do
    # shellcheck disable=SC2016 # run by sh in the sandbox
    case "$(sx "$SA" sh -c 'if [ -e "$1" ] || [ -L "$1" ]; then echo present; else echo absent; fi' probe "$sock" 2>/dev/null | tail -1)" in
      absent) pass RT-12 "the Docker socket $sock is not in A's filesystem" ;;
      present) fail RT-12 "the Docker socket $sock is in A's filesystem" ;;
      *) [ "$DRY" = 1 ] || fail RT-12 "couldn't look for $sock in A (inconclusive)" ;;
    esac
  done
}

# A container's address on the pmos network (read only: docker network inspect).
pmos_address() { # service
  docker network inspect "$NETWORK_NAME" \
    --format '{{range .Containers}}{{.Name}} {{.IPv4Address}}{{"\n"}}{{end}}' 2>/dev/null |
    awk -v s="pmos-$1-" 'index($1, s) == 1 { sub(/\/.*/, "", $2); print $2; exit }'
}

rt_13() {
  section RT-13 "from A, the console, Postgres, the network gateway on 8770 and a pinned address on another port"
  local console pg
  if [ "$DRY" = 1 ]; then
    console="<console address>" pg="<postgres address>"
  else
    console=$(pmos_address console)
    pg=$(pmos_address postgres)
  fi
  batch_begin
  if [ -n "$console" ]; then
    probe RT-13 "the console at $console:8760" "http://$console:8760/healthz" "$console" 8760
  fi
  if [ -n "$pg" ]; then
    probe RT-13 "Postgres at $pg:5432" "http://$pg:5432/" "$pg" 5432
  fi
  probe RT-13 "the network gateway $NETWORK_GATEWAY:8770" "http://$NETWORK_GATEWAY:8770/healthz" "$NETWORK_GATEWAY" 8770
  probe RT-13 "the tool server's pinned address on the gateway's port ($TOOLSERVER_ADDR:8740)" "http://$TOOLSERVER_ADDR:8740/v1/chat/completions" "$TOOLSERVER_ADDR" 8740
  batch_judge
  [ -n "$console" ] || skip RT-13 "the console isn't running on $NETWORK_NAME, so its address wasn't probed"
  [ -n "$pg" ] || skip RT-13 "Postgres isn't running on $NETWORK_NAME, so its address wasn't probed"
}

# RT-13b: rewrite the tool server's line inside the managed block to another address, by a
# rename, as root. SWAPPED is set before the write, so the trap puts the block back however the
# script ends.
hosts_swap() { # address
  local new=$SCRATCH/hosts
  awk -v a="$1" -v n="$TOOLSERVER_NAME" '
    /^#[[:space:]]*BEGIN[[:space:]]+pmos([[:space:]]|$)/ { inb = 1 }
    /^#[[:space:]]*END[[:space:]]+pmos([[:space:]]|$)/ { inb = 0 }
    { if (inb && $2 == n && NF == 2) { print a, n; done = 1 } else print }
    END { exit done ? 0 : 1 }' "$HOSTS_FILE" >"$new" || return 1
  SWAPPED=1
  # shellcheck disable=SC2016 # run by sh as root, with the paths as its arguments
  sudo sh -c 'set -e; t=$(mktemp "$(dirname "$1")/.hosts.pmos-redteam.XXXXXX"); cat "$2" >"$t"; chmod --reference="$1" "$t"; chown --reference="$1" "$t" 2>/dev/null || true; mv -f "$t" "$1"' sh "$HOSTS_FILE" "$new"
}
hosts_restore() {
  [ "$SWAPPED" = 1 ] || return 0
  if sudo "$PMOS_ADMIN" host-names apply >/dev/null; then
    SWAPPED=0
  fi
}

rt_13b() {
  section RT-13b "toolserver.pmos.internal pointed for a moment at .11, then at 10.201.78.10: both refused by allowed_ips"
  local init got target ans
  if [ "$DRY" = 0 ] && ! "$PMOS_ADMIN" host-names check >/dev/null 2>&1; then
    fail RT-13b "deploy/pmos-admin host-names check fails before the test: fix the names first (P-7), nothing was changed"
    return
  fi
  [ "$DRY" = 0 ] || dry RT-13b "would check first: $PMOS_ADMIN host-names check (must pass)"
  may_change RT-13b "rewrites $TOOLSERVER_NAME's line in $HOSTS_FILE's managed block, with sudo, and puts it back with sudo deploy/pmos-admin host-names apply under a trap" || return
  if [ "$DRY" = 0 ] && ! command -v sudo >/dev/null; then
    fail RT-13b "sudo isn't installed: the hosts file can't be changed (nothing was changed)"
    return
  fi
  init=$(mcp_call 1 initialize '{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"redteam","version":"0"}}')
  # Control: before the change, A reaches the tool server (any answer from it, not OpenShell's).
  got=$(http_from "$SA" POST "$TOOLSERVER/mcp" "" "$init")
  if [ "$DRY" = 0 ]; then
    if [ "$(blocked_kind "$got")" = answered ]; then
      pass RT-13b "control: before the change, A reaches the tool server ($(awk 'NR == 1 { print $1, $2 }' <<<"$got"))"
    else
      fail RT-13b "control: before the change, A doesn't reach the tool server ($(awk 'NR == 1 { print $1, $2 }' <<<"$got")); the checks below are inconclusive"
    fi
  fi
  for target in "$GATEWAY_ADDR" "$OUTSIDE_ADDR"; do
    if [ "$DRY" = 1 ]; then
      dry RT-13b "would write '$target $TOOLSERVER_NAME' in the managed block of $HOSTS_FILE (sudo, by a rename)"
      dry RT-13b "would check: $PMOS_ADMIN host-names check fails while it is there"
    else
      if ! hosts_swap "$target"; then
        fail RT-13b "couldn't write the temporary line for $target (the managed block has no plain '$TOOLSERVER_ADDR $TOOLSERVER_NAME' line?)"
        hosts_restore
        return
      fi
      sleep "$RT13B_SETTLE"
      ans=$(getent ahostsv4 "$TOOLSERVER_NAME" 2>/dev/null | awk '{ print $1 }' | sort -u | tr '\n' ' ')
      [ "${ans% }" = "$target" ] || note "the host resolves $TOOLSERVER_NAME to '${ans% }', not $target (a resolver cache?)"
      if "$PMOS_ADMIN" host-names check >/dev/null 2>&1; then
        fail RT-13b "host-names check passes while $TOOLSERVER_NAME points at $target: pmos-admin sandbox wouldn't refuse"
      else
        pass RT-13b "host-names check fails while $TOOLSERVER_NAME points at $target, so pmos-admin sandbox refuses"
      fi
    fi
    batch_begin
    PROBE_BODY=$init probe RT-13b "$TOOLSERVER_NAME:8710 (POST /mcp initialize) while it resolves to $target" "$TOOLSERVER/mcp" "$TOOLSERVER_NAME" 8710
    batch_judge
  done
  if [ "$DRY" = 1 ]; then
    dry RT-13b "would put the block back: sudo $PMOS_ADMIN host-names apply, then check that host-names check passes"
    return
  fi
  hosts_restore
  if [ "$SWAPPED" = 0 ] && "$PMOS_ADMIN" host-names check >/dev/null 2>&1; then
    pass RT-13b "after the test, host-names check passes again"
  else
    fail RT-13b "after the test, host-names check still fails: run sudo deploy/pmos-admin host-names apply"
  fi
}

# --- RT-18: no sandbox reaches the hosted provider, and none holds its key --------------------

# Prints found, not found or error: whether the pattern on standard input is in the place named.
# shellcheck disable=SC2016 # run by sh in the sandbox
KEY_SEARCH='if [ "$1" = environ ]; then grep -qaF -f - /proc/self/environ; else grep -rqsaF -f - "$1"; fi
r=$?
case $r in 0) echo found ;; 1) echo "not found" ;; *) echo error ;; esac'

rt_18() {
  section RT-18 "from A, the hosted provider's host on 443 by name and by address: blocked; no provider key in A"
  local source url key="" host addr where res
  if [ "$DRY" = 1 ]; then
    dry RT-18 "would read the provider's host from deploy/.env (PMOS_HOSTED_TEST_URL; PMOS_GATEWAY_HOSTED_URL under hosted, slice H12), never shown"
    HTTP_SHOWN="GET https://<the provider's host>:443/" probe RT-18 "the provider by name" "" "" 443 1
    HTTP_SHOWN="GET https://<its resolved address>:443/" probe RT-18 "the provider by address" "" "" 443 1
    dry RT-18 "would pass the key's first 8 characters to A on standard input: grep /proc/self/environ and $WORKSPACE, printing only found or not found"
    h12_built || dry RT-18 "under PMOS_MODEL_SOURCE=hosted (slice H12, not built) it would be skipped as not built; hosted-test's provider is probed now"
    return
  fi
  source=$(env_value PMOS_MODEL_SOURCE || true)
  case "$source" in
    hosted-test)
      url=$(env_value PMOS_HOSTED_TEST_URL || true)
      key=$(env_value PMOS_GATEWAY_REMOTE_KEY || true)
      ;;
    hosted)
      if ! h12_built; then
        skip RT-18 "not built: skipped (PMOS_MODEL_SOURCE=hosted needs slice H12)"
        return
      fi
      url=$(env_value PMOS_GATEWAY_HOSTED_URL || true)
      if [ -r "$REPO/deploy/secrets/hosted-model.key" ]; then
        key=$(head -c 8 -- "$REPO/deploy/secrets/hosted-model.key")
      else
        fail RT-18 "can't read deploy/secrets/hosted-model.key as this account: run as its owner (inconclusive)"
      fi
      ;;
    *)
      skip RT-18 "no hosted provider in this deployment (PMOS_MODEL_SOURCE is '${source:-blank}'): nothing to probe"
      return
      ;;
  esac
  host=${url#https://}
  host=${host%%/*}
  host=${host%%:*}
  if [ -z "$host" ] || [ "$host" = "$url" ]; then
    fail RT-18 "the provider's URL in deploy/.env isn't https://<host>/…: nothing was probed (inconclusive)"
  else
    addr=$(getent ahostsv4 "$host" 2>/dev/null | awk 'NR == 1 { print $1 }')
    batch_begin
    probe RT-18 "the provider's host by name on 443 (not shown)" "https://$host:443/" "$host" 443 1
    if [ -n "$addr" ]; then
      probe RT-18 "the provider's resolved address on 443 (not shown)" "https://$addr:443/" "$addr" 443 1
    else
      fail RT-18 "the provider's host doesn't resolve on this host, so its address wasn't probed (inconclusive)"
    fi
    batch_judge
  fi
  if [ ${#key} -lt 8 ]; then
    fail RT-18 "the provider's key is missing or shorter than 8 characters: the key search couldn't run (inconclusive)"
  else
    for where in environ "$WORKSPACE"; do
      res=$(printf '%s\n' "${key:0:8}" | sx "$SA" sh -c "$KEY_SEARCH" search "$where" 2>/dev/null | tail -1)
      case "$res" in
        "not found") pass RT-18 "the provider key's first 8 characters in A's ${where/environ/environment}: not found" ;;
        found) fail RT-18 "the provider key's first 8 characters in A's ${where/environ/environment}: found" ;;
        *) fail RT-18 "couldn't search A's ${where/environ/environment} for the provider's key (inconclusive)" ;;
      esac
    done
  fi
  key=""
}

# --- run --------------------------------------------------------------------------------------

wanted RT-1 && rt_1
wanted RT-2 && rt_2
wanted RT-3 && rt_3
wanted RT-4 && rt_4
wanted RT-5 && rt_5
wanted RT-6 && rt_6
wanted RT-7 && rt_7
wanted RT-8 && rt_8
{ wanted RT-9 || wanted RT-10; } && rt_9_10
wanted RT-11 && rt_11
wanted RT-12 && rt_12
wanted RT-13 && rt_13
wanted RT-13b && rt_13b
wanted RT-18 && rt_18
finish
