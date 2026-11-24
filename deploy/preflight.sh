#!/usr/bin/env bash
# shellcheck disable=SC2329 # the checks are functions called through run_check
# PM OS host preflight (docs/openshell-any-host-spec.md §3.2, slice H1): does this host meet what
# PM OS and OpenShell need? One line per requirement: pass, warn, fail (or skip, when it doesn't
# apply), with what was measured. It changes nothing on the host.
#
#   deploy/preflight.sh [--model-source local|hosted|hosted-test] [--exposure private|public]
#                       [--stage install|run|quick] [--openshell] [--json] [--root <dir>]
#                       [--env-file <deploy/.env>] [--web] [--starting] [--only <ID,ID…>]
#
#   --stage install   setup.sh, before anything is installed: every host check; H-NAMES and
#                     H-METADATA report "not set yet" and H-EGRESS "not started", which pass
#   --stage run       (the default) pmos-admin up, restore, add-bot (with --starting: the stack
#                     is about to start, so H-EGRESS reports "not started"), sandbox and resume
#                     (with --openshell), status and by hand
#   --stage quick     the worker unit's ExecStartPre, at every start and boot: H-LSM, H-LANDLOCK,
#                     H-SECCOMP, H-DOCKER-SECCOMP, H-NAMES and O-GATEWAY. It doesn't read deploy/.env
#   --openshell       also the O-* checks: the CLI, the gateway, lingering, and two canary
#                     sandboxes (pmos-preflight and pmos-preflight-net), made and deleted
#   --web             the web app is (about to be) on (pmos-admin up --web); without it, a
#                     running `web` container of the pmos project counts
#   --root <dir>      read /proc, /sys, /etc, /run, /var and every command's output from a
#                     fixture directory instead of this host (scripts/test-preflight.sh): a file
#                     <dir>/<path> for a host file, <dir>/cmd/<id> (and <dir>/cmd/<id>.exit) for a
#                     command's output, <dir>/bin/openshell for OpenShell's CLI
#   --only <ids>      run only these checks (diagnosis and tests); pmos-admin never passes it
#
# The model source and the exposure come from the flags, else deploy/.env. Since slice H4 a blank
# one is refused by deploy/pmos-admin, by deploy/compose.yaml (`:?`) and by every service, so at
# the run stage a blank PMOS_MODEL_SOURCE or PMOS_EXPOSURE is a fail (H-SETTINGS), and `hosted`
# (an approved provider, slice H12, not built) at every stage. At the install stage a blank one is
# what setup.sh is about to fill in (deploy/env.example ships them blank; setup.sh writes
# hosted-test and public unless told otherwise, §3.1, and passes its choice here), so it passes,
# saying so. Either way the
# checks that depend on a blank setting read it the stricter way: the source as `local` (H-NVIDIA,
# H-LOCAL: the most checked) and the exposure as `public` (the value that allows less, §3.1).
#
# Exit status: 0 every check passes (warnings allowed), 1 any fails, 2 the script couldn't run.
# --json prints {"checks": [{"id", "status", "measured", "expected", "hint"}, …],
# "acknowledgements": […], "attestations": […]}.
#
# Nothing here has run on a host with OpenShell yet. What each O-* check runs is from OpenShell's
# docs (sandbox create --from, sandbox exec, logs --source sandbox, sandbox delete); the acceptance
# run (§6, steps 1 and 2) proves them, and the probe shapes of H-METADATA on each cloud (RT-16).
#
# Internal, for deploy/pmos-admin's `host acknowledge` and `host attest` (one implementation of
# each fingerprint): --fingerprint h-private|local-gpu prints "<sha256>\t<what was seen>".
set -uo pipefail

DEPLOY="$(cd "$(dirname "$0")" && pwd)"
# The fixture directory every host read comes from, when not given with --root. Empty on a real
# host; scripts/test-pmos-admin.sh's copy of this script (and only that copy) names its fixture
# here, as it does for pmos-admin's hosts file. Nothing in the environment or deploy/.env sets it.
readonly HOST_ROOT_DEFAULT=""

usage() {
  awk 'NR > 2 && /^#/ { sub(/^# ?/, ""); print; next } NR > 2 { exit }' "$0" >&2
  exit 2
}
die() { printf 'preflight: %s\n' "$*" >&2; exit 2; }

ROOT=$HOST_ROOT_DEFAULT
ENV_FILE="$DEPLOY/.env"
MS_FLAG="" EXP_FLAG="" STAGE=run OPENSHELL=0 JSON=0 WEB_FLAG=0 STARTING=0 ONLY="" FINGERPRINT=""
while [ $# -gt 0 ]; do
  case "$1" in
    --model-source) [ $# -ge 2 ] || usage; MS_FLAG=$2; shift 2 ;;
    --exposure) [ $# -ge 2 ] || usage; EXP_FLAG=$2; shift 2 ;;
    --stage) [ $# -ge 2 ] || usage; STAGE=$2; shift 2 ;;
    --openshell) OPENSHELL=1; shift ;;
    --json) JSON=1; shift ;;
    --root) [ $# -ge 2 ] || usage; ROOT=$2; shift 2 ;;
    --env-file) [ $# -ge 2 ] || usage; ENV_FILE=$2; shift 2 ;;
    --web) WEB_FLAG=1; shift ;;
    --starting) STARTING=1; shift ;;
    --only) [ $# -ge 2 ] || usage; ONLY=",$2,"; shift 2 ;;
    --fingerprint) [ $# -ge 2 ] || usage; FINGERPRINT=$2; shift 2 ;;
    -h | --help) usage ;;
    *) die "unknown argument $(printf '%q' "$1") (see --help)" ;;
  esac
done
case "$STAGE" in install | run | quick) ;; *) die "--stage must be install, run or quick" ;; esac
case "$MS_FLAG" in '' | local | hosted | hosted-test) ;; *) die "--model-source must be local, hosted or hosted-test" ;; esac
case "$EXP_FLAG" in '' | private | public) ;; *) die "--exposure must be private or public" ;; esac
case "$FINGERPRINT" in '' | h-private | local-gpu) ;; *) die "--fingerprint takes h-private or local-gpu" ;; esac
if [ -n "$ROOT" ]; then
  [ -d "$ROOT" ] || die "--root $(printf '%q' "$ROOT") isn't a directory"
  ROOT=$(cd "$ROOT" && pwd)
fi
command -v python3 >/dev/null || die "python3 is needed (it is for pmos-admin ledger too): sudo apt-get install python3"

# --- Reading the host, or the fixture ---------------------------------------------------------
hp() { printf '%s%s\n' "$ROOT" "$1"; } # a host path
# A command's output: on the host, the command (stderr dropped); with --root, cmd/<id> (missing:
# "not installed", exit 127) and its exit status from cmd/<id>.exit (default 0).
cmd_out() { # id command [args…]
  local id=$1
  shift
  if [ -n "$ROOT" ]; then
    [ -f "$ROOT/cmd/$id" ] || return 127
    cat -- "$ROOT/cmd/$id"
    if [ -f "$ROOT/cmd/$id.exit" ]; then return "$(cat -- "$ROOT/cmd/$id.exit")"; fi
    return 0
  fi
  command -v "$1" >/dev/null 2>&1 || return 127
  "$@" 2>/dev/null
}
# OpenShell's CLI: on the host, `openshell` on PATH; with --root, <root>/bin/openshell (a stand-in).
openshell_bin() {
  if [ -n "$ROOT" ]; then
    [ -x "$ROOT/bin/openshell" ] && printf '%s\n' "$ROOT/bin/openshell"
  else
    command -v openshell
  fi
}

