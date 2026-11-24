#!/usr/bin/env bash
# PM OS red team: what PM OS's host shows to the outside (RT-14) and to its private network
# (RT-15). Slice H7, docs/openshell-any-host-spec.md §5, "Red-team checklist additions".
#
# Run it from ANOTHER machine, never on PM OS's host: RT-14 from outside the host's network (for
# example a laptop on another connection), RT-15 from a second machine on the same private
# network or VPC. It only connects and sends a few GET and POST requests with empty bodies; it
# changes nothing anywhere, and needs only bash, timeout and curl.
#
#   policies/redteam/exposure.sh [--dry-run] [--web on|off] [--ssh operators|outside]
#                                [--admin-published] [--name <public name>] <host's public address>
#   policies/redteam/exposure.sh [--dry-run] --private <host's private address>      (RT-15)
#
#   --web on|off        whether PM OS's web app is on behind the reverse proxy (default off).
#                       Off: nothing answers on 443 or 80. On: 443 answers (80 if you use it is
#                       noted, not judged), and from slice H10 the routes below answer 404 or 403.
#   --ssh operators|outside
#                       whether this machine is in the operators' range, from which 22 may
#                       answer (default outside: 22 must not answer)
#   --admin-published   you published 8772 on the public address ON PURPOSE, on a test host
#                       (RT-14's second part): pmos-web must answer 404 to every request from here.
#                       Until slice H10 nothing of PM OS listens on 8772, so this part is skipped.
#   --name <name>       the web app's public name, for the requests through 443 (default: the
#                       address); the web app checks the Host it is asked for
#   --private <addr>    RT-15 instead: from a machine on the same private network, the pinned
#                       service addresses (10.201.77.10:8710, .11:8740, .12:8750) don't answer.
#                       Add a route towards the host on this machine first if you can (for
#                       example `sudo ip route add 10.201.77.0/24 via <addr>`); this script adds
#                       none. Without a route "no answer" proves little, and the script says so.
#   --dry-run           print every probe; connect to nothing
#
# RT-14's ports that must not answer from outside: 5432 (Postgres), 8710, 8730, 8740, 8750, 8760,
# 8770, 8771 (the web app's internal route, slice H9), 8772 (the admin listener, slice H10) and
# 17670 (OpenShell's gateway). Through 443, once slice H10 is built and the web app is on:
# /healthz, /readyz, /internal/brief/x and /api/admin/users answer 404 (D3 (b)), and
# POST /api/auth/password/sign-in from an address outside PMOS_WEB_PASSWORD_CLIENTS answers
# 403 password_sign_in_not_from_here (D2 (a)). Before H10 those are skipped, not passed.
# Whether H10 is built is read from this checkout (deploy/compose.yaml): use the commit the host
# runs. The H-ADMIN half of the --admin-published case runs on the host: checklist.md, RT-14.
#
# Exit status (lib.sh): 0 all checks passed, 1 a check failed, 2 the script couldn't run,
# 3 nothing failed but a check was skipped (not a pass). A dry run exits 0.

set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=policies/redteam/lib.sh
. "$HERE/lib.sh"

CLOSED_PORTS=(5432 8710 8730 8740 8750 8760 8770 8771 8772 17670)
TIMEOUT=${EXPOSURE_TIMEOUT:-5}
WEB=off SSH_FROM=outside ADMIN_PUBLISHED=0 NAME="" PRIVATE=0

usage() {
  echo "usage: $0 [--dry-run] [--web on|off] [--ssh operators|outside] [--admin-published] [--name <name>] <public address>" >&2
  echo "       $0 [--dry-run] --private <private address>" >&2
  exit 2
}
args=()
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY=1; shift ;;
    --web) [ $# -ge 2 ] || usage; WEB=$2; shift 2 ;;
    --ssh) [ $# -ge 2 ] || usage; SSH_FROM=$2; shift 2 ;;
    --admin-published) ADMIN_PUBLISHED=1; shift ;;
    --name) [ $# -ge 2 ] || usage; NAME=$2; shift 2 ;;
    --private) PRIVATE=1; shift ;;
    -h | --help) usage ;;
    -*) usage ;;
    *) args+=("$1"); shift ;;
  esac
