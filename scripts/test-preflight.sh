#!/usr/bin/env bash
# Checks deploy/preflight.sh (docs/openshell-any-host-spec.md §3.2 and §6, slice H1) without a
# real host: every check, pass and fail, on fixture hosts under deploy/test/preflight/ (read with
# --root), each asserting the exact status and exit code. CI's deploy job runs it. What OpenShell
# does is not shown here: the O-* checks run against a stand-in CLI (the fixture's bin/openshell),
# and are proven only in the acceptance run on a real host (§6).
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
PF="$REPO/deploy/preflight.sh"
FIX="$REPO/deploy/test/preflight"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/pmos-preflight-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
n_cases=0

bash -n "$PF"
bash -n "$REPO/deploy/pmos-metadata-block"
command -v python3 >/dev/null || fail "python3 is needed"

# A fixture host: the Ubuntu host with overlays on top. R is its directory.
R="$WORK/root"
host() { # [overlay…]
  rm -rf "$R"
  cp -a "$FIX/ubuntu-24.04-amd64" "$R"
  local o
  for o in "$@"; do cp -a "$FIX/$o/." "$R/"; done
}
ENVF="$WORK/env"
# Slice H4: the two host settings are required at the run stage (H-SETTINGS), so every fixture
# names them; a later line for the same key (an argument here) wins, as in deploy/.env.
env_lines() { printf '%s\n' "PMOS_VLLM_IMAGE=example.test/vllm:1" PMOS_MODEL_SOURCE=local PMOS_EXPOSURE=public "$@" >"$ENVF"; }
setcmd() { printf '%s\n' "$2" >"$R/cmd/$1"; } # id output
OUT="" CODE=0
pf() { # preflight arguments…
  CODE=0
  OUT=$("$PF" --root "$R" --env-file "$ENVF" --json "$@" 2>"$WORK/stderr") || CODE=$?
  n_cases=$((n_cases + 1))
}
field() { # id field → the value(s) for that check, one per line
  python3 -I -c '
import json, sys
d = json.loads(sys.stdin.read())
for c in d["checks"]:
    if c["id"] == sys.argv[1]: print(c[sys.argv[2]])
' "$1" "$2" <<<"$OUT"
}
expect() { # id status exit [label]
  local got
  [ "$CODE" != 2 ] || fail "${4:-$1}: the preflight couldn't run: $(cat "$WORK/stderr")"
  got=$(field "$1" status | sort -u | paste -sd, -)
  [ "$got" = "$2" ] || fail "${4:-$1}: $1 is '$got', not '$2' ($(field "$1" measured))"
  [ "$CODE" = "$3" ] || fail "${4:-$1}: exit $CODE, not $3: $(python3 -I -c 'import json,sys; print([(c["id"], c["status"]) for c in json.loads(sys.stdin.read())["checks"] if c["status"] == "fail"])' <<<"$OUT")"
}

# --- Arguments --------------------------------------------------------------------------------
host
env_lines
for args in "--stage bogus" "--model-source bogus" "--exposure bogus" "--bogus" "--root $WORK/missing" "--stage" \
  "--fingerprint bogus"; do
  code=0
  # shellcheck disable=SC2086
  "$PF" --root "$R" --env-file "$ENVF" $args >/dev/null 2>&1 || code=$?
  [ "$code" = 2 ] || fail "preflight $args exits $code, not 2"
done
# A deploy/.env line that isn't KEY=VALUE: the preflight can't run (2), and nothing in it runs.
printf 'touch %s/pwned\n' "$WORK" >"$ENVF"
code=0
"$PF" --root "$R" --env-file "$ENVF" >/dev/null 2>&1 || code=$?
[ "$code" = 2 ] && [ ! -e "$WORK/pwned" ] || fail "a bad deploy/.env line exits $code, or ran"
env_lines "PMOS_X=\$(touch $WORK/pwned2)"
pf
[ ! -e "$WORK/pwned2" ] || fail "a deploy/.env value was run as shell"

# --- The Ubuntu 24.04 amd64 host: every check passes ------------------------------------------
host
env_lines
pf
[ "$CODE" = 0 ] || fail "the Ubuntu host fails: $OUT"
for id in H-SETTINGS H-ARCH H-OS H-GLIBC H-KERNEL H-LSM H-LANDLOCK H-SECCOMP H-CGROUP H-DOCKER H-DOCKER-SECCOMP H-NVIDIA \
  H-PORTS H-SUBNET H-NAMES H-METADATA H-EXPOSURE H-DEVLOGIN H-SIGNIN H-ASSERT H-LOCAL; do
  expect "$id" pass 0 "ubuntu"
done
expect H-EGRESS skip 0
expect H-PRIVATE skip 0
expect H-ADMIN skip 0
# The JSON: stage, source and exposure, the five fields per check, no O-* without --openshell.
python3 -I -c '
import json, sys
d = json.loads(sys.stdin.read())
assert d["stage"] == "run" and d["model_source"] == "local" and d["exposure"] == "public", d
assert all(set(c) == {"id", "status", "measured", "expected", "hint"} for c in d["checks"])
assert not any(c["id"].startswith("O-") for c in d["checks"])
assert d["acknowledgements"] == [] and d["attestations"] == []
' <<<"$OUT" || fail "the JSON's shape: $OUT"
# The text form prints the same checks; exit 0.
txt=$("$PF" --root "$R" --env-file "$ENVF") || fail "the text form fails on the Ubuntu host"
[[ "$txt" == *"H-LANDLOCK"*"pass"*"ABI 6"* ]] || fail "the text form: $txt"
# Stage install: H-NAMES and H-METADATA "not set yet", and pass, even with no block and no probe.
rm -f "$R/cmd/metadata-probe"
printf '127.0.0.1 localhost\n' >"$R/etc/hosts"
pf --stage install
expect H-NAMES pass 0 "install"
expect H-METADATA pass 0 "install"
[ "$(field H-NAMES measured)" = "not set yet" ] && [ "$(field H-METADATA measured)" = "not set yet" ] ||
  fail "install doesn't say not set yet"
# Stage quick: only its six checks (O-GATEWAY included), and deploy/.env isn't read at all.
host
printf 'not a line of deploy/.env\n' >"$ENVF"
pf --stage quick
ids=$(python3 -I -c 'import json,sys; print(" ".join(c["id"] for c in json.loads(sys.stdin.read())["checks"]))' <<<"$OUT")
[ "$ids" = "H-LSM H-LANDLOCK H-SECCOMP H-DOCKER-SECCOMP H-NAMES O-GATEWAY" ] || fail "quick runs other checks: $ids"
[ "$CODE" = 0 ] || fail "quick fails on the Ubuntu host: $OUT"
env_lines

# --- A Debian arm64 host ------------------------------------------------------------------------
host debian-12-arm64
pf
[ "$CODE" = 0 ] || fail "the Debian arm64 host fails: $OUT"
expect H-ARCH pass 0 debian
expect H-OS pass 0 debian
[ "$(field H-ARCH measured)" = aarch64 ] || fail "debian isn't aarch64"

