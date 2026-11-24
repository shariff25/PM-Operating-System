#!/usr/bin/env bash
# Checks scripts/policy-prover-ci.sh (CI's `policy-prover` job, slice A2) and
# scripts/build-openshell-prover.sh against the stand-in prover
# (deploy/test/stand-ins/openshell-prover), a stand-in cargo and a local fixture repository;
# nothing here fetches, builds or runs the real prover:
# - with nothing pinned the job FAILS and says "prover not available";
# - with the stand-in pinned, every bot is proved, the planted widenings are each refused as the
#   real prover refuses them (`exceeds`; a writable /sandbox `unsupported`), the unmodified policy
#   is `unsupported` (also with its credential bindings removed), and the job passes;
# - with the stand-in present but another hash pinned, it fails (`not_available`);
# - the source path: a fixture tag at the pinned commit is "built" (the stand-in cargo copies the
#   stand-in prover) and proved with its build record; the tag at another commit, a failing build
#   and a build that leaves no binary each end "prover not available" and FAIL the job; a build
#   record of another commit is refused (`build_record_mismatch`);
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
# Nothing below may reach a real build directory or proof record.
unset PMOS_PROVER_DIR PMOS_PROOF_RECORD
case "$(uname -m)" in x86_64 | amd64) ARCH=X86_64 ;; *) ARCH=AARCH64 ;; esac

# Nothing pinned: never a pass.
printf 'PROVER_VERSION=\nPROVER_SOURCE_REPO=\nPROVER_SOURCE_TAG=\nPROVER_SOURCE_COMMIT=\nPROVER_Z3_FEATURE=\nPROVER_RELEASE_TAG=\n' >"$WORK/empty.lock"
code=0
out=$(scripts/policy-prover-ci.sh --lock "$WORK/empty.lock" 2>&1) || code=$?
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
planted_ok() { # output label
  for w in fourth-host range-24 best-effort root gateway-route; do
    [[ "$1" == *"planted widening $w: exceeds, as it must"* ]] || fail "$2: planted widening $w wasn't exceeds: $1"
  done
  [[ "$1" == *"planted widening writable-sandbox: unsupported [unresolved_filesystem_path], as it must"* ]] ||
    fail "$2: the writable /sandbox wasn't unsupported (unresolved_filesystem_path): $1"
}
planted_ok "$out" "the stand-in"
[[ "$out" == *"the unmodified policy: unsupported"* ]] || fail "the unmodified policy wasn't unsupported: $out"
[[ "$out" == *"docs: proved"* && "$out" == *"proved on the network view (MCP rules removed, which only widens; credential bindings removed, which the prover doesn't model)"* ]] ||
  fail "the Docs bot isn't proved on the labelled network view: $out"
[[ "$out" == *"prover SHA-256: $sum"* && "$out" == *"prover --version: openshell-prover 0.1.9"* ]] ||
  fail "the job doesn't log the prover's SHA-256 and version: $out"
[[ "$out" == *"proof record:"* && "$out" == *"\"sha256\": \"$sum\""* ]] || fail "the job doesn't print the proof record: $out"

# The stand-in present, another binary pinned.
printf 'PROVER_VERSION=0.1.9\nPROVER_BINARY_SHA256_%s=%064d\n' "$ARCH" 0 >"$WORK/other.lock"
code=0
out=$(scripts/policy-prover-ci.sh --prover "$WORK/prover/openshell-prover" --lock "$WORK/other.lock" 2>&1) || code=$?
[ "$code" = 1 ] && [[ "$out" == *"not_available [hash_mismatch]"* ]] || fail "a wrong hash wasn't refused ($code): $out"

# The source path. A fixture repository stands for the prover's: tag v0.1.9 at commit A, then a
# commit B. A stand-in cargo "builds" by copying the stand-in prover (its mode file says how).
src="$WORK/source-repo"
mkdir -p "$src"
git -C "$src" init -q
git -C "$src" config user.email fixture@example.invalid
git -C "$src" config user.name fixture
printf '[toolchain]\nchannel = "1.95.0"\n' >"$src/rust-toolchain.toml"
printf '[workspace.package]\nversion = "0.0.0"\n' >"$src/Cargo.toml"
printf '[[package]]\nname = "openshell-prover-cli"\nversion = "0.0.0"\n' >"$src/Cargo.lock"
git -C "$src" add -A
git -C "$src" commit -q -m A
commit_a=$(git -C "$src" rev-parse HEAD)
git -C "$src" tag v0.1.9
git -C "$src" commit -q --allow-empty -m B
commit_b=$(git -C "$src" rev-parse HEAD)
mkdir -p "$WORK/tools"
cat >"$WORK/tools/cargo" <<'EOF'
#!/usr/bin/env bash
# A stand-in for cargo (scripts/test-policy-prover-ci.sh): `fetch` succeeds; `build` does what the
# file `mode` beside it says: ok (copy the stand-in prover into the target), fail, no-binary.
here=$(cd "$(dirname "$0")" && pwd)
case "${1:-}" in
  fetch) exit 0 ;;
  build)
    grep -q '^version = "0.1.9"$' Cargo.toml || { echo "the version wasn't stamped" >&2; exit 101; }
    case "$(cat "$here/mode")" in
      ok) mkdir -p "$CARGO_TARGET_DIR/release" && cp "$here/prover" "$CARGO_TARGET_DIR/release/openshell-prover" ;;
      fail) echo "error: could not compile" >&2; exit 101 ;;
      no-binary) exit 0 ;;
    esac ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$WORK/tools/cargo"
