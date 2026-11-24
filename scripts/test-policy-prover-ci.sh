#!/usr/bin/env bash
# Checks scripts/policy-prover-ci.sh (CI's `policy-prover` job, slice A2) against the stand-in
# prover (deploy/test/stand-ins/openshell-prover); nothing here downloads or runs the real one:
# - with the committed lock (nothing pinned) the job FAILS and says "prover not available";
# - with the stand-in pinned, every bot is proved, the planted widenings are each `exceeds` and
#   the unmodified policy is `unsupported`, and the job passes;
# - with the stand-in present but another hash pinned, it fails (`not_available`);
# - the boundary-alone rule on a fixture change list: the boundary and a policy together fail,
#   the boundary alone passes.
# PMOS_BIN names a built `pmos` (else it is built with cargo).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
WORK=$(mktemp -d "${TMPDIR:-/tmp}/prover-ci-test.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
if [ -z "${PMOS_BIN:-}" ]; then
  cargo build --locked -q -p pmos
  PMOS_BIN="${CARGO_TARGET_DIR:-$ROOT/target}/debug/pmos"
fi
export PMOS_BIN
case "$(uname -m)" in x86_64 | amd64) ARCH=X86_64 ;; *) ARCH=AARCH64 ;; esac

# Nothing pinned: never a pass.
code=0
out=$(scripts/policy-prover-ci.sh 2>&1) || code=$?
[ "$code" = 1 ] || fail "with nothing pinned the job exits $code, not 1: $out"
[[ "$out" == *"prover not available"* ]] || fail "the job doesn't say the prover is not available: $out"
[[ "$out" == *"docs: not_available"* ]] || fail "the bots aren't reported not_available: $out"
[[ "$out" != *"policy-prover: passed"* ]] || fail "the job said passed with no prover"

# The stand-in, pinned.
mkdir -p "$WORK/prover"
cp deploy/test/stand-ins/openshell-prover "$WORK/prover/openshell-prover"
sum=$(sha256sum "$WORK/prover/openshell-prover" | cut -d' ' -f1)
printf 'PROVER_VERSION=0.1.9\nPROVER_RELEASE_TAG=v0.1.9\nPROVER_BINARY_SHA256_%s=%s\n' "$ARCH" "$sum" >"$WORK/stand-in.lock"
code=0
out=$(scripts/policy-prover-ci.sh --prover "$WORK/prover/openshell-prover" --lock "$WORK/stand-in.lock" 2>&1) || code=$?
[ "$code" = 0 ] || fail "with the stand-in pinned the job exits $code: $out"
for w in fourth-host range-24 writable-sandbox best-effort root; do
  [[ "$out" == *"planted widening $w: exceeds"* ]] || fail "planted widening $w wasn't exceeds: $out"
done
[[ "$out" == *"the unmodified policy: unsupported"* ]] || fail "the unmodified policy wasn't unsupported: $out"
[[ "$out" == *"docs: proved"* && "$out" == *"proved on the network view (MCP rules removed, which only widens)"* ]] ||
  fail "the Docs bot isn't proved on the labelled network view: $out"

# The stand-in present, another binary pinned.
printf 'PROVER_VERSION=0.1.9\nPROVER_BINARY_SHA256_%s=%064d\n' "$ARCH" 0 >"$WORK/other.lock"
code=0
out=$(scripts/policy-prover-ci.sh --prover "$WORK/prover/openshell-prover" --lock "$WORK/other.lock" 2>&1) || code=$?
[ "$code" = 1 ] && [[ "$out" == *"not_available [hash_mismatch]"* ]] || fail "a wrong hash wasn't refused ($code): $out"

# The boundary-alone rule.
printf 'policies/boundary/pmos-bot.yaml\npolicies/bots/docs.yaml\n' >"$WORK/both"
printf 'policies/boundary/pmos-bot.yaml\n' >"$WORK/alone"
code=0
"$PMOS_BIN" policy boundary-alone --changed-files "$WORK/both" >/dev/null || code=$?
[ "$code" = 1 ] || fail "a boundary change with a policy change passed the boundary-alone rule"
"$PMOS_BIN" policy boundary-alone --changed-files "$WORK/alone" >/dev/null || fail "a boundary change alone was refused"

echo "policy-prover CI checks passed"