# --- The kernel and its security --------------------------------------------------------------
host kernel-5.15
pf
expect H-KERNEL fail 1 "5.15"
host no-landlock-lsm
pf
expect H-LSM fail 1 "landlock not in lsm"
host landlock-abi-2
pf
expect H-LANDLOCK fail 1 "ABI 2"
host
setcmd landlock-abi "error Operation not permitted"
echo 1 >"$R/cmd/landlock-abi.exit"
pf
expect H-LANDLOCK fail 1 "probe fails"
[[ "$(field H-LANDLOCK measured)" == "not measurable: error Operation not permitted" ]] || fail "the probe's failure isn't the reason"
host
rm "$R/sys/kernel/security/lsm"
pf
expect H-LSM fail 1 "no lsm file"
host
sed -i '/^Seccomp/d' "$R/proc/self/status"
pf
expect H-SECCOMP fail 1 "no Seccomp line"
host
mkdir -p "$R/boot"
printf 'CONFIG_SECCOMP=y\n# CONFIG_SECCOMP_FILTER is not set\n' >"$R/boot/config-6.8.0-49-generic"
pf
expect H-SECCOMP fail 1 "no CONFIG_SECCOMP_FILTER"
printf 'CONFIG_SECCOMP=y\nCONFIG_SECCOMP_FILTER=y\n' >"$R/boot/config-6.8.0-49-generic"
pf
expect H-SECCOMP pass 0 "CONFIG_SECCOMP_FILTER=y"
host
rm "$R/sys/fs/cgroup/cgroup.controllers"
pf
expect H-CGROUP fail 1 "cgroups v1"
host
setcmd uname-m riscv64
pf
expect H-ARCH fail 1 riscv64
host
setcmd getconf-glibc "glibc 2.27"
pf
expect H-GLIBC fail 1 "glibc 2.27"
host
printf 'ID=fedora\n' >"$R/etc/os-release"
pf
expect H-OS warn 0 fedora

# --- Docker -----------------------------------------------------------------------------------
host docker-27
pf
expect H-DOCKER fail 1 "Docker 27"
host
rm "$R/cmd/docker-server-version"
pf
expect H-DOCKER fail 1 "no docker"
host
echo 1 >"$R/cmd/docker-compose-version.exit"
pf
expect H-DOCKER fail 1 "no compose"
host
setcmd docker-security-options '["name=apparmor","name=seccomp,profile=unconfined"]'
pf
expect H-DOCKER-SECCOMP fail 1 "unconfined"
setcmd docker-security-options '["name=apparmor"]'
pf
expect H-DOCKER-SECCOMP fail 1 "no seccomp"

# --- H-NVIDIA: local only -----------------------------------------------------------------------
host no-nvidia
pf
expect H-NVIDIA fail 1 "local without nvidia"
env_lines "PMOS_GATEWAY_STUB=1"
pf
expect H-NVIDIA skip 0 "local with the stub model on (no model server starts)"
env_lines "PMOS_GATEWAY_STUB=0"
pf
expect H-NVIDIA fail 1 "local with the stub off, without nvidia"
env_lines
pf --model-source hosted-test
expect H-NVIDIA skip 0 "hosted-test without nvidia"
env_lines "PMOS_MODEL_SOURCE=hosted-test"
pf
expect H-NVIDIA skip 0 "hosted-test from deploy/.env"
host
printf 'PMOS_VLLM_IMAGE=\nPMOS_MODEL_SOURCE=local\nPMOS_EXPOSURE=public\n' >"$ENVF"
pf
expect H-NVIDIA fail 1 "local with PMOS_VLLM_IMAGE empty"
env_lines

# --- H-EGRESS: the gateway's own /readyz, hosted and hosted-test ---------------------------------
host
pf --model-source hosted-test --starting
expect H-EGRESS pass 0 "starting"
[ "$(field H-EGRESS measured)" = "not started" ] || fail "egress at start isn't 'not started'"
pf --model-source hosted-test --stage install
expect H-EGRESS pass 0 "install"
pf --model-source hosted-test
expect H-EGRESS pass 0 "no gateway yet"
setcmd gateway-readyz '{"class":"test","upstreams":{"default":{"status":"ok","upstream":"remote-test"},"extraction":{"status":"down","upstream":"remote-test"}}}'
pf --model-source hosted-test
expect H-EGRESS warn 0 "hosted-test down"
[[ "$(field H-EGRESS hint)" == *HTTPS_PROXY*NO_PROXY* ]] || fail "egress doesn't name the proxy settings"
pf --model-source hosted
expect H-EGRESS fail 1 "hosted down"
setcmd gateway-readyz '{"class":"test","upstreams":{"default":{"status":"ok","upstream":"remote-test"}}}'
pf --model-source hosted-test
expect H-EGRESS pass 0 "hosted-test ok"
pf
expect H-EGRESS skip 0 "local"
# The preflight never sends the key: it reads /readyz on loopback only (the fixture shows the
# command it would run is that one URL; on a host it is curl to 127.0.0.1 and the gateway port).
! grep -n 'REMOTE_KEY\|HOSTED_KEY\|hosted-model.key' "$PF" | grep -v '^[0-9]*: *#' || fail "the preflight reads a model key"

# --- H-PORTS, from the configured ports -------------------------------------------------------
host
printf 'LISTEN 0 4096 127.0.0.1:8710 0.0.0.0:*\n' >>"$R/cmd/ss"
pf
expect H-PORTS fail 1 "8710 taken"
setcmd docker-ps-ports '127.0.0.1:8710->8710/tcp'
pf
expect H-PORTS pass 0 "8710 is this stack's"
setcmd docker-ps-ports ''
env_lines PMOS_TOOLSERVER_PORT=18710
pf
expect H-PORTS pass 0 "8710 taken, the stack on 18710"
printf 'LISTEN 0 4096 127.0.0.1:18710 0.0.0.0:*\n' >>"$R/cmd/ss"
pf
expect H-PORTS fail 1 "18710 taken"
for bad in 0 70000 87x0; do
  env_lines "PMOS_BOTS_PORT=$bad"
  pf
  expect H-PORTS fail 1 "PMOS_BOTS_PORT=$bad"
done
env_lines PMOS_BOTS_PORT=8740
pf
expect H-PORTS fail 1 "two services on one port"
env_lines

# --- H-EXPOSURE: PM OS ports on loopback only, on every host ------------------------------------
for exposure in public private; do
  host
  printf 'LISTEN 0 4096 0.0.0.0:8710 0.0.0.0:*\n' >>"$R/cmd/ss"
  pf --exposure "$exposure"
  expect H-EXPOSURE fail 1 "8710 on 0.0.0.0, $exposure"
  host
  printf 'LISTEN 0 4096 10.0.0.5:8770 0.0.0.0:*\n' >>"$R/cmd/ss"
  pf --exposure "$exposure"
  expect H-EXPOSURE fail 1 "8770 on the host's address, $exposure"
done
host
env_lines PMOS_WEB_PORT=18770
printf 'LISTEN 0 4096 [::]:18770 [::]:*\n' >>"$R/cmd/ss"
pf
expect H-EXPOSURE fail 1 "the configured web port on [::]"
env_lines
host
printf 'LISTEN 0 4096 *:8772 *:*\n' >>"$R/cmd/ss"
pf
expect H-EXPOSURE fail 1 "8772 on every address"
# Slice H10: 8772 only on PMOS_WEB_ADMIN_PUBLISH (default 127.0.0.1).
host
printf 'LISTEN 0 4096 127.0.0.1:8772 0.0.0.0:*\n' >>"$R/cmd/ss"
pf
expect H-EXPOSURE pass 0 "8772 on the default loopback publish"
env_lines PMOS_WEB_ADMIN_PUBLISH=10.0.0.5
pf
expect H-EXPOSURE fail 1 "8772 on loopback while the publish is 10.0.0.5"
host
printf 'LISTEN 0 4096 10.0.0.5:8772 0.0.0.0:*\n' >>"$R/cmd/ss"
pf
expect H-EXPOSURE pass 0 "8772 on its private publish"
env_lines
pf
expect H-EXPOSURE fail 1 "8772 on a private address that isn't the publish"
# 8771 (the web app's internal listener, H9) is never published: a host listener on it fails on
# every address, loopback included, and on every host (the rule is the same everywhere).
for exposure in public private; do
  for l in '0.0.0.0:8771' '127.0.0.1:8771' '10.0.0.5:8771' '[::]:8771' '[::1]:8771' '*:8771'; do
    host
    printf 'LISTEN 0 4096 %s 0.0.0.0:*\n' "$l" >>"$R/cmd/ss"
    pf --exposure "$exposure"
    expect H-EXPOSURE fail 1 "8771 on $l, $exposure"
  done
