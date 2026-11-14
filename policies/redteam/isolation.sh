#!/usr/bin/env bash
# PM OS sandbox red-team: isolation between two bots' OpenShell sandboxes (D3, POL-*).
#
# RUN THIS ON THE SPARK. It can't run in CI: it needs a live OpenShell gateway, both bots'
# sandboxes, and the PM OS tool server, gateway and bot runtime behind *.pmos.internal.
#
#   policies/redteam/isolation.sh <bot-a> <bot-b>      e.g. isolation.sh docs pm-ana
#
# It checks, from inside bot A's sandbox, that A:
#   - runs under the committed policy, non-root, with no_new_privs and no capabilities;
#   - can write only its workspace, and can't write its skills or master context;
#   - can't read bot B's files, see B's processes, or hold B's tokens;
#   - can't use B's credential placeholders against the gateway or the bot runtime;
#   - can't reach anything but toolserver.pmos.internal:8710/mcp, gateway.pmos.internal:8740
#     (chat) and bots.pmos.internal:8750 (the worker routes), and each blocked call is logged
#     with its destination and the policy that blocked it.
# It never claims a task: the runtime controls use a heartbeat on a made-up task id.
# The checklist in checklist.md gives each check's id (RT-n), and the items to do by hand.
#
# What it expects from OpenShell (from its docs at github.com/NVIDIA/OpenShell, main at
# 2e5589b; anything marked VERIFY is an assumption to confirm on the Spark):
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
#
# Environment:
#   OPENSHELL       openshell CLI (default: openshell)
#   POLICIES        the policies directory (default: this script's ../)
#   WORKER_BINARY   node in the worker image (default: /usr/local/bin/node; VERIFY)
#   CHAT_MODEL      a gateway model alias for the positive control (default: pmos/default)
#   DENIED_TOOL     an MCP tool bot A's role lacks (default: jira_search)
#   B_REAL_TOKEN    optional: bot B's real gateway token, to search A's sandbox for it
#
# Exit status: 0 all checks passed, 1 a check failed, 2 the script couldn't run.

set -uo pipefail

OPENSHELL=${OPENSHELL:-openshell}
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
POLICIES=${POLICIES:-$(cd "$HERE/.." && pwd)}
WORKER_BINARY=${WORKER_BINARY:-/usr/local/bin/node}
CHAT_MODEL=${CHAT_MODEL:-pmos/default}
DENIED_TOOL=${DENIED_TOOL:-jira_search}
TOOLSERVER=http://toolserver.pmos.internal:8710
GATEWAY=http://gateway.pmos.internal:8740
RUNTIME=http://bots.pmos.internal:8750
WORKSPACE=/sandbox/workspace
# A task id that can't exist, so worker calls with a valid token get the runtime's 404.
FAKE_TASK=tsk_00000000000000000000000000000000