cp deploy/test/stand-ins/openshell-prover "$WORK/tools/prover"
export PMOS_PROVER_CARGO="$WORK/tools/cargo" PMOS_PROVER_RUSTUP=true
source_lock() { # commit
  printf 'PROVER_VERSION=0.1.9\nPROVER_SOURCE_REPO=https://example.invalid/openshell\nPROVER_SOURCE_TAG=v0.1.9\nPROVER_SOURCE_COMMIT=%s\nPROVER_Z3_FEATURE=vendored-z3\nPROVER_RELEASE_TAG=\n' "$1"
}
source_lock "$commit_a" >"$WORK/source.lock"
run_source() { # lock mode: sets code and out
  echo "$2" >"$WORK/tools/mode"
  rm -rf "$WORK/build"
  code=0
  out=$(PMOS_PROVER_DIR="$WORK/build" scripts/policy-prover-ci.sh --lock "$1" --source-url "$src" 2>&1) || code=$?
}
run_source "$WORK/source.lock" ok
[ "$code" = 0 ] || fail "the pinned source, built, doesn't pass ($code): $out"
[[ "$out" == *"docs: proved"* && "$out" == *"build record ("* && "$out" == *"PROVER_SOURCE_COMMIT=$commit_a"* ]] ||
  fail "the source build wasn't proved with its build record: $out"
[[ "$out" == *"binary SHA-256 pinned by build_record"* ]] || fail "the binary wasn't checked against the build record: $out"
planted_ok "$out" "source path"
# The tag names another commit than the pinned one.
source_lock "$commit_b" >"$WORK/wrong.lock"
run_source "$WORK/wrong.lock" ok
[ "$code" = 1 ] && [[ "$out" == *"not the pinned $commit_b"* && "$out" == *"prover not available"* && "$out" == *"docs: not_available"* ]] ||
  fail "a tag at another commit wasn't refused ($code): $out"
[[ "$out" != *"policy-prover: passed"* ]] || fail "the job passed on another commit"
# The build fails; the build leaves no binary.
for mode in fail no-binary; do
  run_source "$WORK/source.lock" "$mode"
  [ "$code" = 1 ] && [[ "$out" == *"prover not available"* && "$out" == *"docs: not_available"* ]] ||
    fail "a build that ends $mode wasn't 'prover not available' ($code): $out"
  [[ "$out" != *"policy-prover: passed"* ]] || fail "the job passed after a build that ends $mode"
done
# A build record of another commit (the binary is right, the record isn't of the pin).
printf 'PROVER_VERSION=0.1.9\nPROVER_SOURCE_COMMIT=%s\nPROVER_BINARY_SHA256_%s=%s\n' "$commit_b" "$ARCH" "$sum" >"$WORK/other.record"
code=0
out=$(scripts/policy-prover-ci.sh --prover "$WORK/prover/openshell-prover" --build-record "$WORK/other.record" --lock "$WORK/source.lock" 2>&1) || code=$?
[ "$code" = 1 ] && [[ "$out" == *"not_available [build_record_mismatch]"* ]] || fail "a build record of another commit wasn't refused ($code): $out"
unset PMOS_PROVER_CARGO PMOS_PROVER_RUSTUP

# The committed lock pins a source (not a release nobody verified), in the shape the build takes.
for k in PROVER_VERSION PROVER_SOURCE_REPO PROVER_SOURCE_TAG PROVER_SOURCE_COMMIT PROVER_Z3_FEATURE; do
  v=$(awk -F= -v k="$k" '$1 == k { sub(/^[^=]*=/, ""); print; exit }' deploy/openshell-prover.lock)
  [ -n "$v" ] || fail "deploy/openshell-prover.lock doesn't pin $k"
done
grep -qE '^PROVER_SOURCE_COMMIT=[0-9a-f]{40}$' deploy/openshell-prover.lock || fail "the pinned commit isn't a full SHA"
grep -qx 'PROVER_Z3_FEATURE=vendored-z3' deploy/openshell-prover.lock || fail "Z3 isn't vendored"

# The boundary-alone rule.
printf 'policies/boundary/pmos-bot.yaml\npolicies/bots/docs.yaml\n' >"$WORK/both"
printf 'policies/boundary/pmos-bot.yaml\n' >"$WORK/alone"
code=0
"$PMOS_BIN" policy boundary-alone --changed-files "$WORK/both" >/dev/null || code=$?
[ "$code" = 1 ] || fail "a boundary change with a policy change passed the boundary-alone rule"
"$PMOS_BIN" policy boundary-alone --changed-files "$WORK/alone" >/dev/null || fail "a boundary change alone was refused"

echo "policy-prover CI checks passed"