done
host
printf 'LISTEN 0 4096 127.0.0.1:8770 0.0.0.0:*\nLISTEN 0 4096 127.0.0.1:8771 0.0.0.0:*\n' >>"$R/cmd/ss"
pf
expect H-EXPOSURE fail 1 "8771 beside a published 8770"
host
printf 'LISTEN 0 4096 172.17.0.1:17670 0.0.0.0:*\n' >>"$R/cmd/ss"
pf
expect H-EXPOSURE pass 0 "17670 on docker0's address"
printf 'LISTEN 0 4096 0.0.0.0:17670 0.0.0.0:*\n' >>"$R/cmd/ss"
pf
expect H-EXPOSURE fail 1 "17670 on 0.0.0.0"
host
printf 'LISTEN 0 4096 127.0.0.1:8710 0.0.0.0:*\nLISTEN 0 4096 [::1]:8740 [::]:*\n' >>"$R/cmd/ss"
setcmd docker-ps-ports '127.0.0.1:8710->8710/tcp, 127.0.0.1:8740->8740/tcp'
pf
expect H-EXPOSURE pass 0 "loopback listeners"

# --- H-SUBNET: H2's pinned network, the routes, the live network, the trusted proxies -----------
host
for bad in PMOS_NETWORK_SUBNET=10.0.0.0/8 PMOS_NETWORK_SUBNET=10.201.0.0/16 PMOS_NETWORK_IP_RANGE=10.201.77.0/28 \
  PMOS_NETWORK_IP_RANGE=10.201.77.12/30 PMOS_NETWORK_GATEWAY=10.201.77.11 PMOS_NETWORK_SUBNET=10.201.78.0/24; do
  env_lines "$bad"
  pf
  expect H-SUBNET fail 1 "$bad"
done
# A subnet without the services' addresses is refused as the policy's constants (slice H3).
for case_ in "PMOS_NETWORK_SUBNET=10.201.78.0/24|allows toolserver.pmos.internal (allowed_ips 10.201.77.10/32" \
  "PMOS_NETWORK_SUBNET=10.201.77.8/30 PMOS_NETWORK_GATEWAY=10.201.77.9 PMOS_NETWORK_IP_RANGE=10.201.77.8/31|allows gateway.pmos.internal (allowed_ips 10.201.77.11/32"; do
  # shellcheck disable=SC2086 # the words are deploy/.env lines
  env_lines ${case_%%|*}
  pf
  expect H-SUBNET fail 1 "${case_%%|*}"
  [[ "$(field H-SUBNET measured)" == *"${case_#*|}"* ]] || fail "${case_%%|*}: H-SUBNET doesn't say '${case_#*|}': $(field H-SUBNET measured)"
done
env_lines PMOS_NETWORK_SUBNET=10.201.77.0/25 PMOS_NETWORK_GATEWAY=10.201.77.126 PMOS_NETWORK_IP_RANGE=10.201.77.64/26
pf
expect H-SUBNET pass 0 "a /25 that holds the pinned addresses"
env_lines
printf '10.201.77.0/24 dev eth1 proto kernel scope link\n' >>"$R/cmd/ip-route"
pf
expect H-SUBNET fail 1 "a VPC route overlapping the subnet"
host
printf '10.201.0.0/16 via 10.0.0.1 dev ens4\n' >>"$R/cmd/ip-route"
pf
expect H-SUBNET fail 1 "a wider route holding the subnet"
host
printf '10.201.77.0/24 dev pmos0 proto kernel scope link src 10.201.77.1\n' >>"$R/cmd/ip-route"
pf
expect H-SUBNET pass 0 "Docker's own pmos0 route"
setcmd docker-network-inspect '10.201.77.0/24 10.201.77.1 10.201.77.128/25;pmos0'
pf
expect H-SUBNET pass 0 "the live network as pinned"
setcmd docker-network-inspect '10.201.77.0/24 10.201.77.1 invalid Prefix;'
pf
expect H-SUBNET fail 1 "a live network made before H2"
setcmd docker-network-inspect '10.201.77.0/24 10.201.77.1 10.201.77.128/25;br-0123'
pf
expect H-SUBNET fail 1 "a live network with Docker's bridge name"
pf --stage install
expect H-SUBNET pass 0 "install doesn't compare the live network"
host
for tp in 10.201.77.10 "10.201.77.1, 10.201.77.13"; do
  env_lines "PMOS_WEB_TRUSTED_PROXIES=$tp"
  pf
  expect H-SUBNET fail 1 "trusted proxy $tp"
done
env_lines PMOS_WEB_TRUSTED_PROXIES=10.201.77.1
pf
expect H-SUBNET pass 0 "trusted proxy the gateway"
env_lines

# --- H-NAMES: the host's view of H2's managed block ---------------------------------------------
host
sed -i 's/^10.201.77.11 /10.201.77.99 /' "$R/etc/hosts"
pf
expect H-NAMES fail 1 "a stale block"
host
printf '127.0.0.1 toolserver.pmos.internal\n' >>"$R/etc/hosts"
pf
expect H-NAMES fail 1 "a loopback line outside the block"
[[ "$(field H-NAMES measured)" == *"loopback"* ]] || fail "the loopback line isn't named"
host
setcmd getent-gateway.pmos.internal "10.1.2.3        STREAM gateway.pmos.internal"
pf
expect H-NAMES fail 1 "a resolver answering elsewhere"
host
rm "$R/cmd/getent-bots.pmos.internal"
pf
expect H-NAMES fail 1 "a name that doesn't resolve"
host
printf '# BEGIN pmos (managed by pmos-admin)\n' >>"$R/etc/hosts"
pf
expect H-NAMES fail 1 "broken markers"

# --- H-METADATA: each answer, on public and private, with and without the block ------------------
for exposure in public private; do
  for ans in none gcp-nosa aws-hop1; do
    host
    setcmd metadata-probe "$ans"
    pf --exposure "$exposure"
    expect H-METADATA pass "$([ "$exposure" = private ] && [ "$ans" != none ] && echo 1 || echo 0)" "$ans, $exposure"
  done
  for ans in gcp-sa aws-open imdsv1 azure other-404; do
    host
    setcmd metadata-probe "$ans"
    pf --exposure "$exposure"
    expect H-METADATA fail 1 "$ans, $exposure"
  done