if [ $# -ne 2 ]; then
  echo "usage: $0 <bot-a> <bot-b>" >&2
  exit 2
fi
A=$1
B=$2
SA=pmos-$A
SB=pmos-$B

fails=0
passes=0
pass() { passes=$((passes + 1)); printf 'PASS %-6s %s\n' "$1" "$2"; }
fail() { fails=$((fails + 1)); printf 'FAIL %-6s %s\n' "$1" "$2"; }
note() { printf '     %s\n' "$*"; }

# Run a command in a sandbox. Never put secrets in its arguments: pass them on stdin.
sx() {
  local sandbox=$1
  shift
  "$OPENSHELL" sandbox exec -n "$sandbox" --no-login-shell -- "$@"
}

# An HTTP request from inside a sandbox, made by the worker binary (the only one allowed out).
# Prints "STATUS <code> <body...>" or "ERROR <reason>". The request travels on stdin.
http_from() {
  local sandbox=$1 method=$2 url=$3 token=${4:-} body=${5:-}
  local req
  req=$(jq -cn --arg m "$method" --arg u "$url" --arg t "$token" --arg b "$body" \
    '{method: $m, url: $u, token: $t, body: $b}')
  printf '%s\n' "const q = $req;
const h = {'content-type': 'application/json', 'accept': 'application/json, text/event-stream',
           'mcp-protocol-version': '2025-06-18'};
if (q.token) h.authorization = 'Bearer ' + q.token;
fetch(q.url, {method: q.method, headers: h, body: q.body || undefined})
  .then(async r => { const t = await r.text(); console.log('STATUS ' + r.status + ' ' + t.slice(0, 300).replace(/\s+/g, ' ')); })
  .catch(e => console.log('ERROR ' + ((e.cause && e.cause.code) || e.message)));" |
    sx "$sandbox" "$WORKER_BINARY" -
}

status_of() { awk '/^STATUS/ {print $2; exit} /^ERROR/ {print "error"; exit}'; }

mcp_call() { # id method params-json
  jq -cn --argjson id "$1" --arg m "$2" --argjson p "$3" '{jsonrpc: "2.0", id: $id, method: $m, params: $p}'
}

chat_body() {
  jq -cn --arg model "$CHAT_MODEL" \
    '{model: $model, max_tokens: 1, messages: [{role: "user", content: "ping"}]}'
}

# --- preflight -------------------------------------------------------------------------------

for tool in "$OPENSHELL" jq; do
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
for sb in "$SA" "$SB"; do
  if ! "$OPENSHELL" sandbox get "$sb" >/dev/null 2>&1; then
    echo "sandbox $sb isn't running; create it from policies/bots/ first (see docs/sandbox-policies.md)" >&2
    exit 2
  fi
done

echo "PM OS sandbox red-team: A=$SA B=$SB ($(date -u +%Y-%m-%dT%H:%M:%SZ))"
REVISION=$("$OPENSHELL" policy list "$SA" 2>/dev/null | head -5)
note "A's policy revisions (newest first):"
printf '%s\n' "$REVISION" | sed 's/^/       /'

# --- RT-1: A runs under the committed policy, and nothing else adds to it --------------------

base=$("$OPENSHELL" policy get "$SA" --base --output json 2>/dev/null | jq -c '.policy // empty')
full=$("$OPENSHELL" policy get "$SA" --full --output json 2>/dev/null | jq -c '.policy // empty')
if [ -z "$base" ]; then
  fail RT-1 "couldn't read A's base policy"
else
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
fi

# --- RT-2: agents can't propose policy changes -------------------------------------------------

settings=$("$OPENSHELL" settings get "$SA" 2>&1 || true)
if grep -Eiq 'agent_policy_proposals_enabled[^a-z]*true' <<<"$settings"; then
  fail RT-2 "the policy advisor is on for $SA: agents could propose network rules outside POL-2 review"
else
  pass RT-2 "the policy advisor is off for $SA (VERIFY the settings output format)"
fi

# --- RT-3: non-root, no_new_privs, no capabilities --------------------------------------------

ident=$(sx "$SA" sh -c 'id -u; grep -E "^(NoNewPrivs|CapEff):" /proc/self/status' 2>&1)
uid=$(head -1 <<<"$ident")
if [ "$uid" != 0 ] && grep -Eq '^NoNewPrivs:[[:space:]]+1' <<<"$ident" &&
  grep -Eq '^CapEff:[[:space:]]+0+$' <<<"$ident"; then
  pass RT-3 "A runs as uid $uid with no_new_privs and no effective capabilities"
else
  fail RT-3 "A's identity: $(tr '\n' ' ' <<<"$ident")"
fi
if sx "$SA" sh -c 'command -v sudo >/dev/null && sudo -n true' >/dev/null 2>&1; then
  fail RT-3 "sudo works in A"
else
  pass RT-3 "sudo is absent or refused in A"
fi

# --- RT-4: A writes only its workspace ------------------------------------------------------

# Non-destructive write probe: in a directory, create a uniquely named file and remove it at
# once; an existing file is opened for append and nothing is written. Prints writable, denied
# or absent.
PROBE='p=$1
if [ -d "$p" ]; then
  f="$p/.pmos-redteam-probe-$$"
  if ( : > "$f" ) 2>/dev/null; then rm -f "$f"; echo writable; else echo denied; fi
elif [ -e "$p" ]; then
  if ( : >> "$p" ) 2>/dev/null; then echo writable; else echo denied; fi
else
  echo absent
fi'
probe() { sx "$1" sh -c "$PROBE" probe "$2" 2>/dev/null | tail -1; }

if [ "$(probe "$SA" "$WORKSPACE")" = writable ]; then
  pass RT-4 "A can write its workspace"
else
  fail RT-4 "A can't write its own workspace ($WORKSPACE): the bot can't work"
fi
mapfile -t content < <(sed -n 's/^    to: //p' "$POLICIES/bots/$A.sandbox.yaml")
for path in "${content[@]}" /sandbox /etc/passwd /usr/local /home /root /app; do
  case "$(probe "$SA" "$path")" in
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
if sx "$SA" ls /sandbox >/dev/null 2>&1; then
  fail RT-4 "A can list /sandbox; only the listed paths under it should be reachable"
else
  pass RT-4 "A can't list /sandbox"
fi

# --- RT-5: A can't read B's files ---------------------------------------------------------

canary="pmos-redteam-$(od -An -N8 -tx1 /dev/urandom | tr -d ' \n')"
if printf '%s\n' "$canary" | sx "$SB" sh -c "read c; printf '%s' \"\$c\" > $WORKSPACE/.redteam-canary" >/dev/null 2>&1; then
  found=$(printf '%s\n' "$canary" |
    sx "$SA" sh -c 'read c; grep -rlsIF --exclude-dir=proc -- "$c" / 2>/dev/null | head -5' 2>/dev/null)
  if [ -z "$found" ]; then
    pass RT-5 "B's canary file is nowhere in A's readable filesystem"
  else
    fail RT-5 "A can read B's canary at: $(tr '\n' ' ' <<<"$found")"
  fi
  sx "$SB" rm -f "$WORKSPACE/.redteam-canary" >/dev/null 2>&1
else
  fail RT-5 "couldn't plant a canary in B's workspace (inconclusive)"
fi

# --- RT-6: A can't see B's processes --------------------------------------------------------

marker=$((RANDOM * 32768 + RANDOM + 100000))
sx "$SB" sleep "$marker" >/dev/null 2>&1 &
sleeper=$!
sleep 3
# The marker goes on stdin so no command line in A contains it; one line per process.
seen=$(printf '%s\n' "$marker" | sx "$SA" sh -c 'read m
  for f in /proc/[0-9]*/cmdline; do tr "\000" " " < "$f" 2>/dev/null; echo; done | grep -c "^sleep $m "' 2>/dev/null)
if [ "${seen:-0}" = 0 ]; then
  pass RT-6 "A can't see B's processes"
else
  fail RT-6 "A sees B's process 'sleep $marker'"
fi
kill "$sleeper" >/dev/null 2>&1
wait "$sleeper" 2>/dev/null

# --- RT-7: A holds only placeholders, and only its own ---------------------------------------

a_env=$(sx "$SA" env 2>/dev/null)
b_gateway=$(sx "$SB" printenv PMOS_GATEWAY_TOKEN 2>/dev/null)
b_worker=$(sx "$SB" printenv PMOS_WORKER_TOKEN 2>/dev/null)
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
  if printf '%s\n' "$t" | sx "$SA" sh -c 'read t; grep -rlsqIF --exclude-dir=proc -- "$t" /' >/dev/null 2>&1; then leak=1; fi
done
if [ "$leak" = 0 ]; then
  pass RT-7 "none of B's tokens or placeholders is in A's environment or files"
else
  fail RT-7 "one of B's tokens or placeholders is visible in A"
fi

# --- RT-8: B's placeholders don't work from A -------------------------------------------------

a_gateway=$(sed -n 's/^PMOS_GATEWAY_TOKEN=//p' <<<"$a_env")
a_worker=$(sed -n 's/^PMOS_WORKER_TOKEN=//p' <<<"$a_env")
control=$(http_from "$SA" POST "$GATEWAY/v1/chat/completions" "$a_gateway" "$(chat_body)" | status_of)
if [ "$control" = 200 ]; then
  pass RT-8 "control: A's own gateway credential works"
else
  fail RT-8 "control: A's own gateway call returned ${control:-nothing}; the B checks below are inconclusive"
fi
if [ -n "$b_gateway" ]; then
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
if [ -n "$b_worker" ]; then
  got=$(http_from "$SA" POST "$heartbeat" "$b_worker" '{}' | status_of)
  if [ "$got" != 404 ] && [ "$got" != 200 ]; then
    pass RT-8 "B's worker placeholder is refused from A ($got)"
  else
    fail RT-8 "B's worker placeholder is accepted by the runtime from A ($got)"
  fi
fi
init=$(mcp_call 1 initialize '{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"redteam","version":"0"}}')

# --- RT-9: blocked calls are blocked, and logged with destination and policy ------------------

since=$(date -u +%s)
# Blocked means OpenShell refused it (403 policy_denied) or the connection never opened. A 4xx
# from the service itself means the request got through the sandbox, which is a failure.
expect_denied() { # id url method token body description
  local id=$1 url=$2 method=$3 token=$4 body=$5 what=$6 got
  got=$(http_from "$SA" "$method" "$url" "$token" "$body")
  if grep -q '^ERROR' <<<"$got" || grep -Eq '^STATUS 403 .*(policy_denied|ssrf_denied)' <<<"$got"; then
    pass "$id" "$what is blocked ($(cut -c1-120 <<<"$got"))"
  else
    fail "$id" "$what got through the sandbox: $(cut -c1-200 <<<"$got")"
  fi
}
expect_denied RT-9 https://slack.com/api/api.test GET "" "" "direct Slack from A"
expect_denied RT-9 https://api.atlassian.com/me GET "" "" "direct Atlassian (Jira, Confluence) from A"
expect_denied RT-9 https://support.fortanix.com/ GET "" "" "the support docs site from A"
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
call=$(mcp_call 2 tools/call "$(jq -cn --arg t "$DENIED_TOOL" '{name: $t, arguments: {}}')")
expect_denied RT-9 "$TOOLSERVER/mcp" POST "" "$call" "tools/call $DENIED_TOOL, outside A's role"
if sx "$SA" sh -c 'command -v curl' >/dev/null 2>&1; then
  if sx "$SA" curl -s -o /dev/null -m 10 "$GATEWAY/v1/chat/completions" >/dev/null 2>&1; then
    fail RT-9 "curl (not the worker binary) reached the gateway"
  else
    pass RT-9 "curl is not allowed to reach the gateway"
  fi
fi

sleep 5
logs=$("$OPENSHELL" logs "$SA" --since "$(($(date -u +%s) - since + 30))s" --source sandbox 2>/dev/null)
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

echo
echo "$passes passed, $fails failed"
[ "$fails" = 0 ]