done
[ ${#args[@]} -eq 1 ] || usage
TARGET=${args[0]}
case "$WEB" in on | off) ;; *) usage ;; esac
case "$SSH_FROM" in operators | outside) ;; *) usage ;; esac
# A host name, an IPv4 address or a bare IPv6 address: plain characters only, since it goes into
# URLs and /dev/tcp paths.
plain_host() { [[ "$1" =~ ^[A-Za-z0-9.:-]{1,253}$ ]] && [[ "$1" != -* ]]; }
plain_host "$TARGET" || { echo "$(printf '%q' "$TARGET") isn't a host name or an address" >&2; exit 2; }
[ -z "$NAME" ] || plain_host "$NAME" || { echo "--name: $(printf '%q' "$NAME") isn't a host name" >&2; exit 2; }
if [ "$PRIVATE" = 1 ] && { [ "$WEB" != off ] || [ "$SSH_FROM" != outside ] || [ "$ADMIN_PUBLISHED" = 1 ] || [ -n "$NAME" ]; }; then
  echo "--private takes no other option but --dry-run" >&2
  exit 2
fi
need=(timeout)
[ "$PRIVATE" = 1 ] || need+=(curl)
if [ "$DRY" = 0 ]; then
  for t in "${need[@]}"; do command -v "$t" >/dev/null || { echo "missing $t" >&2; exit 2; }; done
fi