done
host
setcmd metadata-probe azure
pf
[[ "$(field H-METADATA hint)" == *"host-firewall apply"* ]] || fail "the azure answer doesn't name the block"
setcmd metadata-probe aws-open
pf
[[ "$(field H-METADATA hint)" == *"IMDSv2 required with hop limit 1"* ]] || fail "the AWS answer doesn't name IMDSv2"
setcmd metadata-probe gcp-sa
pf
[[ "$(field H-METADATA hint)" == *"service account"* ]] || fail "the GCP answer doesn't name the service account"
host
rm "$R/cmd/metadata-probe"
pf
expect H-METADATA fail 1 "the probe can't run"
host
setcmd metadata-probe "garbage answer"
pf
expect H-METADATA fail 1 "an unreadable probe answer"
# The probe sends only the three requests §3.2 allows, and never asks for a credential or a token.
probe=$(sed -n "/^METADATA_PROBE='/,/^esac'$/p" "$PF")
[ "$(grep -o 'curl ' <<<"$probe" | wc -l)" = 3 ] || fail "the probe sends other than three requests"
# (Its one token is AWS's one-second IMDSv2 session token, /latest/api/token, never a credential.)
! sed 's|/latest/api/token||; s|X-aws-ec2-metadata-token-ttl-seconds||' <<<"$probe" | grep -qiE 'security-credentials|token|oauth2|identity' || fail "the probe asks for a credential"
grep -q -- '-o /dev/null -m 2 -X PUT' <<<"$probe" || fail "the AWS token PUT's body isn't discarded"
# The block in place (apply was run): its rules (as root: the script's check; otherwise the
# timer's recent status), the drop-in and the timer; each missing one fails, on every host.
block_host() {
  host
  mkdir -p "$R/etc/pmos" "$R/etc/systemd/system/docker.service.d" "$R/run/pmos" "$R/usr/local/sbin"
  echo SUBNET=10.201.77.0/24 >"$R/etc/pmos/metadata-block.conf"
  printf '[Service]\nExecStartPost=-/usr/local/sbin/pmos-metadata-block apply\n' >"$R/etc/systemd/system/docker.service.d/pmos-metadata-block.conf"
  echo "ok $(($(cat "$R/cmd/now") - 30))" >"$R/run/pmos/metadata-block.status"
  setcmd timer-enabled enabled
  setcmd timer-active active
}
for exposure in public private; do
  block_host
  pf --exposure "$exposure"
  expect H-METADATA pass 0 "the block in place, $exposure"
  rm "$R/etc/systemd/system/docker.service.d/pmos-metadata-block.conf"
  pf --exposure "$exposure"
  expect H-METADATA fail 1 "the drop-in missing, $exposure"
  block_host
  setcmd timer-enabled disabled
  pf --exposure "$exposure"
  expect H-METADATA fail 1 "the timer disabled, $exposure"
  block_host
  setcmd timer-active inactive
  pf --exposure "$exposure"
  expect H-METADATA fail 1 "the timer inactive, $exposure"
  block_host
  echo "ok $(($(cat "$R/cmd/now") - 600))" >"$R/run/pmos/metadata-block.status"
  pf --exposure "$exposure"
  expect H-METADATA fail 1 "the rules not confirmed for 10 minutes, $exposure"
  block_host
  rm "$R/run/pmos/metadata-block.status"
  pf --exposure "$exposure"
  expect H-METADATA fail 1 "no status, $exposure"
done
block_host
setcmd id-u 0
setcmd metadata-block-check ""
pf
expect H-METADATA pass 0 "root: the rules checked"
echo 1 >"$R/cmd/metadata-block-check.exit"
pf
expect H-METADATA fail 1 "root: a rule missing"

# --- H-DEVLOGIN, H-SIGNIN, H-ADMIN, H-ASSERT ---------------------------------------------------
host
env_lines PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=1
pf
expect H-DEVLOGIN fail 1 "dev sign-in on public"
pf --exposure private
expect H-DEVLOGIN pass 0 "dev sign-in on a host that measures private"
# Slice H10: H-SIGNIN as decision D2 (a) says, from the settings.
ADMIN_OK=PMOS_WEB_ADMIN_URL=http://pmos-test.localhost:8772
env_lines "$ADMIN_OK"
pf --web
expect H-SIGNIN pass 0 "the web app on public, Microsoft sign-in only"
expect H-ADMIN pass 0 "the tunnel mode on the loopback publish"
env_lines "$ADMIN_OK" PMOS_WEB_PASSWORD_LOGIN=1
pf --web
expect H-SIGNIN fail 1 "public with password sign-in and no allow-list"
[[ "$(field H-SIGNIN hint)" == *"set PMOS_WEB_PASSWORD_CLIENTS"*"decision D2"* ]] || fail "H-SIGNIN's message: $(field H-SIGNIN hint)"
for bad in 0.0.0.0/0 ::/0 ::ffff:0:0/96 10.0.0.0/15 2001:db8::/47 10.1.0.1/16 10.1.0.0 "10.1.0.0/16, 0.0.0.0/0"; do
  env_lines "$ADMIN_OK" PMOS_WEB_PASSWORD_LOGIN=1 "PMOS_WEB_PASSWORD_CLIENTS=$bad"
  pf --web
  expect H-SIGNIN fail 1 "PMOS_WEB_PASSWORD_CLIENTS=$bad on public"
  pf --web --exposure private
  expect H-SIGNIN fail 1 "PMOS_WEB_PASSWORD_CLIENTS=$bad on private"
done
for good in 10.1.0.0/16 2001:db8:1::/48 "10.1.0.0/16, 192.168.7.0/24, 2001:db8:1:2::/64"; do
  env_lines "$ADMIN_OK" PMOS_WEB_PASSWORD_LOGIN=1 "PMOS_WEB_PASSWORD_CLIENTS=$good"
  pf --web
  expect H-SIGNIN pass 0 "PMOS_WEB_PASSWORD_CLIENTS=$good on public"
done
env_lines "$ADMIN_OK" PMOS_WEB_PASSWORD_LOGIN=1
pf --web --exposure private
expect H-SIGNIN pass 0 "the web app on private: the allow-list optional"
setcmd docker-ps-services $'postgres\nweb\ntoolserver'
pf
expect H-SIGNIN fail 1 "a running web container on public with password sign-in and no allow-list"
host
# H-ADMIN, every host (decision D3 (b); r7, X6).
for case in "PMOS_WEB_ADMIN_URL=|PMOS_WEB_ADMIN_URL is blank" \
  "PMOS_WEB_ADMIN_PUBLISH=203.0.113.7|outside loopback" \
  "PMOS_WEB_ADMIN_PUBLISH=0.0.0.0|outside loopback" \
  "PMOS_WEB_ADMIN_PUBLISH=169.254.1.1|outside loopback" \
  "PMOS_WEB_ADMIN_PUBLISH=10.0.0.5|which isn't loopback" \
  "PMOS_WEB_ADMIN_TRUSTED_PROXIES=10.201.77.1|http admin URL with PMOS_WEB_ADMIN_TRUSTED_PROXIES" \
  "PMOS_WEB_ADMIN_URL=http://pmos-admin.corp.example:8772|isn't a loopback name" \
  "PMOS_WEB_ADMIN_URL=https://pmos-admin.corp.example|without PMOS_WEB_ADMIN_TRUSTED_PROXIES"; do
  env_lines "$ADMIN_OK" "${case%%|*}"
  pf --web
  expect H-ADMIN fail 1 "H-ADMIN: ${case%%|*}"
  [[ "$(field H-ADMIN measured)" == *"${case#*|}"* ]] || fail "H-ADMIN with ${case%%|*} says: $(field H-ADMIN measured)"
done
env_lines PMOS_WEB_ADMIN_URL=https://pmos-admin.corp.example PMOS_WEB_ADMIN_TRUSTED_PROXIES=10.201.77.1 PMOS_WEB_ADMIN_PUBLISH=10.0.0.5
pf --web
expect H-ADMIN pass 0 "the proxy mode on a private publish"
for bare in http://localhost:8772 http://127.0.0.1:8772; do
  env_lines "PMOS_WEB_ADMIN_URL=$bare"
  pf --web
  expect H-ADMIN warn 0 "a bare $bare"