# --- deploy/.env, read as data (never sourced; as deploy/pmos-admin's load_env) -----------------
declare -A E=()
read_env() {
  local line key value n=0
  [ -f "$ENV_FILE" ] || return 0
  [ -r "$ENV_FILE" ] || die "$ENV_FILE can't be read by this account"
  while IFS= read -r line || [ -n "$line" ]; do
    n=$((n + 1))
    line=${line%$'\r'}
    [[ "$line" =~ ^[[:space:]]*(#.*)?$ ]] && continue
    [[ "$line" =~ ^[[:space:]]*(export[[:space:]]+)?([A-Za-z_][A-Za-z0-9_]*)=(.*)$ ]] ||
      die "$ENV_FILE line $n isn't KEY=VALUE, a comment or blank"
    key=${BASH_REMATCH[2]}
    value=${BASH_REMATCH[3]}
    if [[ "$value" =~ ^\"([^\"]*)\"([[:space:]]+#.*)?[[:space:]]*$ ||
      "$value" =~ ^\'([^\']*)\'([[:space:]]+#.*)?[[:space:]]*$ ]]; then
      value=${BASH_REMATCH[1]}
    else
      value=${value%%[[:space:]]#*}
      value=${value%"${value##*[![:space:]]}"}
    fi
    E[$key]=$value
  done <"$ENV_FILE"
}
ev() { printf '%s' "${E[$1]:-}"; }
is_set() { [ -n "${E[$1]:-}" ] && [ "${E[$1]}" != generated ]; }
# At install on a new host there is no deploy/.env yet: the settings setup.sh is about to write
# from deploy/env.example are the ones checked.
if [ "$STAGE" = install ] && [ ! -e "$ENV_FILE" ] && [ -f "$DEPLOY/env.example" ]; then ENV_FILE="$DEPLOY/env.example"; fi
[ "$STAGE" = quick ] && [ -z "$FINGERPRINT" ] || read_env

# What the settings say (a flag, else deploy/.env; blank when neither), and the reading the
# checks use (a blank one the stricter way, above).
MS_SET=${MS_FLAG:-${E[PMOS_MODEL_SOURCE]:-}}
EXPOSURE_SET=${EXP_FLAG:-${E[PMOS_EXPOSURE]:-}}
MS=${MS_SET:-local}
EXPOSURE=${EXPOSURE_SET:-public}
case "$MS" in local | hosted | hosted-test) ;; *) die "PMOS_MODEL_SOURCE=$(printf '%q' "$MS"): use local, hosted or hosted-test" ;; esac
case "$EXPOSURE" in private | public) ;; *) die "PMOS_EXPOSURE=$(printf '%q' "$EXPOSURE"): use private or public" ;; esac

# --- Results ------------------------------------------------------------------------------------
R_ID=() R_ST=() R_MEAS=() R_EXP=() R_HINT=()
res() { # id status measured expected [hint]
  R_ID+=("$1") R_ST+=("$2") R_MEAS+=("$3") R_EXP+=("$4") R_HINT+=("${5:-}")
}
want() { [ -z "$ONLY" ] || [[ "$ONLY" == *",$1,"* ]]; }
join_sc() { local out="" x; for x in "$@"; do out+="${out:+; }$x"; done; printf '%s' "$out"; }

# --- Numbers and addresses ----------------------------------------------------------------------
# "a.b[.c]" >= "x.y[.z]", numerically.
version_ge() {
  local IFS=. i a b
  read -r -a a <<<"$1"
  read -r -a b <<<"$2"
  for i in 0 1 2; do
    [ "$((10#${a[$i]:-0}))" -gt "$((10#${b[$i]:-0}))" ] && return 0
    [ "$((10#${a[$i]:-0}))" -lt "$((10#${b[$i]:-0}))" ] && return 1
  done
  return 0
}
# As deploy/pmos-admin's (slice H2): four plain decimal octets, no leading zero.
ipv4_strict() {
  local IFS=. a b c d o
  [[ "$1" =~ ^[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}$ ]] || return 1
  read -r a b c d <<<"$1"
  for o in "$a" "$b" "$c" "$d"; do
    [[ "$o" =~ ^(0|[1-9][0-9]{0,2})$ ]] && [ "$o" -le 255 ] || return 1
  done
  echo $(((a << 24) | (b << 16) | (c << 8) | d))
}
ipv4_cidr() {
  local addr bits n
  [[ "$1" == */* ]] || return 1
  addr=${1%/*}
  bits=${1#*/}
  [[ "$bits" =~ ^(0|[1-9][0-9]?)$ ]] && [ "$bits" -le 32 ] || return 1
  n=$(ipv4_strict "$addr") || return 1
  [ $((n & ((0xffffffff << (32 - bits)) & 0xffffffff))) = "$n" ] || return 1
  echo "$n $bits"
}
ipv4_num_in() { [ $(($1 & ((0xffffffff << (32 - $3)) & 0xffffffff))) = "$2" ]; }
# Is an address (IPv4 or IPv6) private in the sense of H-PRIVATE: RFC 1918, 100.64.0.0/10,
# loopback, link-local, IPv6 unique-local and link-local? python3's ipaddress, without its
# is_private (which also counts documentation and other reserved ranges as private).
addr_class() { # address… → one "<address> private|public|unspecified|bad" line each
  python3 -I -c '
import ipaddress, sys
nets = [ipaddress.ip_network(n) for n in ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16",
        "100.64.0.0/10", "127.0.0.0/8", "169.254.0.0/16", "::1/128", "fc00::/7", "fe80::/10")]
for a in sys.argv[1:]:
    try:
        ip = ipaddress.ip_address(a.split("%")[0])
    except ValueError:
        print(a, "bad"); continue
    if ip.is_unspecified:
        print(a, "unspecified")
    elif any(ip.version == n.version and ip in n for n in nets):
        print(a, "private")
    else:
        print(a, "public")
' "$@"
}
is_loopback() { case "$1" in 127.* | ::1 | localhost) return 0 ;; esac; return 1; }

# --- The settings read like deploy/pmos-admin (published ports, the pinned network) -------------
# The pinned addresses are reviewed code, equal to deploy/compose.yaml's literals and to
# deploy/pmos-admin's (scripts/test-preflight.sh checks all three agree); the three services' are
# also the policy crate's constants, each name's allowed_ips in every bot's policy (slice H3;
# crates/policy/tests/policies.rs checks PINNED_NAMES and PINNED_SERVICE_ADDRS against them).
PINNED_NAMES=(toolserver.pmos.internal gateway.pmos.internal bots.pmos.internal)
PINNED_SERVICE_ADDRS=(10.201.77.10 10.201.77.11 10.201.77.12)
PINNED_ADDRS=(10.201.77.10 10.201.77.11 10.201.77.12 10.201.77.13 10.201.77.14)
NETWORK_BRIDGE=pmos0
NETWORK_NAME=pmos_pmos
NETWORK_SUBNET_DEFAULT=10.201.77.0/24
NETWORK_GATEWAY_DEFAULT=10.201.77.1
NETWORK_IP_RANGE_DEFAULT=10.201.77.128/25
NETWORK_INSPECT_FORMAT='{{range .IPAM.Config}}{{.Subnet}} {{.Gateway}} {{.IPRange}};{{end}}{{index .Options "com.docker.network.bridge.name"}}'
# The published ports (deploy/compose.yaml, always on 127.0.0.1) as deploy/pmos-admin's
# port_checks reads them; the admin door (8772, slice H10: on PMOS_WEB_ADMIN_PUBLISH) and OpenShell's gateway (17670).
# 8780 is the sandbox-audit collector (docs/openshell-prover-and-sandbox-audit-spec.md §B.1): OpenShell
# exports to it on 127.0.0.1, so it is loopback only like the rest.
PORT_KEYS=(TOOLSERVER:8710 GATEWAY:8740 BOTS:8750 CONSOLE:8760 WEB:8770 SANDBOX_AUDIT:8780)
ADMIN_PORT=8772
# The web app's internal listener (PMOS_WEB_INTERNAL_BIND, slice H9): on the `pmos` network only,
# never published, so no listener on the host may hold it.
INTERNAL_PORT=8771
OPENSHELL_PORT=17670
PORTS=() PORT_PROBLEMS=()
read_ports() {
  local k d v
  PORTS=() PORT_PROBLEMS=()
  for k in "${PORT_KEYS[@]}"; do
    d=${k#*:}
    k=PMOS_${k%%:*}_PORT
    v=$(ev "$k")
    if [ -z "$v" ]; then
      v=$d
    elif ! [[ "$v" =~ ^[0-9]{1,5}$ ]] || [ $((10#$v)) -lt 1 ] || [ $((10#$v)) -gt 65535 ]; then
      PORT_PROBLEMS+=("$k=$(printf '%q' "$v") isn't a port number (1 to 65535)")
      v=$d
    fi
    PORTS+=("$((10#$v))")
  done
}
# ss -ltnH: "<address> <port>" per listener, the address without brackets or %interface.
listeners() {
  local out line local_addr addr port
  out=$(cmd_out ss ss -ltnH) || return 1
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    read -r _ _ _ local_addr _ <<<"$line"
    port=${local_addr##*:}
    addr=${local_addr%:*}
    addr=${addr#[}
    addr=${addr%]}
    addr=${addr%%%*}
    [ "$addr" != '*' ] || addr=0.0.0.0
    printf '%s %s\n' "$addr" "$port"
  done <<<"$out"
}

# --- The checks -----------------------------------------------------------------------------------
# The two host settings the stack starts from (§3.1). A blank one is refused by pmos-admin,
# deploy/compose.yaml and every service since slice H4, so at the run stage it fails here too;
# at install it is what setup.sh fills in. The checks that depend on one read a blank the
# stricter way either way (local, public).
c_settings() {
  local probs=() fill=()
  if [ -z "$MS_SET" ]; then
    if [ "$STAGE" = install ]; then
      fill+=("PMOS_MODEL_SOURCE blank: setup.sh writes hosted-test unless --model-source local")
    else
      probs+=("PMOS_MODEL_SOURCE is blank (use local or hosted-test; hosted isn't built yet, slice H12)")
    fi
  elif [ "$MS_SET" = hosted ]; then
    probs+=("PMOS_MODEL_SOURCE=hosted, an approved provider, isn't built yet (slice H12): every service refuses it (use local or hosted-test)")
  fi
  if [ -z "$EXPOSURE_SET" ]; then
    if [ "$STAGE" = install ]; then
      fill+=("PMOS_EXPOSURE blank: setup.sh writes public unless --exposure private")
    else
      probs+=("PMOS_EXPOSURE is blank (use public or private)")
    fi
  fi
  if [ ${#probs[@]} -gt 0 ]; then
    res H-SETTINGS fail "$(join_sc "${probs[@]}")" "PMOS_MODEL_SOURCE local or hosted-test, PMOS_EXPOSURE public or private" "set them in deploy/.env (deploy/setup.sh writes them: --model-source hosted-test|local, --exposure public|private); deploy/pmos-admin and every service refuse a blank one, which is never read as another value. The checks below read a blank source as local and a blank exposure as public"
  elif [ ${#fill[@]} -gt 0 ]; then
    res H-SETTINGS pass "$(join_sc "${fill[@]}"); checked as PMOS_MODEL_SOURCE=$MS, PMOS_EXPOSURE=$EXPOSURE" "PMOS_MODEL_SOURCE local or hosted-test, PMOS_EXPOSURE public or private"
  else
    res H-SETTINGS pass "PMOS_MODEL_SOURCE=$MS_SET, PMOS_EXPOSURE=$EXPOSURE_SET" "PMOS_MODEL_SOURCE local or hosted-test, PMOS_EXPOSURE public or private"
  fi
}
c_arch() {
  local m
  m=$(cmd_out uname-m uname -m) || { res H-ARCH fail "uname -m failed" "x86_64 or aarch64"; return; }
  case "$m" in
    x86_64 | aarch64) res H-ARCH pass "$m" "x86_64 or aarch64" ;;
    *) res H-ARCH fail "$m" "x86_64 or aarch64" "OpenShell supports Linux on x86_64 and aarch64 only" ;;
  esac
}
c_os() {
  local f id="" like="" line
  f=$(hp /etc/os-release)
  if [ ! -r "$f" ]; then
    res H-OS warn "no /etc/os-release" "Debian or Ubuntu" "not in OpenShell's support matrix"
    return
  fi
  while IFS= read -r line; do
    case "$line" in
      ID=*) id=${line#ID=} ;;
      ID_LIKE=*) like=${line#ID_LIKE=} ;;
    esac
  done <"$f"
  id=${id//\"/} like=${like//\"/}
  if [[ " $id $like " == *" debian "* || " $id $like " == *" ubuntu "* ]]; then
    res H-OS pass "ID=$id${like:+ ID_LIKE=$like}" "Debian or Ubuntu"
  else
    res H-OS warn "ID=$id${like:+ ID_LIKE=$like}" "Debian or Ubuntu" "not in OpenShell's support matrix"
  fi
}
c_glibc() {
  local out v
  out=$(cmd_out getconf-glibc getconf GNU_LIBC_VERSION) || { res H-GLIBC fail "getconf GNU_LIBC_VERSION failed (not glibc?)" "glibc 2.28 or newer" "OpenShell's gateway needs glibc 2.28+"; return; }
  v=${out#glibc }
  if [[ "$v" =~ ^[0-9]+\.[0-9]+ ]] && version_ge "$v" 2.28; then
    res H-GLIBC pass "$out" "glibc 2.28 or newer"
  else
    res H-GLIBC fail "$out" "glibc 2.28 or newer" "OpenShell's gateway needs glibc 2.28+"
  fi
}
c_kernel() {
  local r v
  r=$(cmd_out uname-r uname -r) || { res H-KERNEL fail "uname -r failed" "Linux 6.2 or newer"; return; }
  v=$(sed -E 's/^([0-9]+\.[0-9]+).*/\1/' <<<"$r")
  if [[ "$v" =~ ^[0-9]+\.[0-9]+$ ]] && version_ge "$v" 6.2; then
    res H-KERNEL pass "$r" "Linux 6.2 or newer" "a kernel version alone proves nothing: H-LSM and H-LANDLOCK decide"
  else
    res H-KERNEL fail "$r" "Linux 6.2 or newer" "Landlock ABI 3 needs Linux 6.2+"
  fi
}
c_lsm() {
  local f lsm
  f=$(hp /sys/kernel/security/lsm)
  if [ ! -r "$f" ]; then
    res H-LSM fail "/sys/kernel/security/lsm can't be read" "landlock in the active LSM list" "is securityfs mounted?"
    return
  fi
  lsm=$(tr -d '\n' <"$f")
  if [[ ",$lsm," == *",landlock,"* ]]; then
    res H-LSM pass "$lsm" "landlock in the active LSM list"
  else
    res H-LSM fail "$lsm" "landlock in the active LSM list" "add landlock to the kernel's lsm= boot parameter (a kernel built with Landlock but not listed there fails)"
  fi
}
# landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION) returns the ABI: syscall 444
# on x86_64 and aarch64 alike. It prints the number, or "error <reason>" and exits 1; never a crash.
LANDLOCK_PROBE='
import ctypes, os, sys
try:
    libc = ctypes.CDLL(None, use_errno=True)
    libc.syscall.restype = ctypes.c_long
    r = libc.syscall(ctypes.c_long(444), None, ctypes.c_size_t(0), ctypes.c_uint32(1))
except Exception as e:
    print("error " + type(e).__name__)
    sys.exit(1)
if r < 0:
    print("error " + os.strerror(ctypes.get_errno()))
    sys.exit(1)
print(r)
'
c_landlock() {
  local out code=0
  out=$(cmd_out landlock-abi python3 -I -c "$LANDLOCK_PROBE") || code=$?
  out=$(head -1 <<<"$out")
  if [ "$code" = 0 ] && [[ "$out" =~ ^[0-9]+$ ]]; then
    if [ "$out" -ge 3 ]; then
      res H-LANDLOCK pass "ABI $out" "Landlock ABI 3 or newer"
    else
      res H-LANDLOCK fail "ABI $out" "Landlock ABI 3 or newer" "a newer kernel (6.2+) is needed"
    fi
  else
    res H-LANDLOCK fail "not measurable: ${out:-the probe failed}" "Landlock ABI 3 or newer" "the probe calls landlock_create_ruleset; a seccomp filter or a disabled Landlock makes it fail"
  fi
}
c_seccomp() {
  local status cfg="" conf r
  status=$(hp /proc/self/status)
  if ! grep -q '^Seccomp:' "$status" 2>/dev/null; then
    res H-SECCOMP fail "no Seccomp: line in /proc/self/status" "seccomp available" "the kernel is built without seccomp"
    return
  fi
  r=$(cmd_out uname-r uname -r || true)
  if [ -r "$(hp /proc/config.gz)" ]; then
    conf=$(gzip -dc "$(hp /proc/config.gz)" 2>/dev/null)
    cfg=/proc/config.gz
  elif [ -n "$r" ] && [ -r "$(hp "/boot/config-$r")" ]; then
    conf=$(cat "$(hp "/boot/config-$r")")
    cfg=/boot/config-$r
  fi
  if [ -n "$cfg" ] && ! grep -qx 'CONFIG_SECCOMP_FILTER=y' <<<"$conf"; then
    res H-SECCOMP fail "$cfg has no CONFIG_SECCOMP_FILTER=y" "seccomp filters available" "the kernel is built without seccomp filters"
    return
  fi
  res H-SECCOMP pass "Seccomp in /proc/self/status${cfg:+; CONFIG_SECCOMP_FILTER=y in $cfg}" "seccomp available"
}
c_cgroup() {
  if [ -e "$(hp /sys/fs/cgroup/cgroup.controllers)" ]; then
    res H-CGROUP pass "cgroups v2" "cgroups v2"
  else
    res H-CGROUP fail "cgroups v1 only (no /sys/fs/cgroup/cgroup.controllers)" "cgroups v2" "boot with systemd.unified_cgroup_hierarchy=1"
  fi
}
c_docker() {
  local v
  v=$(cmd_out docker-server-version docker version --format '{{.Server.Version}}') || v=""
  v=$(head -1 <<<"$v")
  if ! [[ "$v" =~ ^[0-9]+\.[0-9]+ ]]; then
    res H-DOCKER fail "docker missing, or its daemon not reachable by this user" "Docker Engine 28.0+, Compose v2" "install Docker Engine 28+ and add this user to the docker group (platform owners only: it is root on the host)"
    return
  fi
  if ! version_ge "$v" 28.0; then
    res H-DOCKER fail "Docker $v" "Docker Engine 28.0+, Compose v2" "upgrade Docker Engine to 28.0 or newer"
    return
  fi
  if ! cmd_out docker-compose-version docker compose version >/dev/null; then
    res H-DOCKER fail "Docker $v, no docker compose v2" "Docker Engine 28.0+, Compose v2" "install the docker-compose-plugin"
    return
  fi
  res H-DOCKER pass "Docker $v, Compose v2" "Docker Engine 28.0+, Compose v2"
}
c_docker_seccomp() {
  local o
  o=$(cmd_out docker-security-options docker info --format '{{json .SecurityOptions}}') || {
    res H-DOCKER-SECCOMP fail "docker info failed" "seccomp in Docker's security options" "is the daemon reachable by this user?"
    return
  }
  if [[ "$o" != *seccomp* ]]; then
    res H-DOCKER-SECCOMP fail "$o" "seccomp in Docker's security options" "Docker must run containers under its default seccomp profile"
  elif [[ "$o" == *"seccomp,profile=unconfined"* ]]; then
    res H-DOCKER-SECCOMP fail "$o" "seccomp with Docker's default profile" "remove seccomp-profile=unconfined from the daemon's settings"
  else
    res H-DOCKER-SECCOMP pass "$o" "seccomp in Docker's security options"
  fi
}
c_nvidia() {
  local r img
  if [ "$MS" != local ]; then
    res H-NVIDIA skip "PMOS_MODEL_SOURCE=$MS" "an NVIDIA runtime only for local"
    return
  fi
  # The stub model (PMOS_GATEWAY_STUB=1, the install runbook's first start and CI's stack) answers
  # every call inside the gateway: no model server starts, so there is nothing for a GPU to run.
  # The gateway then serves as class `test`, and the check applies again once the stub is off.
  if [ "$(ev PMOS_GATEWAY_STUB)" = 1 ]; then
    res H-NVIDIA skip "PMOS_GATEWAY_STUB=1" "an NVIDIA runtime once the stub model is off"
    return
  fi
  r=$(cmd_out docker-runtimes docker info --format '{{json .Runtimes}}') || r=""
  img=$(ev PMOS_VLLM_IMAGE)
  if [[ "$r" != *'"nvidia"'* ]]; then
    res H-NVIDIA fail "no nvidia runtime in docker info" "the NVIDIA container runtime, and PMOS_VLLM_IMAGE set" "PMOS_MODEL_SOURCE=local runs its model servers on this host's GPU: install the NVIDIA container toolkit, or use hosted-test with fake data"
  elif [ -z "$img" ]; then
    res H-NVIDIA fail "the nvidia runtime; PMOS_VLLM_IMAGE empty" "the NVIDIA container runtime, and PMOS_VLLM_IMAGE set" "set PMOS_VLLM_IMAGE in deploy/.env to the vLLM image you have reviewed (it has no default)"
  else
    res H-NVIDIA pass "the nvidia runtime; PMOS_VLLM_IMAGE set" "the NVIDIA container runtime, and PMOS_VLLM_IMAGE set"
  fi
}
c_egress() {
  local gw body verdict
  case "$MS" in hosted | hosted-test) ;; *) res H-EGRESS skip "PMOS_MODEL_SOURCE=$MS" "checked for hosted and hosted-test"; return ;; esac
  if [ "$STAGE" = install ] || [ "$STARTING" = 1 ]; then
    res H-EGRESS pass "not started" "the gateway's /readyz shows its hosted upstream ok"
    return
  fi
  read_ports
  gw=${PORTS[1]}
  # The gateway's own probe, labels only; never a request with the key from here.
  body=$(cmd_out gateway-readyz curl -sS --noproxy '*' -m 5 "http://127.0.0.1:$gw/readyz") || body=""
  if [ -z "$body" ]; then
    res H-EGRESS pass "not started (no answer on 127.0.0.1:$gw)" "the gateway's /readyz shows its hosted upstream ok"
    return
  fi
  verdict=$(python3 -I -c '
import json, sys
try:
    d = json.loads(sys.stdin.read())
    ups = d.get("upstreams") or {}
    down = sorted(k for k, v in ups.items() if not isinstance(v, dict) or v.get("status") != "ok")
    print("down " + ",".join(down) if down else ("ok" if ups else "none"))
except Exception:
    print("unreadable")
' <<<"$body")
  case "$verdict" in
    ok) res H-EGRESS pass "every upstream ok" "the gateway's /readyz shows its hosted upstream ok" ;;
    *)
      local st=fail
      [ "$MS" = hosted ] || st=warn
      res H-EGRESS "$st" "gateway /readyz: $verdict" "the gateway's /readyz shows its hosted upstream ok" "the gateway can't reach its hosted endpoint: check HTTPS_PROXY and NO_PROXY in deploy/.env"
      ;;
  esac
}
c_ports() {
  local i p bad=0 lst ours seen=" " msg=()
  read_ports
  for p in "${PORT_PROBLEMS[@]}"; do msg+=("$p"); bad=1; done
  for p in "${PORTS[@]}"; do
    if [[ "$seen" == *" $p "* ]]; then msg+=("port $p is set for two services"); bad=1; fi
    seen+="$p "
  done
  if ! lst=$(listeners); then
    res H-PORTS fail "ss -ltnH failed" "the published ports free on 127.0.0.1" "install iproute2"
    return
  fi
  # This stack's own published ports (the pmos project's containers): in use by the stack itself.
  ours=$(cmd_out docker-ps-ports docker ps --filter label=com.docker.compose.project=pmos --format '{{.Ports}}' || true)
  for i in "${!PORTS[@]}"; do
    p=${PORTS[$i]}
    if awk -v p="$p" '$2 == p { f = 1 } END { exit !f }' <<<"$lst"; then
      if [[ "$ours" == *"127.0.0.1:$p->"* ]]; then
        continue
      fi
      msg+=("$p is in use by something other than this stack")
      bad=1
    fi
  done
  if [ "$bad" = 0 ]; then
    res H-PORTS pass "${PORTS[*]} free or this stack's" "the published ports free on 127.0.0.1"
  else
    res H-PORTS fail "$(join_sc "${msg[@]}")" "the published ports free on 127.0.0.1" "stop what holds them, or set PMOS_*_PORT in deploy/.env"
  fi
}
network_settings_problems() {
  local subnet gw range sn sb rn rb g a an i bcast held subnet_ok=0
  subnet=$(ev PMOS_NETWORK_SUBNET)
  subnet=${subnet:-$NETWORK_SUBNET_DEFAULT}
  gw=$(ev PMOS_NETWORK_GATEWAY)
  gw=${gw:-$NETWORK_GATEWAY_DEFAULT}
  range=$(ev PMOS_NETWORK_IP_RANGE)
  range=${range:-$NETWORK_IP_RANGE_DEFAULT}
  if ! read -r sn sb < <(ipv4_cidr "$subnet"); then
    echo "PMOS_NETWORK_SUBNET=$(printf '%q' "$subnet") isn't an IPv4 network with its host bits clear"
  elif [ "$sb" -lt 24 ]; then
    echo "PMOS_NETWORK_SUBNET=$subnet is shorter than a /24"
  else
    subnet_ok=1
    bcast=$((sn | ((1 << (32 - sb)) - 1)))
    held=1
    # The three services' first: the policy pins each name to its own /32 (slice H3).
    for i in "${!PINNED_SERVICE_ADDRS[@]}"; do
      a=${PINNED_SERVICE_ADDRS[$i]}
      an=$(ipv4_strict "$a")
      if ! ipv4_num_in "$an" "$sn" "$sb" || [ "$an" = "$sn" ] || [ "$an" = "$bcast" ]; then
        echo "PMOS_NETWORK_SUBNET=$subnet doesn't hold $a, where every sandbox's policy allows ${PINNED_NAMES[$i]} (allowed_ips $a/32, the policy crate's constant)"
        held=0
        break
      fi
    done
    if [ "$held" = 1 ]; then
      for a in "${PINNED_ADDRS[@]}"; do
        an=$(ipv4_strict "$a")
        if ! ipv4_num_in "$an" "$sn" "$sb" || [ "$an" = "$sn" ] || [ "$an" = "$bcast" ]; then
          echo "PMOS_NETWORK_SUBNET=$subnet doesn't hold the pinned address $a"
          break
        fi
      done
    fi
  fi
  if ! g=$(ipv4_strict "$gw"); then
    echo "PMOS_NETWORK_GATEWAY=$(printf '%q' "$gw") isn't an IPv4 address"
  else
    for a in "${PINNED_ADDRS[@]}"; do [ "$gw" != "$a" ] || echo "PMOS_NETWORK_GATEWAY=$gw is a pinned address"; done
    if [ "$subnet_ok" = 1 ] && { ! ipv4_num_in "$g" "$sn" "$sb" || [ "$g" = "$sn" ] || [ "$g" = "$bcast" ]; }; then
      echo "PMOS_NETWORK_GATEWAY=$gw isn't inside PMOS_NETWORK_SUBNET=$subnet"
    fi
  fi
  if ! read -r rn rb < <(ipv4_cidr "$range"); then
    echo "PMOS_NETWORK_IP_RANGE=$(printf '%q' "$range") isn't an IPv4 network with its host bits clear"
  else
    if [ "$subnet_ok" = 1 ] && { [ "$rb" -lt "$sb" ] || ! ipv4_num_in "$rn" "$sn" "$sb"; }; then
      echo "PMOS_NETWORK_IP_RANGE=$range isn't inside PMOS_NETWORK_SUBNET=$subnet"
    fi
    for a in "${PINNED_ADDRS[@]}"; do
      if ipv4_num_in "$(ipv4_strict "$a")" "$rn" "$rb"; then
        echo "PMOS_NETWORK_IP_RANGE=$range holds the pinned address $a"
        break
      fi
    done
  fi
}
c_subnet() {
  local subnet sn sb probs=() p routes line dest dev dn db live want entry list=() tp gwv rangev
  subnet=$(ev PMOS_NETWORK_SUBNET)
  subnet=${subnet:-$NETWORK_SUBNET_DEFAULT}
  gwv=$(ev PMOS_NETWORK_GATEWAY)
  gwv=${gwv:-$NETWORK_GATEWAY_DEFAULT}
  rangev=$(ev PMOS_NETWORK_IP_RANGE)
  rangev=${rangev:-$NETWORK_IP_RANGE_DEFAULT}
  while IFS= read -r p; do [ -z "$p" ] || probs+=("$p"); done < <(network_settings_problems)
  # Routes that overlap the subnet, other than Docker's own pmos0 bridge.
  if read -r sn sb < <(ipv4_cidr "$subnet"); then
    if routes=$(cmd_out ip-route ip -4 route show); then
      while IFS= read -r line; do
        [ -n "$line" ] || continue
        dest=${line%% *}
        dev=$(sed -nE 's/.* dev ([^ ]+).*/\1/p' <<<"$line")
        [ "$dest" != default ] || continue
        [[ "$dest" == */* ]] || dest="$dest/32"
        read -r dn db < <(ipv4_cidr "$dest") || continue
        if { [ "$db" -ge "$sb" ] && ipv4_num_in "$dn" "$sn" "$sb"; } || { [ "$sb" -ge "$db" ] && ipv4_num_in "$sn" "$dn" "$db"; }; then
          [ "$dev" = "$NETWORK_BRIDGE" ] || probs+=("the route $dest (dev ${dev:-?}) overlaps PMOS_NETWORK_SUBNET=$subnet")
        fi
      done <<<"$routes"
    else
      probs+=("ip route failed (is iproute2 installed?)")
    fi
  fi
  # A trusted proxy for the web app that is a pinned address would be believed about which
  # browser it speaks for, while the address belongs to a service (H2 left this to H1).
  tp=$(ev PMOS_WEB_TRUSTED_PROXIES)
  IFS=, read -ra list <<<"$tp"
  for entry in "${list[@]}"; do
    entry=${entry//[[:space:]]/}
    for p in "${PINNED_ADDRS[@]}"; do
      [ "$entry" != "$p" ] || probs+=("PMOS_WEB_TRUSTED_PROXIES names $entry, a pinned service address (10.201.77.10 to .14)")
    done
  done
  # The live network, at stage run (made by `up`; Compose doesn't change one that exists).
  if [ "$STAGE" = run ]; then
    live=$(cmd_out docker-network-inspect docker network inspect "$NETWORK_NAME" --format "$NETWORK_INSPECT_FORMAT" || true)
    if [ -n "$live" ]; then
      want="$subnet $gwv $rangev;$NETWORK_BRIDGE"
      [ "$live" = "$want" ] || probs+=("the live $NETWORK_NAME network is \"$live\", deploy/.env and compose.yaml say \"$want\": deploy/pmos-admin down && deploy/pmos-admin up")
    fi
  fi
  if [ ${#probs[@]} -eq 0 ]; then
    res H-SUBNET pass "$subnet; gateway $gwv; range $rangev" "the pmos network as pinned"
  else
    res H-SUBNET fail "$(join_sc "${probs[@]}")" "the pmos network as pinned" "see docs/openshell-any-host-spec.md §3.3 and §3.5: the addresses are the policy crate's constants, moved only by a code change under POL-2"
  fi
}
# The managed block (as deploy/pmos-admin host-names check, slice H2).
HOSTS_BEGIN='# BEGIN pmos (managed by pmos-admin)'
HOSTS_END='# END pmos'
HOSTS_BEGIN_RE='^#[[:space:]]*BEGIN[[:space:]]+pmos([[:space:]]|$)'
HOSTS_END_RE='^#[[:space:]]*END[[:space:]]+pmos([[:space:]]|$)'
c_names() {
  local f probs=() line in=0 blocks=0 block="" cur="" outside=() l fl addr name i ans want expected
  if [ "$STAGE" = install ]; then
    res H-NAMES pass "not set yet" "the three names resolve on this host to .10, .11, .12"
    return
  fi
  f=$(hp /etc/hosts)
  if [ ! -r "$f" ]; then
    res H-NAMES fail "/etc/hosts can't be read" "the managed block" "sudo deploy/pmos-admin host-names apply"
    return
  fi
  expected=$HOSTS_BEGIN$'\n'
  for i in "${!PINNED_NAMES[@]}"; do expected+="${PINNED_SERVICE_ADDRS[$i]} ${PINNED_NAMES[$i]}"$'\n'; done
  expected+=$HOSTS_END$'\n'
  while IFS= read -r line || [ -n "$line" ]; do
    if [[ "$line" =~ $HOSTS_BEGIN_RE ]]; then
      [ "$in" = 0 ] || { probs+=("broken pmos markers in /etc/hosts"); break; }
      in=1 cur="$line"$'\n'
    elif [[ "$line" =~ $HOSTS_END_RE ]]; then
      [ "$in" = 1 ] || { probs+=("broken pmos markers in /etc/hosts"); break; }
      in=0 cur+="$line"$'\n' blocks=$((blocks + 1))
      [ "$blocks" != 1 ] || block=$cur
    elif [ "$in" = 1 ]; then
      cur+="$line"$'\n'
    else
      outside+=("$line")
    fi
  done <"$f"
  [ "$in" = 0 ] || probs+=("a pmos block in /etc/hosts starts and never ends")
  if [ "$blocks" = 0 ]; then
    probs+=("no managed pmos block in /etc/hosts")
  elif [ "$blocks" != 1 ]; then
    probs+=("$blocks managed pmos blocks in /etc/hosts")
  elif [ "$block" != "$expected" ]; then
    probs+=("the managed pmos block in /etc/hosts is stale")
  fi
  for line in "${outside[@]}"; do
    l=${line%%#*}
    read -r -a fl <<<"$l" || true
    [ ${#fl[@]} -ge 2 ] || continue
    addr=${fl[0]}
    for name in "${fl[@]:1}"; do
      name=${name,,}
      name=${name%.}
      for i in "${PINNED_NAMES[@]}"; do
        [ "$name" = "$i" ] || continue
        if is_loopback "$addr" || [ "$addr" = 0.0.0.0 ]; then
          probs+=("/etc/hosts maps $name to loopback ($addr) outside the managed block")
        else
          probs+=("/etc/hosts has a second line for $name outside the managed block ($addr)")
        fi
      done
    done
  done
  for i in "${!PINNED_NAMES[@]}"; do
    name=${PINNED_NAMES[$i]}
    want=${PINNED_SERVICE_ADDRS[$i]}
    ans=$(cmd_out "getent-$name" getent ahostsv4 "$name" | awk '{ print $1 }' | sort -u | tr '\n' ' ' || true)
    ans=${ans% }
    if [ -z "$ans" ]; then
      probs+=("$name doesn't resolve on this host")
    elif [ "$ans" != "$want" ]; then
      probs+=("$name resolves on this host to $ans, not $want")
    fi
  done
  if [ ${#probs[@]} -eq 0 ]; then
    res H-NAMES pass "toolserver .10, gateway .11, bots .12 (the host's view; the sandbox's is O-REACH)" "the three names resolve on this host to .10, .11, .12"
  else
    res H-NAMES fail "$(join_sc "${probs[@]}")" "the three names resolve on this host to .10, .11, .12" "sudo deploy/pmos-admin host-names apply"
  fi
}

# H-METADATA's probe, run in a container on the pmos network (or Docker's default bridge before
# the network exists). Exactly three requests can be sent; no answer's body is kept, printed or
# logged: only the status line, whether Metadata-Flavor: Google came back, and whether GCP lists
# any service account. It prints one word. Never a request for a token or a credential: not AWS's
# /latest/meta-data/iam/security-credentials/, not GCP's …/token, not Azure's oauth2/token.
#   none           no answer at all
#   gcp-nosa       GCP, no service account attached
#   gcp-sa         GCP, a service account attached
#   aws-hop1       AWS with IMDSv2 required: GET answers 401, the token PUT gets no answer here
#   aws-open       AWS: the token PUT answers this container (hop limit above 1)
#   imdsv1         an answer to GET / with 200 that isn't GCP's (AWS's IMDSv1 or anything alike)
#   azure          400 without the Metadata: true header (Azure's answer)
#   other-<code>   any other answer
# The AWS PUT asks for an IMDSv2 session token (one second): its body is discarded unread.
# shellcheck disable=SC2016 # run by sh in the probe container, not here
METADATA_PROBE='m=http://169.254.169.254
h=$(curl -s -o /dev/null -D - -m 2 "$m/" 2>/dev/null) || { echo none; exit 0; }
code=$(printf "%s\n" "$h" | head -1 | cut -d" " -f2)
if printf "%s\n" "$h" | grep -qi "^metadata-flavor: *google"; then
  n=$(curl -s -m 2 -H "Metadata-Flavor: Google" "$m/computeMetadata/v1/instance/service-accounts/" 2>/dev/null | head -c 4096 | tr -d "[:space:]" | wc -c)
  if [ "$n" = 0 ]; then echo gcp-nosa; else echo gcp-sa; fi
  exit 0
fi
case "$code" in
  400) echo azure; exit 0 ;;
  200) echo imdsv1; exit 0 ;;
  401)
    if curl -s -o /dev/null -m 2 -X PUT -H "X-aws-ec2-metadata-token-ttl-seconds: 1" "$m/latest/api/token" 2>/dev/null; then
      echo aws-open
    else
      echo aws-hop1
    fi
    exit 0 ;;
  *) echo "other-$code"; exit 0 ;;
esac'
PROBE_RESULT="" # the word above, or "" when the probe couldn't run
metadata_probe() {
  local net image out
  [ -z "$PROBE_RESULT" ] || return 0
  if [ -n "$ROOT" ]; then
    out=$(cmd_out metadata-probe true) || return 1
  else
    command -v docker >/dev/null || return 1
    image=$(ev PMOS_VERSION)
    image=pmos:${image:-local}
    docker image inspect "$image" >/dev/null 2>&1 || return 1
    if docker network inspect "$NETWORK_NAME" >/dev/null 2>&1; then net=$NETWORK_NAME; else net=bridge; fi
    out=$(timeout 30 docker run --rm --network "$net" --entrypoint sh "$image" -c "$METADATA_PROBE" 2>/dev/null) || return 1
  fi
  out=$(head -1 <<<"$out")
  [[ "$out" =~ ^(none|gcp-nosa|gcp-sa|aws-hop1|aws-open|imdsv1|azure|other-[0-9]*)$ ]] || return 1
  PROBE_RESULT=$out
}
# The block (pmos-admin host-firewall apply), where it was applied: its rules, the docker.service
# drop-in and the timer. Prints each thing missing.
BLOCK_CONF=/etc/pmos/metadata-block.conf
BLOCK_SCRIPT=/usr/local/sbin/pmos-metadata-block
BLOCK_DROPIN=/etc/systemd/system/docker.service.d/pmos-metadata-block.conf
BLOCK_STATUS=/run/pmos/metadata-block.status
BLOCK_DROPIN_TEXT='[Service]
ExecStartPost=-/usr/local/sbin/pmos-metadata-block apply'
block_problems() {
  local uid st now t
  [ -f "$(hp "$BLOCK_CONF")" ] || return 0
  # The rules: as root, the installed script's own check; otherwise (DOCKER-USER can't be read
  # without root) the status the timer's run writes every minute, which must be "ok" and recent.
  uid=$(cmd_out id-u id -u || echo "")
  if [ "$uid" = 0 ]; then
    cmd_out metadata-block-check "$(hp "$BLOCK_SCRIPT")" check >/dev/null || echo "a metadata-block rule is missing from DOCKER-USER"
  else
    st=$(cat -- "$(hp "$BLOCK_STATUS")" 2>/dev/null || true)
    now=$(cmd_out now date +%s) || now=$(date +%s) # a fixture without cmd/now: this clock
    t=${st#ok }
    if [[ "$st" != "ok "* ]] || ! [[ "$t" =~ ^[0-9]+$ ]] || [ $((now - t)) -gt 180 ] || [ $((now - t)) -lt -60 ]; then
      echo "the metadata-block rules weren't confirmed in the last 3 minutes ($BLOCK_STATUS: ${st:-missing}); run sudo deploy/pmos-admin host-firewall check"
    fi
  fi
  [ "$(cat -- "$(hp "$BLOCK_DROPIN")" 2>/dev/null)" = "$BLOCK_DROPIN_TEXT" ] || echo "the docker.service drop-in $BLOCK_DROPIN is missing or changed"
  [ "$(cmd_out timer-enabled systemctl is-enabled pmos-metadata-block.timer || true)" = enabled ] || echo "the timer pmos-metadata-block.timer isn't enabled"
  [ "$(cmd_out timer-active systemctl is-active pmos-metadata-block.timer || true)" = active ] || echo "the timer pmos-metadata-block.timer isn't active"
}
c_metadata() {
  local verdict hint="" probs=() p
  if [ "$STAGE" = install ]; then
    res H-METADATA pass "not set yet" "no container can get the VM's cloud credentials"
    return
  fi
  while IFS= read -r p; do [ -z "$p" ] || probs+=("$p"); done < <(block_problems)
  if ! metadata_probe; then
    res H-METADATA fail "not measurable: the probe couldn't run (it needs docker and the pmos image: deploy/pmos-admin build)" "no container can get the VM's cloud credentials"
    return
  fi
  case "$PROBE_RESULT" in
    none | gcp-nosa | aws-hop1) verdict=pass ;;
    gcp-sa) verdict=fail hint="detach the VM's service account, or apply the block: sudo deploy/pmos-admin host-firewall apply" ;;
    aws-open | imdsv1) verdict=fail hint="set the instance's metadata to IMDSv2 required with hop limit 1, or apply the block: sudo deploy/pmos-admin host-firewall apply" ;;
    *) verdict=fail hint="apply the block: sudo deploy/pmos-admin host-firewall apply" ;;
  esac
  if [ ${#probs[@]} -gt 0 ]; then
    verdict=fail
    hint="sudo deploy/pmos-admin host-firewall apply puts the rules, the drop-in and the timer back"
  fi
  res H-METADATA "$verdict" "probe: $PROBE_RESULT${probs[*]:+; $(join_sc "${probs[@]}")}" "no container can get the VM's cloud credentials" "$hint"
}
c_exposure() {
  local lst addr port p probs=() d0 ok
  read_ports
  if ! lst=$(listeners); then
    res H-EXPOSURE fail "ss -ltnH failed" "PM OS ports on 127.0.0.1 only" "install iproute2"
    return
  fi
  d0=$(cmd_out docker0-addr ip -4 -o addr show dev docker0 | awk '{ sub(/\/.*/, "", $4); print $4 }' || true)
  ADMIN_PUBLISH=$(ev PMOS_WEB_ADMIN_PUBLISH)
  ADMIN_PUBLISH=${ADMIN_PUBLISH:-127.0.0.1}
  while read -r addr port; do
    [ -n "$port" ] || continue
    ok=1
    if [ "$port" = "$INTERNAL_PORT" ]; then
      # Not published (deploy/compose.yaml): a host listener here, on any address, loopback
      # included, means the internal route was published or something else took the port.
      ok=0
    elif [ "$port" = "$OPENSHELL_PORT" ]; then
      # OpenShell's gateway: loopback, or the host's address on docker0 (which one it binds is
      # verify on a real host).
      is_loopback "$addr" || { [ -n "$d0" ] && [ "$addr" = "$d0" ]; } || ok=0
    else
      for p in "${PORTS[@]}"; do
        [ "$port" = "$p" ] && ! is_loopback "$addr" && ok=0
      done
      # 8772, the admin door (slice H10): only on PMOS_WEB_ADMIN_PUBLISH (127.0.0.1 by default).
      if [ "$port" = "$ADMIN_PORT" ] && [ "$addr" != "$ADMIN_PUBLISH" ]; then ok=0; fi
    fi
    [ "$ok" = 1 ] || probs+=("$addr:$port")
  done <<<"$lst"
  if [ ${#probs[@]} -eq 0 ]; then
    res H-EXPOSURE pass "${PORTS[*]} on loopback only; $ADMIN_PORT on $ADMIN_PUBLISH only; $INTERNAL_PORT not published; $OPENSHELL_PORT on loopback${d0:+ or $d0}" "nothing of PM OS listens outside loopback but the reverse proxy"
  else
    res H-EXPOSURE fail "listening outside loopback: ${probs[*]}" "nothing of PM OS listens outside loopback but the reverse proxy" "publish every PM OS port on 127.0.0.1 only, the admin door ($ADMIN_PORT) on PMOS_WEB_ADMIN_PUBLISH only, and never $INTERNAL_PORT (deploy/compose.yaml), and keep OpenShell's gateway local"
  fi
}
ACK_PRIVATE=0 # 1 when H-PRIVATE passes on its acknowledgement
web_on() {
  local s
  [ "$WEB_FLAG" = 0 ] || return 0
  s=$(cmd_out docker-ps-services docker ps --filter label=com.docker.compose.project=pmos --format '{{.Label "com.docker.compose.service"}}' || true)
  grep -qx web <<<"$s"
}
c_devlogin() {
  local dev
  dev=$(ev PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY)
  if [ "$dev" != 1 ]; then
    res H-DEVLOGIN pass "off" "development sign-in off"
  elif [ "$EXPOSURE" = public ]; then
    res H-DEVLOGIN fail "on, PMOS_EXPOSURE=public" "development sign-in off" "set PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=0: anyone who reaches it signs in as anyone"
  elif [ "$ACK_PRIVATE" = 1 ] || [ -n "$(ev PMOS_HOST_ACK_H_PRIVATE)" ]; then
    res H-DEVLOGIN fail "on, on a private declaration that is only acknowledged" "development sign-in off" "an acknowledged private host is treated as public here: turn development sign-in off"
  else
    # Allowed on a host that is private by H-PRIVATE's own measure (a closed test network); the
    # spec fails it only on public and on an acknowledged private.
    res H-DEVLOGIN pass "on, on a host that measures private (a closed test network only)" "development sign-in off, or a closed private network"
  fi
}
# Slice H10 (decision D2 (a)): on `public`, password sign-in only from PMOS_WEB_PASSWORD_CLIENTS,
# each entry no wider than a /16 (IPv4) or a /48 (IPv6); a too-wide entry fails on every host
# (pmos-web refuses it everywhere). Development sign-in on public is H-DEVLOGIN's.
password_clients_problems() { # the list → one line per bad entry
  python3 -I -c '
import ipaddress, sys
for e in (x.strip() for x in sys.argv[1].split(",")):
    if not e:
        continue
    try:
        if "/" not in e:
            raise ValueError
        n = ipaddress.ip_network(e, strict=True)
    except ValueError:
        print(f"{e}: not a network written address/length with no host bits"); continue
    mapped = n.version == 6 and n.network_address.ipv4_mapped is not None
    if (n.version == 4 and n.prefixlen < 16) or (n.version == 6 and n.prefixlen < 48) or (mapped and n.prefixlen < 112):
        print(f"{e}: wider than a /16 (IPv4) or a /48 (IPv6)")
' "$1"
}
c_signin() {
  local clients probs
  clients=$(ev PMOS_WEB_PASSWORD_CLIENTS)
  if ! web_on; then
    res H-SIGNIN pass "the web app is off" "sign-in as decision D2 (a) says"
    return
  fi
  probs=$(password_clients_problems "$clients")
  if [ -n "$probs" ]; then
    res H-SIGNIN fail "PMOS_WEB_PASSWORD_CLIENTS: $(paste -sd';' <<<"$probs")" "sign-in as decision D2 (a) says" "name only the VPN's or the office's own ranges, each no wider than a /16 (IPv4) or a /48 (IPv6); 0.0.0.0/0, ::/0 and ::ffff:0:0/96 are the internet"
  elif [ "$EXPOSURE" = public ] && [ "$(ev PMOS_WEB_PASSWORD_LOGIN)" = 1 ] && [ -z "$clients" ]; then
    res H-SIGNIN fail "PMOS_EXPOSURE=public with password sign-in and PMOS_WEB_PASSWORD_CLIENTS blank" "sign-in as decision D2 (a) says" "set PMOS_WEB_PASSWORD_CLIENTS (a VPN or office range), or turn password sign-in off and use Microsoft sign-in (decision D2); pmos-web refuses to start as it is"
  elif [ "$EXPOSURE" = public ] && [ "$(ev PMOS_WEB_PASSWORD_LOGIN)" = 1 ]; then
    res H-SIGNIN pass "PMOS_EXPOSURE=public: Microsoft sign-in, and password sign-in only from $clients" "sign-in as decision D2 (a) says"
  elif [ "$EXPOSURE" = public ]; then
    res H-SIGNIN pass "PMOS_EXPOSURE=public: Microsoft sign-in only" "sign-in as decision D2 (a) says"
  else
    res H-SIGNIN pass "the web app on, PMOS_EXPOSURE=private${clients:+; password sign-in only from $clients}" "sign-in as decision D2 (a) says"
  fi
}
# Slice H10 (decision D3 (b), every host; r7, X6): the admin door's settings, and any nginx server or
# Caddy site that forwards to its publish address and port while listening on a public address (or
# on the unspecified one while the host has a public address). Only what nginx -T and caddy adapt
# print is read; another kind of forwarder (socat, HAProxy, ssh -R) isn't seen (§3.8, a stated limit).
proxy_forwarders() { # publish port public-host(0|1) → one "<what> listens on <addr>" line per bad one
  local nginx caddy
  nginx=$(cmd_out nginx-T nginx -T || true)
  caddy=""
  if [ -n "$ROOT" ] || command -v caddy >/dev/null 2>&1; then
    caddy=$(cmd_out caddy-adapt caddy adapt --config /etc/caddy/Caddyfile || true)
  fi
  NGINX="$nginx" CADDY="$caddy" python3 -I -c '
import ipaddress, json, os, re, sys
publish, port, public_host = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
targets = {f"{publish}:{port}"}
if publish.startswith("127."):
    targets |= {f"localhost:{port}", f"127.0.0.1:{port}"}
def bad_listen(addr):
    a = addr.strip()
    if a.startswith("["):
        host = a[1:a.index("]")]
    elif a.count(":") == 1:
        host = a.split(":")[0]
    elif re.fullmatch(r"\d+", a) or a == "":
        host = ""
    else:
        host = a
    if host in ("", "*", "0.0.0.0", "::"):
        return public_host
    try:
        ip = ipaddress.ip_address(host)
    except ValueError:
        return True  # a name: unknown, so not trusted
    nets = [ipaddress.ip_network(n) for n in ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16",
            "100.64.0.0/10", "127.0.0.0/8", "::1/128", "fc00::/7")]
    return not any(ip.version == n.version and ip in n for n in nets)
def forwards(text):
    for m in re.finditer(r"proxy_pass\s+\w+://([^/;\s]+)", text):
        if m.group(1) in targets:
            return True
    return False
# nginx -T: each file follows a "# configuration file <path>:" line; includes are inlined.
text = os.environ.get("NGINX", "")
files, cur, name = {}, [], None
for line in text.splitlines():
    m = re.match(r"# configuration file (.+):$", line)
    if m:
        if name: files[name] = "\n".join(cur)
        name, cur = m.group(1), []
    else:
        cur.append(line)
if name: files[name] = "\n".join(cur)
if not files and text:
    files["nginx"] = text
def inline(body, depth=0):
    def sub(m):
        if depth > 5: return ""
        return "\n".join(inline(files[f], depth + 1) for f in files if f == m.group(1) or f.endswith("/" + m.group(1)))
    return re.sub(r"\binclude\s+([^;]+);", sub, body)
for body in files.values():
    body = re.sub(r"#.*", "", inline(body))
    i = 0
    while True:
        m = re.search(r"\bserver\s*\{", body[i:])
        if not m: break
        start = i + m.end()
        level, j = 1, start
        while j < len(body) and level:
            level += {"{": 1, "}": -1}.get(body[j], 0)
            j += 1
        block = body[start:j - 1]
        i = j
        if not forwards(block):
            continue
        for lm in re.finditer(r"^\s*listen\s+([^;\s]+)", block, re.M):
            if bad_listen(lm.group(1)):
                print(f"an nginx server forwarding to {publish}:{port} listens on {lm.group(1)}")
# caddy adapt: JSON.
try:
    c = json.loads(os.environ.get("CADDY") or "{}")
except ValueError:
    c = {}
for sname, srv in ((c.get("apps") or {}).get("http") or {}).get("servers", {}).items():
    dials = json.dumps(srv.get("routes", []))
    if not any(f"\"dial\": \"{t}\"" in dials for t in targets):
        continue
    for l in srv.get("listen", []):
        addr = l.split("/", 1)[-1] if "/" in l else l
        if bad_listen(addr):
            print(f"a Caddy site ({sname}) forwarding to {publish}:{port} listens on {l}")
' "$1" "$2" "$3"
}
c_admin() {
  local url publish proxies probs=() host fwd public_host=0 kind
  if ! web_on; then
    res H-ADMIN skip "the web app is off" "the admin routes on their own listener"
    return
  fi
  url=$(ev PMOS_WEB_ADMIN_URL)
  publish=$(ev PMOS_WEB_ADMIN_PUBLISH)
  publish=${publish:-127.0.0.1}
  proxies=$(ev PMOS_WEB_ADMIN_TRUSTED_PROXIES)
  [ -n "$url" ] || probs+=("PMOS_WEB_ADMIN_URL is blank (the admin door needs its own address)")
  kind=$(addr_class "$publish" | awk '{ print $2 }')
  case "$publish" in
    127.* | ::1) kind=loopback ;;
    169.254.* | fe80:*) kind=public ;; # link-local isn't a place to publish the admin door
  esac
  [ "$kind" = private ] || [ "$kind" = loopback ] || probs+=("PMOS_WEB_ADMIN_PUBLISH=$publish is outside loopback, RFC 1918, 100.64.0.0/10 and IPv6 unique-local")
  case "$url" in
    http://*)
      [ "$kind" = loopback ] || probs+=("an http admin URL (the tunnel mode) with PMOS_WEB_ADMIN_PUBLISH=$publish, which isn't loopback")
      [ -z "$proxies" ] || probs+=("an http admin URL with PMOS_WEB_ADMIN_TRUSTED_PROXIES set")
      host=${url#http://}
      host=${host%%/*}
      case "${host%:*}" in
        localhost | *.localhost | 127.0.0.1 | '[::1]') ;;
        *) probs+=("an http admin URL on $host, which isn't a loopback name") ;;
      esac
      ;;
    https://*) [ -n "$proxies" ] || probs+=("an https admin URL without PMOS_WEB_ADMIN_TRUSTED_PROXIES") ;;
    "") ;;
    *) probs+=("PMOS_WEB_ADMIN_URL=$url is neither http nor https") ;;
  esac
  # A host whose addresses can't be read counts as having a public one (the stricter reading).
  if ! measure_public_addrs 2>/dev/null || [ -n "$PRIV_PUBLIC" ]; then public_host=1; fi
  while IFS= read -r fwd; do [ -z "$fwd" ] || probs+=("$fwd"); done < <(proxy_forwarders "$publish" "$ADMIN_PORT" "$public_host")
  if [ ${#probs[@]} -gt 0 ]; then
    res H-ADMIN fail "$(join_sc "${probs[@]}")" "the admin routes on their own listener" "docs/runbooks/install.md, \"The admin door\": an http://<deployment>.localhost:8772 admin URL with PMOS_WEB_ADMIN_PUBLISH=127.0.0.1 (an SSH tunnel), or an https one with PMOS_WEB_ADMIN_TRUSTED_PROXIES and a TLS terminator listening on a private address only"
    return
  fi
  case "$url" in
    http://localhost:* | http://127.0.0.1:* | 'http://[::1]:'*)
      res H-ADMIN warn "PMOS_WEB_ADMIN_URL=$url, a bare loopback name" "the admin routes on their own listener" "every tunnel to a bare localhost shares its cookies: use a deployment-unique name such as http://pmos-prod.localhost:8772"
      ;;
    *) res H-ADMIN pass "admin door $url, published on $publish:$ADMIN_PORT" "the admin routes on their own listener" ;;
  esac
}

# H-PRIVATE's measurement: the sorted public addresses on this host, and whether the metadata
# probe got any answer (yes, no, or unknown when it couldn't run). The fingerprint is a SHA-256 of
# both, so the same host keeps its fingerprint and a new public address changes it.
PRIV_PUBLIC="" PRIV_PROBE=unknown PRIV_ERR=""
measure_private() {
  measure_public_addrs || return 1
  if metadata_probe; then
    if [ "$PROBE_RESULT" = none ]; then PRIV_PROBE=no; else PRIV_PROBE=yes; fi
  fi
}
# The sorted public interface addresses only (PRIV_PUBLIC), without the metadata probe.
measure_public_addrs() {
  local j addrs
  if ! j=$(cmd_out ip-addr ip -j addr); then
    PRIV_ERR="ip -j addr failed"
    return 1
  fi
  addrs=$(python3 -I -c '
import json, sys
try:
    for i in json.loads(sys.stdin.read()):
        for a in i.get("addr_info", []):
            if a.get("local"): print(a["local"])
except Exception:
    sys.exit(1)
' <<<"$j") || { PRIV_ERR="ip -j addr gave no readable answer"; return 1; }
  # shellcheck disable=SC2086 # one address per word
  PRIV_PUBLIC=$(addr_class $addrs | awk '$2 == "public" { print $1 }' | sort -u | paste -sd, -)
}
private_fingerprint() { # probe-answered (yes|no|unknown)
  printf 'public=%s\nmetadata=%s\n' "$PRIV_PUBLIC" "$1" | sha256sum | cut -d' ' -f1
}
private_saw() { printf 'public addresses: %s; metadata probe answered: %s' "${PRIV_PUBLIC:-none}" "$PRIV_PROBE"; }

# The acknowledgement log (pmos-admin host acknowledge / host attest): tab-separated lines
#   <time>\t<login>\t<check>\t<acknowledge|attest|clear>\t<what it saw>\t<fingerprint>\t<reason>
# A deploy/.env line counts only when the latest log line for its check records it with the
# same fingerprint (no later clear).
ACK_LOG=/var/log/pmos/host-acknowledgements.log
log_has() { # check fingerprint
  local f last
  f=$(hp "$ACK_LOG")
  [ -r "$f" ] || return 1
  last=$(awk -F'\t' -v c="$1" '$3 == c { l = $0 } END { print l }' "$f")
  [ -n "$last" ] || return 1
  awk -F'\t' -v fp="$2" '($4 == "acknowledge" || $4 == "attest") && $6 == fp { ok = 1 } END { exit !ok }' <<<"$last"
}
log_line() { # check → the latest log line for it
  local f
  f=$(hp "$ACK_LOG")
  [ -r "$f" ] || return 0
  awk -F'\t' -v c="$1" '$3 == c { l = $0 } END { if (l != "") print l }' "$f"
}
c_private() {
  local ack mismatch=0 lst addr port cls probs=() hp_ok=0 fps
  if [ "$EXPOSURE" != private ]; then
    res H-PRIVATE skip "PMOS_EXPOSURE=$EXPOSURE" "a host declared private looks private"
    return
  fi
  if ! measure_private; then
    res H-PRIVATE fail "not measurable: $PRIV_ERR" "a host declared private looks private"
    return
  fi
  [ -z "$PRIV_PUBLIC" ] || mismatch=1
  [ "$PRIV_PROBE" != yes ] || mismatch=1
  ack=$(ev PMOS_HOST_ACK_H_PRIVATE)
  if [ "$mismatch" = 0 ] && [ -z "$ack" ]; then
    res H-PRIVATE pass "$(private_saw)" "a host declared private looks private"
    return
  fi
  if [ -z "$ack" ]; then
    res H-PRIVATE fail "$(private_saw)" "a host declared private looks private" "the host has a public address or its metadata service answers: declare PMOS_EXPOSURE=public, or, on a closed test host, sudo deploy/pmos-admin host acknowledge H-PRIVATE --reason \"…\""
    return
  fi
  # The fingerprint as measured now; while the probe can't run (stage install, before the images
  # exist) only the addresses are compared, and the probe at the next `up`.
  if [ "$PRIV_PROBE" = unknown ]; then fps="$(private_fingerprint yes) $(private_fingerprint no)"; else fps=$(private_fingerprint "$PRIV_PROBE"); fi
  if [[ " $fps " != *" $ack "* ]]; then
    res H-PRIVATE fail "$(private_saw); the acknowledgement's fingerprint no longer matches" "a host declared private looks private" "the host changed since it was acknowledged: check it, then acknowledge again (sudo deploy/pmos-admin host acknowledge H-PRIVATE --reason \"…\")"
    return
  fi
  if ! log_has H-PRIVATE "$ack"; then
    res H-PRIVATE fail "PMOS_HOST_ACK_H_PRIVATE is set, with no entry in $ACK_LOG" "a host declared private looks private" "an acknowledgement is made only with sudo deploy/pmos-admin host acknowledge H-PRIVATE --reason \"…\", never by editing deploy/.env"
    return
  fi
  # Acknowledged: nothing may listen on 80 or 443 where the internet could reach a sign-in page.
  if ! lst=$(listeners); then
    res H-PRIVATE fail "ss -ltnH failed" "nothing on 80 or 443 outside the private ranges"
    return
  fi
  while read -r addr port; do
    [ "$port" = 80 ] || [ "$port" = 443 ] || continue
    read -r _ cls < <(addr_class "$addr")
    if [ "$cls" = public ] || { [ "$cls" = unspecified ] && [ -n "$PRIV_PUBLIC" ]; } || [ "$cls" = bad ]; then
      probs+=("$addr:$port")
    fi
  done <<<"$lst"
  if [ ${#probs[@]} -gt 0 ]; then
    res H-PRIVATE fail "acknowledged, but listening on ${probs[*]}" "nothing on 80 or 443 outside the private ranges" "an acknowledged private host may not serve 80 or 443 on a public or unspecified address: bind the proxy to a private address"
    return
  fi
  hp_ok=1
  ACK_PRIVATE=$hp_ok
  res H-PRIVATE warn "acknowledged: $(private_saw)" "a host declared private looks private" "$(log_line H-PRIVATE | awk -F'\t' '{ printf "acknowledged on %s by %s: \"%s\"", $1, $2, $7 }')"
}
c_assert() {
  local mode person=() k
  mode=$(ev PMOS_ASSERTION_MODE)
  for k in PMOS_RT_WEB_TOKEN PMOS_TS_WEB_TOKEN PMOS_RT_CONSOLE_TOKEN PMOS_CONSOLE_TS_TOKEN; do
    if is_set "$k"; then person+=("$k"); fi
  done
  case "$mode" in "" | off | verify | require) ;; *)
    res H-ASSERT fail "PMOS_ASSERTION_MODE=$mode" "require" "set PMOS_ASSERTION_MODE=require (docs/user-assertion.md)"
    return ;;
  esac
  if [ ${#person[@]} -gt 0 ] && [ -n "$mode" ] && [ "$mode" != require ]; then
    res H-ASSERT fail "PMOS_ASSERTION_MODE=$mode with ${person[*]} set" "require" "set PMOS_ASSERTION_MODE=require (docs/user-assertion.md)"
  else
    res H-ASSERT pass "PMOS_ASSERTION_MODE=${mode:-unset (require once a web or console token is set)}" "require"
  fi
}
# The `local` attestation's fingerprint: SHA-256 of /etc/machine-id and the sorted GPU UUIDs.
LOCAL_FP="" LOCAL_GPUS="" LOCAL_ERR=""
measure_local_gpu() {
  local mid list uuids
  mid=$(tr -d '[:space:]' <"$(hp /etc/machine-id)" 2>/dev/null || true)
  [ -n "$mid" ] || { LOCAL_ERR="/etc/machine-id is missing or empty"; return 1; }
  list=$(cmd_out nvidia-smi-L nvidia-smi -L) || { LOCAL_ERR="nvidia-smi -L failed (no NVIDIA driver?)"; return 1; }
  uuids=$(grep -oE 'UUID: [A-Za-z0-9-]+' <<<"$list" | cut -d' ' -f2 | sort -u)
  [ -n "$uuids" ] || { LOCAL_ERR="nvidia-smi -L lists no GPU"; return 1; }
  LOCAL_GPUS=$(sed -nE 's/^GPU [0-9]+: (.*) \(UUID: .*/\1/p' <<<"$list" | sort | uniq -c | awk '{ c = $1; $1 = ""; sub(/^ /, ""); printf "%s%dx %s", (n++ ? ", " : ""), c, $0 }')
  LOCAL_FP=$(printf 'machine-id=%s\ngpus=%s\n' "$mid" "$(paste -sd, - <<<"$uuids")" | sha256sum | cut -d' ' -f1)
}
c_local() {
  local att live=()
  if [ "$MS" != local ]; then
    res H-LOCAL skip "PMOS_MODEL_SOURCE=$MS" "an attestation for real data under local"
    return
  fi
  [ "$(ev PMOS_CONNECTORS)" != live ] || live+=("PMOS_CONNECTORS=live")
  [ "$(ev PMOS_MEMORY)" != github ] || live+=("PMOS_MEMORY=github")
  att=$(ev PMOS_HOST_ATTEST_LOCAL_GPU)
  if [ ${#live[@]} -eq 0 ]; then
    res H-LOCAL pass "no live connectors or GitHub memory${att:+ (attested)}" "an attestation for real data under local"
    return
  fi
  local hint="sudo deploy/pmos-admin host attest local-gpu --owner-controls --reason \"…\" (only on a GPU host the owner controls)"
  if [ -z "$att" ]; then
    res H-LOCAL fail "${live[*]} with no attestation" "a recorded attestation that the owner controls this GPU host" "$hint"
  elif ! log_has local-gpu "$att"; then
    res H-LOCAL fail "PMOS_HOST_ATTEST_LOCAL_GPU is set, with no entry in $ACK_LOG" "a recorded attestation that the owner controls this GPU host" "$hint"
  elif ! measure_local_gpu; then
    res H-LOCAL fail "not measurable: $LOCAL_ERR" "a recorded attestation that the owner controls this GPU host" "$hint"
  elif [ "$LOCAL_FP" != "$att" ]; then
    res H-LOCAL fail "the attestation's fingerprint no longer matches (another machine id or GPU)" "a recorded attestation that the owner controls this GPU host" "$hint"
  else
    res H-LOCAL pass "attested: $LOCAL_GPUS" "a recorded attestation that the owner controls this GPU host" "$(log_line local-gpu | awk -F'\t' '{ printf "attested on %s by %s: \"%s\"", $1, $2, $7 }')"
  fi
}
# Only H-PRIVATE takes an acknowledgement, and only local-gpu is attested: a line for anything
# else in deploy/.env is itself a fail (§3.2, P-8).
c_stray_acks() {
  local k id
  for k in "${!E[@]}"; do
    case "$k" in
      PMOS_HOST_ACK_H_PRIVATE | PMOS_HOST_ATTEST_LOCAL_GPU) ;;
      PMOS_HOST_ACK_* | PMOS_HOST_ATTEST_*)
        id=${k#PMOS_HOST_ACK_}
        id=${id#PMOS_HOST_ATTEST_}
        id=${id//_/-}
        want "$id" || want H-ACK || continue
        res "$id" fail "$k is set in deploy/.env" "no acknowledgement (only H-PRIVATE takes one)" "remove the line: only H-PRIVATE takes an acknowledgement, and only local-gpu an attestation, each made with sudo deploy/pmos-admin host …"
        ;;
    esac
  done
}

# --- OpenShell (--openshell) ----------------------------------------------------------------------
OS_BIN=""
c_ocli() {
  local v
  OS_BIN=$(openshell_bin) || OS_BIN=""
  if [ -z "$OS_BIN" ]; then
    res O-CLI fail "openshell isn't installed" "the OpenShell CLI, its gateway reachable" "install OpenShell from NVIDIA's install page"
    return
  fi
  v=$("$OS_BIN" --version 2>/dev/null | head -1) || v=""
  if ! "$OS_BIN" status >/dev/null 2>&1; then
    res O-CLI fail "${v:-openshell}; openshell status failed" "the OpenShell CLI, its gateway reachable" "systemctl --user status openshell-gateway"
    return
  fi
  res O-CLI pass "${v:-openshell}; status ok" "the OpenShell CLI, its gateway reachable"
}
# The endpoint the CLI uses, from `openshell status` (its output format is verify on a real host):
# https to loopback or the host's docker0 address only. Plaintext anywhere, or any other host, fails.
c_ogateway() {
  local bin out url scheme host d0
  bin=$(openshell_bin) || bin=""
  if [ -z "$bin" ]; then
    res O-GATEWAY fail "openshell isn't installed" "a local gateway over TLS"
    return
  fi
  out=$("$bin" status 2>/dev/null) || { res O-GATEWAY fail "openshell status failed" "a local gateway over TLS"; return; }
  url=$(grep -oE '[a-z]+://(\[[0-9A-Fa-f:.]+\]|[A-Za-z0-9.-]+)(:[0-9]+)?' <<<"$out" | head -1)
  if [ -z "$url" ]; then
    res O-GATEWAY fail "no endpoint in openshell status" "a local gateway over TLS" "verify on a real host: how openshell status names its endpoint"
    return
  fi
  scheme=${url%%://*}
  host=${url#*://}
  if [[ "$host" == \[* ]]; then
    host=${host#[}
    host=${host%%]*}
  else
    host=${host%%:*}
  fi
  d0=$(cmd_out docker0-addr ip -4 -o addr show dev docker0 | awk '{ sub(/\/.*/, "", $4); print $4 }' || true)
  if [ "$scheme" = https ] && { is_loopback "$host" || { [ -n "$d0" ] && [ "$host" = "$d0" ]; }; }; then
    res O-GATEWAY pass "$url" "a local gateway over TLS"
  else
    res O-GATEWAY fail "$url" "a local gateway over TLS" "keep OpenShell's gateway on 127.0.0.1 with mTLS; its plaintext mode is not used"
  fi
}
c_olinger() {
  local me l e
  me=$(cmd_out id-un id -un || true)
  l=$(cmd_out linger loginctl show-user "$me" -p Linger --value || true)
  e=$(cmd_out gateway-unit-enabled systemctl --user is-enabled openshell-gateway.service || true)
  if [ "$l" = yes ] && [ "$e" = enabled ]; then
    res O-LINGER pass "Linger=yes; openshell-gateway.service enabled" "the gateway and workers survive logout and reboot"
  else
    res O-LINGER fail "Linger=${l:-?}; openshell-gateway.service ${e:-?}" "the gateway and workers survive logout and reboot" "sudo loginctl enable-linger ${me:-<user>}; systemctl --user enable openshell-gateway.service"
  fi
}
# The two canaries share pmos-admin sandbox's create command (deploy/sandbox-create.sh), so they
# can't drift apart. Their policies are written to a private temporary directory.
# shellcheck source=deploy/sandbox-create.sh
. "$DEPLOY/sandbox-create.sh"
CANARY_POLICY='filesystem_policy:
  include_workdir: false
  read_only:
    - /dev/urandom
    - /etc
    - /lib
    - /proc
    - /usr
  read_write:
    - /dev/null
    - /tmp
landlock:
  compatibility: hard_requirement
network_policies: {}
process:
  run_as_group: sandbox
  run_as_user: sandbox
version: 1'
reach_policy() {
  local i name port ports=(8710 8740 8750) keys=(pmos_toolserver pmos_gateway pmos_runtime)
  printf '%s\n' "${CANARY_POLICY%%network_policies: \{\}*}network_policies:"
  for i in 0 1 2; do
    name=${PINNED_NAMES[$i]}
    port=${ports[$i]}
    # Each name only at its own pinned address, as in every bot's policy (slice H3, allowed_ips).
    printf '  %s:\n    binaries:\n      - path: /usr/local/bin/node\n    endpoints:\n      - allowed_ips:\n          - \"%s/32\"\n        enforcement: enforce\n        host: %s\n        port: %s\n        protocol: rest\n        rules:\n          - allow:\n              method: GET\n              path: /healthz\n    name: %s\n' \
      "${keys[$i]}" "${PINNED_SERVICE_ADDRS[$i]}" "$name" "$port" "${keys[$i]}"
  done
  printf '%s\n' "process:" "  run_as_group: sandbox" "  run_as_user: sandbox" "version: 1"
}
REACH_JS='const u=["http://toolserver.pmos.internal:8710/healthz","http://gateway.pmos.internal:8740/healthz","http://bots.pmos.internal:8750/healthz"];Promise.all(u.map(x=>fetch(x,{signal:AbortSignal.timeout(5000)}).then(r=>r.status,()=>0))).then(s=>{console.log(s.join(" "));process.exit(s.every(c=>c===200)?0:1)})'
CANARY_TMP=""
canary_cleanup() {
  [ -z "$OS_BIN" ] || "$OS_BIN" sandbox delete pmos-preflight >/dev/null 2>&1 || true
  [ -z "$OS_BIN" ] || "$OS_BIN" sandbox delete pmos-preflight-net >/dev/null 2>&1 || true
  [ -z "$CANARY_TMP" ] || rm -rf -- "$CANARY_TMP"
  CANARY_TMP=""
}
c_canaries() {
  local version out
  OS_BIN=$(openshell_bin) || OS_BIN=""
  if [ -z "$OS_BIN" ]; then
    want O-CANARY && res O-CANARY fail "openshell isn't installed" "a sandbox starts under PM OS's settings"
    want O-REACH && res O-REACH fail "openshell isn't installed" "a sandbox reaches the three services by name"
    return
  fi
  version=$(ev PMOS_VERSION)
  version=${version:-local}
  CANARY_TMP=$(mktemp -d "${TMPDIR:-/tmp}/pmos-preflight.XXXXXX")
  trap canary_cleanup EXIT
  printf '%s\n' "$CANARY_POLICY" >"$CANARY_TMP/canary.yaml"
  reach_policy >"$CANARY_TMP/reach.yaml"
  if want O-CANARY; then
    "$OS_BIN" sandbox delete pmos-preflight >/dev/null 2>&1 || true
    pmos_sandbox_create_cmd pmos-preflight "$version" "$CANARY_TMP/canary.yaml"
    if ! "$OS_BIN" "${SANDBOX_CREATE[@]}" >/dev/null 2>&1; then
      res O-CANARY fail "the sandbox didn't start (openshell ${SANDBOX_CREATE[*]})" "a sandbox starts under PM OS's settings" "see the gateway's logs: systemctl --user status openshell-gateway"
    elif ! "$OS_BIN" sandbox exec -n pmos-preflight -- /usr/local/bin/node -e 'process.exit(0)' >/dev/null 2>&1; then
      res O-CANARY fail "the sandbox started; /usr/local/bin/node didn't run in it" "a sandbox starts under PM OS's settings" "is pmos-worker:$version built (deploy/pmos-admin build), with node at /usr/local/bin/node?"
    elif ! "$OS_BIN" logs -n pmos-preflight --source sandbox 2>/dev/null | grep -q 'Landlock ruleset built'; then
      res O-CANARY fail "node ran; no 'Landlock ruleset built' in the sandbox's log" "a sandbox starts under PM OS's settings" "Landlock wasn't applied: check H-LSM and H-LANDLOCK"
    else
      res O-CANARY pass "pmos-preflight started, ran node, Landlock applied" "a sandbox starts under PM OS's settings"
    fi
    "$OS_BIN" sandbox delete pmos-preflight >/dev/null 2>&1 || true
  fi
  if want O-REACH; then
    "$OS_BIN" sandbox delete pmos-preflight-net >/dev/null 2>&1 || true
    pmos_sandbox_create_cmd pmos-preflight-net "$version" "$CANARY_TMP/reach.yaml"
    if ! "$OS_BIN" "${SANDBOX_CREATE[@]}" >/dev/null 2>&1; then
      res O-REACH fail "the sandbox pmos-preflight-net didn't start" "each /healthz answers 200 from a sandbox"
    else
      out=$("$OS_BIN" sandbox exec -n pmos-preflight-net -- /usr/local/bin/node -e "$REACH_JS" 2>/dev/null | head -1)
      if [ "$out" = "200 200 200" ]; then
        res O-REACH pass "toolserver, gateway, bots: 200 200 200" "each /healthz answers 200 from a sandbox"
      else
        res O-REACH fail "toolserver, gateway, bots: ${out:-no answer}" "each /healthz answers 200 from a sandbox" "the sandbox can't reach the services by name: stop and report (docs/openshell-any-host-spec.md §3.3 step 6)"
      fi
    fi
    "$OS_BIN" sandbox delete pmos-preflight-net >/dev/null 2>&1 || true
  fi
  canary_cleanup
  trap - EXIT
}

# --- Fingerprints for pmos-admin ------------------------------------------------------------------
if [ -n "$FINGERPRINT" ]; then
  case "$FINGERPRINT" in
    h-private)
      measure_private || die "$PRIV_ERR"
      [ "$PRIV_PROBE" != unknown ] || die "the metadata probe couldn't run (it needs docker and the pmos image: deploy/pmos-admin build)"
      if [ -z "$PRIV_PUBLIC" ] && [ "$PRIV_PROBE" = no ]; then
        printf 'nothing to acknowledge: the host looks private\n' >&2
        exit 1
      fi
      printf '%s\t%s\n' "$(private_fingerprint "$PRIV_PROBE")" "$(private_saw)"
      ;;
    local-gpu)
      measure_local_gpu || die "$LOCAL_ERR"
      printf '%s\t%s\n' "$LOCAL_FP" "$LOCAL_GPUS"
      ;;
  esac
  exit 0
fi

# --- Run ----------------------------------------------------------------------------------------
run_check() { # id function
  want "$1" || return 0
  "$2"
}
if [ "$STAGE" = quick ]; then
  run_check H-LSM c_lsm
  run_check H-LANDLOCK c_landlock
  run_check H-SECCOMP c_seccomp
  run_check H-DOCKER-SECCOMP c_docker_seccomp
  run_check H-NAMES c_names
  run_check O-GATEWAY c_ogateway
else
  run_check H-SETTINGS c_settings
  run_check H-ARCH c_arch
  run_check H-OS c_os
  run_check H-GLIBC c_glibc
  run_check H-KERNEL c_kernel
  run_check H-LSM c_lsm
  run_check H-LANDLOCK c_landlock
  run_check H-SECCOMP c_seccomp
  run_check H-CGROUP c_cgroup
  run_check H-DOCKER c_docker
  run_check H-DOCKER-SECCOMP c_docker_seccomp
  run_check H-NVIDIA c_nvidia
  run_check H-EGRESS c_egress
  run_check H-PORTS c_ports
  run_check H-SUBNET c_subnet
  run_check H-NAMES c_names
  run_check H-METADATA c_metadata
  run_check H-EXPOSURE c_exposure
  run_check H-PRIVATE c_private # before H-DEVLOGIN, which reads its outcome
  run_check H-DEVLOGIN c_devlogin
  run_check H-SIGNIN c_signin
  run_check H-ADMIN c_admin
  run_check H-ASSERT c_assert
  run_check H-LOCAL c_local
  c_stray_acks
  if [ "$OPENSHELL" = 1 ]; then
    run_check O-CLI c_ocli
    run_check O-GATEWAY c_ogateway
    run_check O-LINGER c_olinger
    if want O-CANARY || want O-REACH; then c_canaries; fi
  fi
fi

failed=0
for i in "${!R_ID[@]}"; do [ "${R_ST[$i]}" != fail ] || failed=1; done

# What is in force (§3.2: status, the --json output and the acceptance record list each one).
in_force() { # check env-key → "<check>\t<time>\t<login>\t<reason>\t<saw>" when its line and log agree
  local v line
  v=$(ev "$2")
  [ -n "$v" ] || return 0
  line=$(log_line "$1")
  [ -n "$line" ] || return 0
  awk -F'\t' -v fp="$v" -v c="$1" '$6 == fp && ($4 == "acknowledge" || $4 == "attest") { printf "%s\t%s\t%s\t%s\t%s\n", c, $1, $2, $7, $5 }' <<<"$line"
}
ACKS=$(in_force H-PRIVATE PMOS_HOST_ACK_H_PRIVATE)
ATTS=$(in_force local-gpu PMOS_HOST_ATTEST_LOCAL_GPU)

if [ "$JSON" = 1 ]; then
  for i in "${!R_ID[@]}"; do
    printf '%s\t%s\t%s\t%s\t%s\n' "${R_ID[$i]}" "${R_ST[$i]}" "${R_MEAS[$i]//$'\t'/ }" "${R_EXP[$i]//$'\t'/ }" "${R_HINT[$i]//$'\t'/ }"
  done | ACKS=$ACKS ATTS=$ATTS STAGE=$STAGE MS=$MS EXPOSURE=$EXPOSURE python3 -I -c '
import json, os, sys
keys = ("id", "status", "measured", "expected", "hint")
checks = [dict(zip(keys, l.rstrip("\n").split("\t"))) for l in sys.stdin if l.strip()]
def recs(v, kind):
    out = []
    for l in v.splitlines():
        c, t, who, reason, saw = l.split("\t")
        out.append({"check": c, "kind": kind, "at": t, "by": who, "reason": reason, "saw": saw})
    return out
print(json.dumps({"stage": os.environ["STAGE"], "model_source": os.environ["MS"],
                  "exposure": os.environ["EXPOSURE"], "checks": checks,
                  "acknowledgements": recs(os.environ["ACKS"], "acknowledgement"),
                  "attestations": recs(os.environ["ATTS"], "attestation")}, indent=1))
'
else
  printf 'PM OS host preflight: stage %s, PMOS_MODEL_SOURCE=%s, PMOS_EXPOSURE=%s\n' "$STAGE" \
    "${MS_SET:-(blank; checked as $MS)}" "${EXPOSURE_SET:-(blank; checked as $EXPOSURE)}"
  for i in "${!R_ID[@]}"; do
    printf '  %-17s %-5s %s (expected: %s)\n' "${R_ID[$i]}" "${R_ST[$i]}" "${R_MEAS[$i]}" "${R_EXP[$i]}"
    [ -z "${R_HINT[$i]}" ] || [ "${R_ST[$i]}" = pass ] || printf '  %-17s       %s\n' "" "${R_HINT[$i]}"
  done
fi
exit "$failed"