# Whether a TCP connection to host:port opens within the timeout: open, or closed (refused, no
# route or no answer). Nothing is sent on it.
tcp_state() { # host port
  # shellcheck disable=SC2016 # bash -c's own arguments
  if timeout "$TIMEOUT" bash -c 'exec 3<>"/dev/tcp/$1/$2"' tcp "$1" "$2" 2>/dev/null; then echo open; else echo closed; fi
}
expect_closed() { # id host port what
  if [ "$DRY" = 1 ]; then dry "$1" "TCP connect to $2:$3 ($4): expect no answer"; return; fi
  if [ "$(tcp_state "$2" "$3")" = closed ]; then
    pass "$1" "$2:$3 ($4) doesn't answer"
  else
    fail "$1" "$2:$3 ($4) answers from this machine"
  fi
}
expect_open() { # id host port what
  if [ "$DRY" = 1 ]; then dry "$1" "TCP connect to $2:$3 ($4): expect an answer"; return; fi
  if [ "$(tcp_state "$2" "$3")" = open ]; then
    pass "$1" "$2:$3 ($4) answers"
  else
    fail "$1" "$2:$3 ($4) doesn't answer, but should"
  fi
}
# An HTTPS request through 443 (or to 8772): sets CODE to the status code (000 for none) and
# BODY to the body, which is kept in memory, never printed, and only searched for a marker.
BODY="" CODE=""
http_request() { # method url
  local out data=() tls=()
  [ "$1" != POST ] || data=(-H 'content-type: application/json' --data-binary '{}')
  # By address, the certificate can't match, so it isn't checked (nothing secret is sent); with
  # --name it is.
  [ -n "$NAME" ] || tls=(-k)
  out=$(curl -s "${tls[@]}" -m "$TIMEOUT" -X "$1" "${data[@]}" -w '\n%{http_code}' -- "$2" 2>/dev/null) || true
  BODY=${out%$'\n'*}
  CODE=${out##*$'\n'}
}
expect_http() { # id method url code [marker] what
  local id=$1 m=$2 u=$3 want=$4 marker=$5 what=$6 got
  if [ "$DRY" = 1 ]; then dry "$id" "$m $u: expect $want${marker:+ $marker}"; return; fi
  http_request "$m" "$u"
  got=$CODE
  if [ "$got" = "$want" ] && { [ -z "$marker" ] || grep -qF -- "$marker" <<<"$BODY"; }; then
    pass "$id" "$what: $got${marker:+ $marker}"
  else
    fail "$id" "$what: answered $got, not $want${marker:+ $marker}"
  fi
  BODY=""
}

echo "PM OS exposure probe of $TARGET from $(hostname 2>/dev/null || echo this machine) ($(date -u +%Y-%m-%dT%H:%M:%SZ))"
[ "$DRY" = 0 ] || echo "DRY RUN: the probes below are printed, not run"
commit_line

# --- RT-15: the private network -----------------------------------------------------------------

if [ "$PRIVATE" = 1 ]; then
  if [ "$DRY" = 0 ] && command -v ip >/dev/null; then
    via=$(ip route get 10.201.77.10 2>/dev/null | head -1)
    if [[ "$via" != *" via $TARGET "* ]]; then
      note "no route towards $TARGET for 10.201.77.0/24 on this machine (ip route get: ${via:-none}):"
      note "a refused or missing answer here may only mean the packets never went to the host."
    fi
  fi
  expect_closed RT-15 10.201.77.10 8710 "the tool server's pinned address"
  expect_closed RT-15 10.201.77.11 8740 "the gateway's pinned address"
  expect_closed RT-15 10.201.77.12 8750 "the runtime's pinned address"
  expect_closed RT-15 "$TARGET" 8710 "the tool server's port on the host's private address"
  finish
fi

# --- RT-14: from outside the host's network ----------------------------------------------------

for p in "${CLOSED_PORTS[@]}"; do
  if [ "$p" = 8772 ] && [ "$ADMIN_PUBLISHED" = 1 ]; then continue; fi
  expect_closed RT-14 "$TARGET" "$p" "a PM OS port"
done
if [ "$SSH_FROM" = operators ]; then
  expect_open RT-14 "$TARGET" 22 "SSH, from the operators' range"
else
  expect_closed RT-14 "$TARGET" 22 "SSH, from outside the operators' range"
fi
if [ "$WEB" = off ]; then
  expect_closed RT-14 "$TARGET" 443 "HTTPS, with the web app off"
  expect_closed RT-14 "$TARGET" 80 "HTTP, with the web app off"
else
  expect_open RT-14 "$TARGET" 443 "HTTPS, the reverse proxy"
  if [ "$DRY" = 1 ]; then
    dry RT-14 "TCP connect to $TARGET:80: noted, not judged (80 only if the proxy uses it)"
  else
    note "80 $( [ "$(tcp_state "$TARGET" 80)" = open ] && echo answers || echo "doesn't answer") (allowed only if the proxy uses it for its redirect)"
  fi
  base="https://${NAME:-$TARGET}"
  if h10_built; then
    for path in /healthz /readyz /internal/brief/x /api/admin/users; do
      expect_http RT-14 GET "$base$path" 404 "" "GET $path through 443 (D3 (b))"
    done
    expect_http RT-14 POST "$base/api/auth/password/sign-in" 403 password_sign_in_not_from_here \
      "POST /api/auth/password/sign-in from outside PMOS_WEB_PASSWORD_CLIENTS (D2 (a))"
  else
    skip RT-14 "not built: skipped: through 443, /healthz, /readyz, /internal/brief/x and /api/admin/users answering 404 and the password sign-in answering 403 need slice H10"
    if [ "$DRY" = 1 ]; then
      for path in /healthz /readyz /internal/brief/x /api/admin/users; do dry RT-14 "after H10: GET $base$path: expect 404"; done
      dry RT-14 "after H10: POST $base/api/auth/password/sign-in: expect 403 password_sign_in_not_from_here"
    fi
  fi
fi
if [ "$ADMIN_PUBLISHED" = 1 ]; then
  if h10_built; then
    for path in / /api/admin/users /api/auth/session /healthz; do
      expect_http RT-14 GET "https://${NAME:-$TARGET}:8772$path" 404 "" "GET $path on the admin listener published on purpose (the in-service peer check)"
    done
    byhand RT-14 "on the host: deploy/preflight.sh fails H-ADMIN while PMOS_WEB_ADMIN_PUBLISH is a public address"
  else
    skip RT-14 "not built: skipped: 8772 published on purpose needs slice H10 (the admin listener); until then nothing of PM OS listens on 8772"
    [ "$DRY" = 0 ] || dry RT-14 "after H10: GET https://${NAME:-$TARGET}:8772/, /api/admin/users, /api/auth/session, /healthz: expect 404 each"
    expect_closed RT-14 "$TARGET" 8772 "the admin listener's port, before H10"
  fi
fi
finish