done
pf
expect H-ADMIN skip 0 "the web app off"
# A reverse proxy on this host that forwards a public listener to the admin door (r7): nginx -T
# and caddy adapt, as each prints its running configuration.
nginx_conf() { # listen-line… → an nginx -T output whose server includes the proxy file
  {
    echo "nginx: the configuration file /etc/nginx/nginx.conf syntax is ok"
    echo "# configuration file /etc/nginx/nginx.conf:"
    echo "http {"
    echo "  server {"
    local l
    for l in "$@"; do echo "    listen $l;"; done
    echo "    server_name pmos-admin.corp.example;"
    echo "    location / { include /etc/nginx/pmos-admin-proxy.conf; }"
    echo "  }"
    echo "  server { listen 443 ssl; location / { proxy_pass http://127.0.0.1:8770; } }"
    echo "}"
    echo "# configuration file /etc/nginx/pmos-admin-proxy.conf:"
    echo "proxy_pass http://10.0.0.5:8772;"
    echo "proxy_set_header Host \$host;"
  } >"$R/cmd/nginx-T"
}
PROXY_ENV=(PMOS_WEB_ADMIN_URL=https://pmos-admin.corp.example PMOS_WEB_ADMIN_TRUSTED_PROXIES=10.201.77.1 PMOS_WEB_ADMIN_PUBLISH=10.0.0.5)
# On a host with a public address, every unspecified listen counts as public.
for listen in "443 ssl" "0.0.0.0:443 ssl" "203.0.113.7:443 ssl" "[::]:443 ssl" "pmos.example.com:443"; do
  host
  setcmd ip-addr '[{"ifname":"eth0","addr_info":[{"local":"203.0.113.7"},{"local":"10.0.0.5"}]}]'
  nginx_conf "10.0.0.5:443 ssl" "$listen"
  env_lines "${PROXY_ENV[@]}"
  pf --web
  expect H-ADMIN fail 1 "nginx forwarding to the admin door on $listen"
  [[ "$(field H-ADMIN measured)" == *"an nginx server forwarding to 10.0.0.5:8772 listens on"* ]] || fail "H-ADMIN doesn't name the nginx server: $(field H-ADMIN measured)"
done
host
nginx_conf "10.0.0.5:443 ssl" "127.0.0.1:8443"
env_lines "${PROXY_ENV[@]}"
pf --web
expect H-ADMIN pass 0 "nginx forwarding to the admin door on private addresses only"
# The unspecified address fails only on a host with a public address; the fixture host has none.
host
nginx_conf "443 ssl"
setcmd ip-addr '[{"ifname":"lo","addr_info":[{"local":"127.0.0.1"}]},{"ifname":"eth0","addr_info":[{"local":"10.0.0.5"}]}]'
env_lines "${PROXY_ENV[@]}"
pf --web
expect H-ADMIN pass 0 "nginx on the unspecified address of a host with no public address"
setcmd ip-addr '[{"ifname":"eth0","addr_info":[{"local":"203.0.113.7"},{"local":"10.0.0.5"}]}]'
pf --web
expect H-ADMIN fail 1 "nginx on the unspecified address of a host with a public address"
host
setcmd caddy-adapt '{"apps":{"http":{"servers":{"srv0":{"listen":[":443"],"routes":[{"handle":[{"handler":"reverse_proxy","upstreams":[{"dial": "10.0.0.5:8772"}]}]}]}}}}}'
setcmd ip-addr '[{"ifname":"eth0","addr_info":[{"local":"203.0.113.7"}]}]'
env_lines "${PROXY_ENV[@]}"
pf --web
expect H-ADMIN fail 1 "a Caddy site forwarding to the admin door on :443 of a public host"
[[ "$(field H-ADMIN measured)" == *"a Caddy site (srv0)"* ]] || fail "H-ADMIN doesn't name the Caddy site"
setcmd caddy-adapt '{"apps":{"http":{"servers":{"srv0":{"listen":["10.0.0.5:443"],"routes":[{"handle":[{"handler":"reverse_proxy","upstreams":[{"dial": "10.0.0.5:8772"}]}]}]}}}}}'
pf --web
expect H-ADMIN pass 0 "a Caddy site forwarding to the admin door on a private address"
# The kit's own examples (deploy/nginx/) as nginx -T prints them, on a host with a public
# address: the admin door's terminator listens on its private address only, so H-ADMIN passes;
# the same server on `listen 443` fails.
host
setcmd ip-addr '[{"ifname":"eth0","addr_info":[{"local":"203.0.113.7"},{"local":"10.0.0.5"}]}]'
for f in pmos-public.conf.example pmos-admin.conf.example pmos-proxy.conf; do
  printf '# configuration file /etc/nginx/%s:\n' "$f"
  cat "$REPO/deploy/nginx/$f"
done >"$R/cmd/nginx-T"
env_lines "${PROXY_ENV[@]}"
pf --web
expect H-ADMIN pass 0 "the kit's nginx examples"
sed -i 's|listen 10.0.0.5:443 ssl;|listen 443 ssl;|' "$R/cmd/nginx-T"
pf --web
expect H-ADMIN fail 1 "the kit's admin example on listen 443"
host
for line in "PMOS_ASSERTION_MODE=verify" "PMOS_ASSERTION_MODE=off"; do
  env_lines "$line" PMOS_RT_WEB_TOKEN=rt-web-0123456789abcdef
  pf
  expect H-ASSERT fail 1 "$line with a web token"
done
env_lines PMOS_ASSERTION_MODE=bogus
pf
expect H-ASSERT fail 1 "a bogus mode"
env_lines PMOS_RT_WEB_TOKEN=rt-web-0123456789abcdef
pf
expect H-ASSERT pass 0 "a blank mode is require with a token (as assertion_checks)"
env_lines PMOS_ASSERTION_MODE=off PMOS_RT_WEB_TOKEN=generated
pf
expect H-ASSERT pass 0 "off with no real token"
env_lines

# --- H-PRIVATE, its acknowledgement and the 80/443 check ----------------------------------------
LOG_REL=/var/log/pmos/host-acknowledgements.log
ack() { # fingerprint [reason] → the env line and the log line, as pmos-admin host acknowledge writes them
  mkdir -p "$R/var/log/pmos"
  printf '2026-11-23T10:00:00Z\tana\tH-PRIVATE\tacknowledge\tpublic addresses: x\t%s\t%s\n' "$1" "${2:-closed test}" >>"$R$LOG_REL"
}
host
pf --exposure private
expect H-PRIVATE pass 0 "a private host"
# A public interface address, then a metadata answer: each fails without an acknowledgement.
public_addr() {
  python3 -I -c '
import json, sys
d = json.load(open(sys.argv[1]))
d[1]["addr_info"].append({"family": "inet", "local": "203.0.113.7", "prefixlen": 24})
json.dump(d, open(sys.argv[1], "w"))
' "$R/cmd/ip-addr"
}
host
public_addr
pf --exposure private
expect H-PRIVATE fail 1 "a public address"
[[ "$(field H-PRIVATE measured)" == *203.0.113.7* ]] || fail "the public address isn't named"
host
setcmd metadata-probe gcp-nosa
pf --exposure private
expect H-PRIVATE fail 1 "a metadata answer"
# The fingerprint, as pmos-admin takes it: the preflight's own --fingerprint.
host
public_addr
fpl=$("$PF" --root "$R" --env-file "$ENVF" --exposure private --fingerprint h-private) || fail "--fingerprint h-private failed"
fp=${fpl%%$'\t'*}
[[ "$fp" =~ ^[0-9a-f]{64}$ ]] && [[ "$fpl" == *"public addresses: 203.0.113.7; metadata probe answered: no" ]] ||
  fail "--fingerprint h-private: $fpl"
# … with the acknowledgement: warn (exit 0), and the JSON lists it.
env_lines PMOS_EXPOSURE=private "PMOS_HOST_ACK_H_PRIVATE=$fp"
ack "$fp" "closed test, inbound blocked at the security group"
pf
expect H-PRIVATE warn 0 "acknowledged"
python3 -I -c '
import json, sys
a = json.loads(sys.stdin.read())["acknowledgements"]
assert len(a) == 1 and a[0]["check"] == "H-PRIVATE" and a[0]["by"] == "ana" and a[0]["reason"].startswith("closed test"), a
' <<<"$OUT" || fail "the JSON doesn't list the acknowledgement: $OUT"
# … but H-DEVLOGIN then applies at public strength.
env_lines PMOS_EXPOSURE=private "PMOS_HOST_ACK_H_PRIVATE=$fp" PMOS_WEB_ENABLE_DEV_LOGIN_CLOSED_TEST_NETWORK_ONLY=1
pf
expect H-DEVLOGIN fail 1 "dev sign-in on an acknowledged private host"
env_lines PMOS_EXPOSURE=private "PMOS_HOST_ACK_H_PRIVATE=$fp"
# Listeners on 80 or 443: on a public or unspecified address (the host has a public one) fail;
# on a private address pass.
for l in "0.0.0.0:443" "203.0.113.7:80" "[::]:443" "*:80"; do
  cp "$FIX/ubuntu-24.04-amd64/cmd/ss" "$R/cmd/ss"
  printf 'LISTEN 0 511 %s 0.0.0.0:*\n' "$l" >>"$R/cmd/ss"
  pf
  expect H-PRIVATE fail 1 "acknowledged, a listener on $l"
  [[ "$(field H-PRIVATE measured)" == *"listening on"* ]] || fail "the listener on $l isn't named"
done
cp "$FIX/ubuntu-24.04-amd64/cmd/ss" "$R/cmd/ss"
printf 'LISTEN 0 511 10.0.0.5:443 0.0.0.0:*\n' >>"$R/cmd/ss"
pf
expect H-PRIVATE warn 0 "acknowledged, a listener on a private address"
# A new public address: the fingerprint no longer matches.
python3 -I -c '
import json, sys
d = json.load(open(sys.argv[1]))
d[1]["addr_info"].append({"family": "inet", "local": "198.51.100.9", "prefixlen": 24})
json.dump(d, open(sys.argv[1], "w"))
' "$R/cmd/ip-addr"
pf
expect H-PRIVATE fail 1 "an acknowledgement whose fingerprint no longer matches"
# A deploy/.env line with no log entry, or whose latest log line is a clear: fails.
host
public_addr
env_lines PMOS_EXPOSURE=private "PMOS_HOST_ACK_H_PRIVATE=$fp"
pf
expect H-PRIVATE fail 1 "an acknowledgement with no log entry"
ack "$fp"
printf '2026-11-23T11:00:00Z\tana\tH-PRIVATE\tclear\t-\t-\tcleared\n' >>"$R$LOG_REL"
pf
expect H-PRIVATE fail 1 "an acknowledgement cleared in the log"
# A metadata answer, acknowledged: warn; the probe that no longer answers changes the fingerprint.
host
setcmd metadata-probe gcp-nosa
fpl=$("$PF" --root "$R" --env-file "$ENVF" --exposure private --fingerprint h-private)
fp2=${fpl%%$'\t'*}
env_lines PMOS_EXPOSURE=private "PMOS_HOST_ACK_H_PRIVATE=$fp2"
ack "$fp2"
pf
expect H-PRIVATE warn 0 "a metadata answer, acknowledged"
# No public address, so 0.0.0.0:443 is allowed (only a listener reachable from outside fails).
printf 'LISTEN 0 511 0.0.0.0:443 0.0.0.0:*\n' >>"$R/cmd/ss"
pf
expect H-PRIVATE warn 0 "0.0.0.0:443 on a host with no public address"
# At install (no probe yet), only the addresses are compared.
rm "$R/cmd/metadata-probe"
pf --stage install
expect H-PRIVATE warn 0 "acknowledged, at install"
# A host that looks private has nothing to acknowledge.
host
code=0
"$PF" --root "$R" --env-file "$ENVF" --exposure private --fingerprint h-private >/dev/null 2>&1 || code=$?
[ "$code" = 1 ] || fail "--fingerprint h-private on a private host exits $code, not 1"
# Only H-PRIVATE takes an acknowledgement, only local-gpu an attestation: any other line fails (P-8).
host
for line in PMOS_HOST_ACK_H_EXPOSURE=abc PMOS_HOST_ACK_H_METADATA=abc PMOS_HOST_ATTEST_OTHER=abc PMOS_HOST_ACK_X=1; do
  env_lines "$line"
  pf
  [ "$CODE" = 1 ] || fail "a hand-written $line doesn't fail the preflight ($CODE)"
  python3 -I -c '
import json, sys
d = json.loads(sys.stdin.read())
assert any(c["status"] == "fail" and sys.argv[1] in c["measured"] for c in d["checks"]), d
' "${line%%=*}" <<<"$OUT" || fail "$line isn't named in a fail"
done
env_lines

# --- H-LOCAL: real data under local rests on a recorded attestation ------------------------------
host
setcmd nvidia-smi-L $'GPU 0: NVIDIA L4 (UUID: GPU-11111111-2222-3333-4444-555555555555)\nGPU 1: NVIDIA L4 (UUID: GPU-66666666-7777-8888-9999-000000000000)'
for live in PMOS_CONNECTORS=live PMOS_MEMORY=github; do
  env_lines "$live"
  pf
  expect H-LOCAL fail 1 "local, $live, no attestation"
done
fpl=$("$PF" --root "$R" --env-file "$ENVF" --fingerprint local-gpu) || fail "--fingerprint local-gpu failed"
lfp=${fpl%%$'\t'*}
[[ "$lfp" =~ ^[0-9a-f]{64}$ ]] && [[ "$fpl" == *$'\t2x NVIDIA L4' ]] || fail "--fingerprint local-gpu: $fpl"
env_lines PMOS_CONNECTORS=live "PMOS_HOST_ATTEST_LOCAL_GPU=$lfp"
pf
expect H-LOCAL fail 1 "an attestation with no log line"
mkdir -p "$R/var/log/pmos"
printf '2026-11-23T10:00:00Z\tana\tlocal-gpu\tattest\t2x NVIDIA L4\t%s\tthe owner'"'"'s rack\n' "$lfp" >>"$R$LOG_REL"
pf
expect H-LOCAL pass 0 "a matching attestation"
python3 -I -c '
import json, sys
a = json.loads(sys.stdin.read())["attestations"]
assert len(a) == 1 and a[0]["check"] == "local-gpu" and a[0]["saw"] == "2x NVIDIA L4", a
' <<<"$OUT" || fail "the JSON doesn't list the attestation: $OUT"
printf 'fedcba9876543210fedcba9876543210\n' >"$R/etc/machine-id"
pf
expect H-LOCAL fail 1 "another machine id"
cp "$FIX/ubuntu-24.04-amd64/etc/machine-id" "$R/etc/machine-id"
setcmd nvidia-smi-L 'GPU 0: NVIDIA L4 (UUID: GPU-11111111-2222-3333-4444-555555555555)'
pf
expect H-LOCAL fail 1 "another set of GPUs"
rm "$R/cmd/nvidia-smi-L"
pf
expect H-LOCAL fail 1 "no nvidia-smi"
env_lines PMOS_CONNECTORS=live PMOS_MODEL_SOURCE=hosted-test
pf
expect H-LOCAL skip 0 "hosted-test isn't checked"
env_lines PMOS_CONNECTORS=fake
pf
expect H-LOCAL pass 0 "local with fake connectors"
env_lines

# --- H-SETTINGS: a blank source or exposure (slice H4) -------------------------------------------
# At the run stage a blank PMOS_MODEL_SOURCE or PMOS_EXPOSURE fails, naming the setting and the
# values; the checks that depend on it still run the stricter way (local: H-NVIDIA; public).
host
printf 'PMOS_VLLM_IMAGE=example.test/vllm:1\nPMOS_EXPOSURE=public\n' >"$ENVF"
pf
expect H-SETTINGS fail 1 "a blank source at run"
[[ "$(field H-SETTINGS measured)" == *"PMOS_MODEL_SOURCE is blank (use local or hosted-test"* ]] || fail "H-SETTINGS doesn't name the source: $(field H-SETTINGS measured)"
expect H-NVIDIA pass 1 "a blank source is checked as local"
python3 -I -c 'import json,sys; d=json.loads(sys.stdin.read()); assert d["model_source"] == "local", d' <<<"$OUT" || fail "a blank source isn't read as local for the checks"
txt=$("$PF" --root "$R" --env-file "$ENVF" 2>/dev/null) || true
[[ "$txt" == *"PMOS_MODEL_SOURCE=(blank; checked as local)"* ]] || fail "the text form hides a blank source: $txt"
printf 'PMOS_VLLM_IMAGE=example.test/vllm:1\nPMOS_MODEL_SOURCE=local\n' >"$ENVF"
pf
expect H-SETTINGS fail 1 "a blank exposure at run"
[[ "$(field H-SETTINGS measured)" == *"PMOS_EXPOSURE is blank (use public or private)"* ]] || fail "H-SETTINGS doesn't name the exposure"
printf 'PMOS_VLLM_IMAGE=example.test/vllm:1\nPMOS_MODEL_SOURCE=local\nPMOS_WEB_PASSWORD_LOGIN=1\n' >"$ENVF"
pf --web
expect H-SIGNIN fail 1 "a blank exposure is checked as public (password sign-in, no allow-list)"
printf 'PMOS_VLLM_IMAGE=example.test/vllm:1\n' >"$ENVF"
pf
expect H-SETTINGS fail 1 "both blank at run"
[[ "$(field H-SETTINGS measured)" == *PMOS_MODEL_SOURCE*PMOS_EXPOSURE* ]] || fail "H-SETTINGS doesn't name both"
# A flag stands in for a blank setting (by hand); pmos-admin never passes one at run.
pf --model-source hosted-test --exposure private
expect H-SETTINGS pass 0 "flags at run"
env_lines PMOS_MODEL_SOURCE=hosted
pf
expect H-SETTINGS fail 1 "hosted (not built) at run"
pf --stage install --model-source hosted
expect H-SETTINGS fail 1 "hosted (not built) at install"
# At install with no deploy/.env, the preflight reads deploy/env.example, which ships both blank:
# what setup.sh is about to fill in (hosted-test and public unless told otherwise), so it passes,
# saying so, and the source-dependent checks read it as local (the stricter reading).
grep -qx 'PMOS_MODEL_SOURCE=' "$REPO/deploy/env.example" && grep -qx 'PMOS_EXPOSURE=' "$REPO/deploy/env.example" ||
  fail "env.example no longer ships the host settings blank: revisit the install reading"
mkdir -p "$WORK/ex/deploy"
cp "$PF" "$REPO/deploy/sandbox-create.sh" "$REPO/deploy/env.example" "$WORK/ex/deploy/"
CODE=0
OUT=$("$WORK/ex/deploy/preflight.sh" --root "$R" --env-file "$WORK/ex/deploy/.env" --json --stage install 2>"$WORK/stderr") || CODE=$?
n_cases=$((n_cases + 1))
expect H-SETTINGS pass 0 "install from env.example"
[[ "$(field H-SETTINGS measured)" == *"setup.sh writes hosted-test"*"setup.sh writes public"*"checked as PMOS_MODEL_SOURCE=local, PMOS_EXPOSURE=public"* ]] ||
  fail "install from env.example doesn't say what setup.sh fills in: $(field H-SETTINGS measured)"
# env.example ships the stub model on, so H-NVIDIA is skipped for the stub, not for the source.
[ "$(field H-NVIDIA measured)" = "PMOS_GATEWAY_STUB=1" ] || fail "install from env.example doesn't read a blank source as local: $(field H-NVIDIA measured)"
CODE=0
OUT=$("$WORK/ex/deploy/preflight.sh" --root "$R" --env-file "$WORK/ex/deploy/.env" --json --stage install --model-source hosted-test --exposure public 2>"$WORK/stderr") || CODE=$?
n_cases=$((n_cases + 1))
expect H-SETTINGS pass 0 "install with setup.sh's flags"
# Run stage with that same blank file as deploy/.env: a fail.
cp "$REPO/deploy/env.example" "$WORK/ex/deploy/.env"
CODE=0
OUT=$("$WORK/ex/deploy/preflight.sh" --root "$R" --env-file "$WORK/ex/deploy/.env" --json 2>"$WORK/stderr") || CODE=$?
n_cases=$((n_cases + 1))
expect H-SETTINGS fail 1 "run with env.example's blanks"
# The quick stage reads no settings and has no H-SETTINGS.
pf --stage quick
[ -z "$(field H-SETTINGS status)" ] || fail "quick runs H-SETTINGS"
env_lines

# --- OpenShell (--openshell): the CLI, the gateway, lingering, the two canaries -------------------
export OPENSHELL_FIXTURE_LOG="$WORK/openshell.log" OPENSHELL_FIXTURE_POLICIES="$WORK/policies"
mkdir -p "$OPENSHELL_FIXTURE_POLICIES"
host
: >"$OPENSHELL_FIXTURE_LOG"
TMPDIR="$WORK/tmp"
mkdir -p "$TMPDIR"
export TMPDIR
pf --openshell
[ "$CODE" = 0 ] || fail "--openshell fails on the Ubuntu host: $OUT"
for id in O-CLI O-GATEWAY O-LINGER O-CANARY O-REACH; do expect "$id" pass 0 "openshell"; done
# Both canaries use pmos-admin sandbox's create command (deploy/sandbox-create.sh), are deleted
# afterwards, and their policy files don't outlive the run.
grep -qx "sandbox create --name pmos-preflight --from pmos-worker:local --policy $WORK/tmp/pmos-preflight\.[A-Za-z0-9]*/canary\.yaml" "$OPENSHELL_FIXTURE_LOG" ||
  fail "the canary isn't created with --from: $(cat "$OPENSHELL_FIXTURE_LOG")"
grep -q '^sandbox create --name pmos-preflight-net --from pmos-worker:local --policy ' "$OPENSHELL_FIXTURE_LOG" || fail "no reach canary"
grep -qx "sandbox exec -n pmos-preflight -- /usr/local/bin/node -e process.exit(0)" "$OPENSHELL_FIXTURE_LOG" || fail "the canary doesn't run node"
grep -qx "logs -n pmos-preflight --source sandbox" "$OPENSHELL_FIXTURE_LOG" || fail "the canary's log isn't read"
[ "$(tail -2 "$OPENSHELL_FIXTURE_LOG" | sort | paste -sd' ' -)" = "sandbox delete pmos-preflight sandbox delete pmos-preflight-net" ] ||
  fail "the canaries aren't deleted at the end: $(tail -3 "$OPENSHELL_FIXTURE_LOG")"
[ -z "$(ls -A "$WORK/tmp")" ] || fail "the canaries' policy files were left behind"
# The canary's policy: no network, Landlock a hard requirement, run as sandbox; the reach canary
# allows only GET /healthz on the three names and their ports.
c=$OPENSHELL_FIXTURE_POLICIES/pmos-preflight.yaml
grep -qx '  compatibility: hard_requirement' "$c" && grep -qx 'network_policies: {}' "$c" &&
  grep -qx '  run_as_user: sandbox' "$c" || fail "the canary's policy: $(cat "$c")"
n=$OPENSHELL_FIXTURE_POLICIES/pmos-preflight-net.yaml
[ "$(grep -c 'method: GET' "$n")" = 3 ] && [ "$(grep -c 'path: /healthz' "$n")" = 3 ] && ! grep -q 'method: POST' "$n" ||
  fail "the reach canary's policy: $(cat "$n")"
for hp in toolserver.pmos.internal:8710 gateway.pmos.internal:8740 bots.pmos.internal:8750; do
  grep -qx "        host: ${hp%%:*}" "$n" && grep -qx "        port: ${hp#*:}" "$n" || fail "the reach canary misses $hp"
done
# ... each name only at its own pinned address, as in every bot's policy (slice H3).
for hp in toolserver.pmos.internal:10.201.77.10 gateway.pmos.internal:10.201.77.11 bots.pmos.internal:10.201.77.12; do
  grep -A2 -x "      - allowed_ips:" "$n" | grep -A1 -x "          - \"${hp#*:}/32\"" | grep -qx "        enforcement: enforce" &&
    [ "$(grep -B3 -x "        host: ${hp%%:*}" "$n" | head -1)" = "      - allowed_ips:" ] &&
    [ "$(grep -B2 -x "        host: ${hp%%:*}" "$n" | head -1)" = "          - \"${hp#*:}/32\"" ] ||
    fail "the reach canary doesn't pin ${hp%%:*} to ${hp#*:}/32: $(cat "$n")"
done
[ "$(grep -c '^          - "10\.201\.77\.[0-9]*/32"$' "$n")" = 3 ] || fail "the reach canary pins other than three addresses: $(cat "$n")"
for case in nostart nonode nolandlock; do
  FAKE_CANARY=$case pf --openshell
  expect O-CANARY fail 1 "canary $case"
done
FAKE_REACH="200 0 200" pf --openshell
expect O-REACH fail 1 "the gateway unreachable from a sandbox"
for ep in "http://127.0.0.1:17670" "http://[::1]:17670" "https://10.0.0.5:17670" "http://10.0.0.5:17670" "https://gateway.example.com:443"; do
  FAKE_OS_ENDPOINT=$ep pf --openshell
  expect O-GATEWAY fail 1 "endpoint $ep"
done
FAKE_OS_ENDPOINT=https://172.17.0.1:17670 pf --openshell
expect O-GATEWAY pass 0 "the docker0 address over TLS"
FAKE_OS_ENDPOINT="https://[::1]:17670" pf --openshell
expect O-GATEWAY pass 0 "IPv6 loopback over TLS"
FAKE_OS_STATUS_EXIT=1 pf --openshell
expect O-CLI fail 1 "openshell status failing"
setcmd linger no
pf --openshell
expect O-LINGER fail 1 "lingering off"
host
setcmd gateway-unit-enabled disabled
pf --openshell
expect O-LINGER fail 1 "the gateway unit not enabled"
host
rm "$R/bin/openshell"
pf --openshell
for id in O-CLI O-GATEWAY O-CANARY O-REACH; do expect "$id" fail 1 "no openshell ($id)"; done
host
pf --stage quick
expect O-GATEWAY pass 0 "quick: O-GATEWAY"
: >"$OPENSHELL_FIXTURE_LOG"
pf --stage quick
! grep -q 'sandbox create' "$OPENSHELL_FIXTURE_LOG" || fail "the quick stage made a canary"
unset TMPDIR

# --- The pinned addresses agree with deploy/pmos-admin and deploy/compose.yaml ---------------------
for v in 'PINNED_NAMES=(toolserver.pmos.internal gateway.pmos.internal bots.pmos.internal)' \
  'PINNED_SERVICE_ADDRS=(10.201.77.10 10.201.77.11 10.201.77.12)' \
  'PINNED_ADDRS=(10.201.77.10 10.201.77.11 10.201.77.12 10.201.77.13 10.201.77.14)' \
  'NETWORK_SUBNET_DEFAULT=10.201.77.0/24' 'NETWORK_GATEWAY_DEFAULT=10.201.77.1' \
  'NETWORK_IP_RANGE_DEFAULT=10.201.77.128/25' 'NETWORK_BRIDGE=pmos0'; do
  grep -qxF "$v" "$PF" && grep -qxF "$v" "$REPO/deploy/pmos-admin" || fail "$v differs between the preflight and pmos-admin"
done
[ "$(grep -F 'NETWORK_INSPECT_FORMAT=' "$PF")" = "$(grep -F 'NETWORK_INSPECT_FORMAT=' "$REPO/deploy/pmos-admin")" ] ||
  fail "the live-network format differs from pmos-admin's"
for p in 8710 8740 8750 8760 8770; do
  grep -q "127.0.0.1:\${PMOS_[A-Z]*_PORT:-$p}:$p" "$REPO/deploy/compose.yaml" || fail "compose.yaml doesn't publish $p as the preflight reads it"
done
# 8771 is the web app's internal listener: the preflight refuses any host listener on it, so
# compose.yaml must set it and publish nothing for it.
grep -qxF 'INTERNAL_PORT=8771' "$PF" || fail "the preflight's internal port isn't 8771"
grep -qxF '      PMOS_WEB_INTERNAL_BIND: 0.0.0.0:8771' "$REPO/deploy/compose.yaml" || fail "compose.yaml doesn't set PMOS_WEB_INTERNAL_BIND to 8771"
[ -z "$(grep '8771' "$REPO/deploy/compose.yaml" | grep -vE '^[[:space:]]*#|^      PMOS_WEB_INTERNAL_BIND: ')" ] || fail "compose.yaml mentions 8771 outside a comment and PMOS_WEB_INTERNAL_BIND (published?)"
# The block's drop-in text is the same in the preflight and pmos-admin.
[ "$(sed -n "/^BLOCK_DROPIN_TEXT='/,/'$/p" "$PF")" = "$(sed -n "/^BLOCK_DROPIN_TEXT='/,/'$/p" "$REPO/deploy/pmos-admin")" ] ||
  fail "the drop-in's text differs between the preflight and pmos-admin"
# Nothing in the repository sets the fixture root but the test copies (the line is empty here).
[ "$(grep -c '^readonly HOST_ROOT_DEFAULT=""$' "$PF")" = 1 ] && [ "$(grep -c 'HOST_ROOT_DEFAULT=' "$PF")" = 1 ] ||
  fail "the preflight's HOST_ROOT_DEFAULT isn't one empty constant"

# --- The Landlock probe itself, on this machine (the result depends on the kernel: not asserted) ---
code=0
out=$("$PF" --env-file "$ENVF" --only H-LANDLOCK) || code=$?
[ "$code" = 0 ] || [ "$code" = 1 ] || fail "the Landlock probe crashed the preflight (exit $code): $out"
[[ "$out" == *"H-LANDLOCK"*" pass "*"ABI "* || "$out" == *"H-LANDLOCK"*" fail "*"not measurable: "* || "$out" == *"H-LANDLOCK"*" fail "*"ABI "* ]] ||
  fail "the Landlock probe printed neither an ABI nor a reason: $out"
printf '%s\n' "$out" | grep H-LANDLOCK

printf 'preflight checks passed (%d runs)\n' "$n_cases"
