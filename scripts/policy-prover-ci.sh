#!/usr/bin/env bash
# CI's `policy-prover` job (docs/openshell-prover-and-sandbox-audit-spec.md §A.1, §A.6, §A.7,
# slice A2). Run from the repository root:
#
#   scripts/policy-prover-ci.sh [--base <ref>]
#
# 1. The boundary-alone rule (PRV-5): with --base, a change to policies/boundary/ may change no
#    other file (`pmos policy boundary-alone` on `git diff --name-only <base>...HEAD`).
# 2. The pinned prover (PRV-7): deploy/openshell-prover.lock names the release, the version and the
#    SHA-256 of each archive and binary. Nothing pinned: every bot is `not_available` and the job
#    FAILS with "prover not available" (it is never a pass). Pinned: the archive is downloaded and
#    checked against the lock's SHA-256 (not only the release's own checksum file).
# 3. Every bot's policy is proved against the BASE branch's boundary (so a pull request can't widen
#    a policy and its boundary together); without --base, or when the base has no boundary yet
#    (the pull request that adds it), the checked-out one, and the job says so.
# 4. With the real prover, the acceptance checks of §A.8: the unmodified policy is `unsupported`
#    (MCP); planted widenings (a fourth host, a /24, a writable /sandbox, best_effort, root) are
#    each `exceeds`.
# 5. A pin bump (PRV-8): when the base's lock pins another prover, both run and the job fails if
#    any bot's status differs.
#
# Tests only (scripts/test-policy-prover-ci.sh): --prover <binary> --lock <file> use a given
# binary and pin instead of downloading; the binary must still match that pin. The workflow never
# passes them. The job has no secrets and no write token.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
base=""
given_prover=""
lock="deploy/openshell-prover.lock"
while [ $# -gt 0 ]; do
  case "$1" in
    --base) [ $# -ge 2 ] || { echo "usage: $0 [--base <ref>]" >&2; exit 2; }; base=$2; shift 2 ;;
    --prover) given_prover=$2; shift 2 ;;
    --lock) lock=$2; shift 2 ;;
    *) echo "usage: $0 [--base <ref>]" >&2; exit 2 ;;
  esac
done
PMOS=${PMOS_BIN:-}
if [ -z "$PMOS" ]; then
  cargo build --locked -q -p pmos
  PMOS="${CARGO_TARGET_DIR:-$ROOT/target}/debug/pmos"
fi
WORK=$(mktemp -d "${TMPDIR:-/tmp}/policy-prover.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
failed=0
note() { printf '%s\n' "$*"; }

lock_value() { # file key: the value, read as data
  awk -F= -v k="$2" '$1 == k { sub(/^[^=]*=/, ""); print; exit }' "$1"
}
case "$(uname -m)" in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) note "prover not available: no pinned prover for $(uname -m)"; exit 1 ;;
esac
ARCH_KEY=$(printf '%s' "$arch" | tr '[:lower:]' '[:upper:]')

# 1. The boundary-alone rule.
base_has_boundary=0
if [ -n "$base" ]; then
  git rev-parse --verify --quiet "$base^{commit}" >/dev/null || { note "no such base: $base" >&2; exit 2; }
  if git cat-file -e "$base:policies/boundary/pmos-bot.yaml" 2>/dev/null; then base_has_boundary=1; fi
  git diff --name-only --end-of-options "$base"...HEAD >"$WORK/changed"
  if [ "$base_has_boundary" = 1 ]; then
    "$PMOS" policy boundary-alone --changed-files "$WORK/changed" || failed=1
  else
    note "boundary-alone: the base has no boundary yet (this change adds it); the rule applies from the next change"
  fi
fi

# 3. The boundary to prove against.
if [ "$base_has_boundary" = 1 ]; then
  git show "$base:policies/boundary/pmos-bot.yaml" >"$WORK/boundary.yaml"
  note "proving against the base branch's boundary ($base)"
else
  cp policies/boundary/pmos-bot.yaml "$WORK/boundary.yaml"
  note "proving against this checkout's boundary (BOOTSTRAP or a push: no base boundary to hold it to; the reviewer must read the boundary itself)"
fi

# 2. The pinned prover.
fetch_prover() { # lock-file destination-dir: prints the binary's path, or nothing
  local l=$1 d=$2 version tag sum url
  version=$(lock_value "$l" PROVER_VERSION)
  tag=$(lock_value "$l" PROVER_RELEASE_TAG)
  sum=$(lock_value "$l" "PROVER_ARCHIVE_SHA256_$ARCH_KEY")
  [ -n "$version" ] && [ -n "$tag" ] && [ -n "$sum" ] || return 0
  mkdir -p "$d"
  # Verify with the real release (slice A2): the repository and the archive's name and layout.
  url="https://github.com/NVIDIA/OpenShell/releases/download/$tag/openshell-prover-$arch-unknown-linux-musl.tar.gz"
  curl -fsSL --proto '=https' --retry 3 -o "$d/prover.tar.gz" "$url" || { note "the pinned prover couldn't be downloaded from $url" >&2; return 0; }
  printf '%s  %s\n' "$sum" "$d/prover.tar.gz" | sha256sum -c --quiet - || { note "the downloaded archive's SHA-256 isn't the pinned one: not run" >&2; return 0; }
  tar -xzf "$d/prover.tar.gz" -C "$d"
  find "$d" -type f -name openshell-prover -perm -u+x | head -n1
}
if [ -n "$given_prover" ]; then
  prover=$given_prover
else
  prover=$(fetch_prover "$lock" "$WORK/new")
fi
if [ -z "$prover" ]; then
  note "prover not available: $lock pins no prover for $arch (or it couldn't be fetched and checked). Every bot is not_available; this job fails until the owner pins one (see the lock file)."
  prover="$WORK/no-prover"
fi

# Every bot, against the boundary. `pmos policy prove` checks the binary's SHA-256 and version
# against the pin itself, and reports `not_available` or `version_mismatch` if they differ.
set +e
"$PMOS" policy prove --root content --policies policies --boundary "$WORK/boundary.yaml" \
  --prover "$prover" --lock "$lock"
code=$?
"$PMOS" policy prove --root content --policies policies --boundary "$WORK/boundary.yaml" \
  --prover "$prover" --lock "$lock" --format json >"$WORK/new.json"
set -e
case "$code" in
  0) ;;
  1) failed=1 ;;
  *) note "pmos policy prove could not run (exit $code)"; exit 2 ;;
esac

statuses() { python3 -I -c 'import json,sys; [print(r["bot"], r["status"]) for r in json.load(open(sys.argv[1]))["results"]]' "$1"; }
pinned=0
python3 -I -c 'import json,sys; sys.exit(0 if any(r["status"] not in ("not_available",) for r in json.load(open(sys.argv[1]))["results"]) else 1)' "$WORK/new.json" && pinned=1

# 4. The acceptance checks with the prover (only when it ran: not_available proves nothing here).
if [ "$pinned" = 1 ]; then
  plant() { # name sed-expression
    sed -e "$2" policies/bots/docs.yaml >"$WORK/planted-$1.yaml"
    cmp -s policies/bots/docs.yaml "$WORK/planted-$1.yaml" && { note "planted widening $1 changed nothing" >&2; failed=1; return; }
    set +e
    "$PMOS" policy prove --root content --boundary "$WORK/boundary.yaml" --prover "$prover" --lock "$lock" \
      --candidate "$WORK/planted-$1.yaml" --bot docs --format json >"$WORK/planted-$1.json"
    set -e
    got=$(statuses "$WORK/planted-$1.json" | awk '{print $2}')
    if [ "$got" = exceeds ]; then note "planted widening $1: exceeds (as it must)"; else note "planted widening $1: $got, not exceeds" >&2; failed=1; fi
  }
  plant fourth-host 's|^  pmos_toolserver:$|  pmos_planted:\n    binaries:\n      - path: /usr/local/bin/node\n    endpoints:\n      - host: planted.example\n        port: 443\n  pmos_toolserver:|'
  plant range-24 's|10\.201\.77\.11/32|10.201.77.0/24|'
  plant writable-sandbox 's|^    - /sandbox/workspace$|    - /sandbox|'
  plant best-effort 's|compatibility: hard_requirement|compatibility: best_effort|'
  plant root 's|run_as_user: sandbox|run_as_user: root|'
  # The unmodified policy, straight to the prover (no view): the MCP rules are unsupported.
  set +e
  "$prover" check policies/bots/docs.yaml --boundary "$WORK/boundary.yaml" --output json --timeout 60 >"$WORK/raw.json"
  raw=$?
  set -e
  if [ "$raw" = 3 ] && grep -q '"unsupported"' "$WORK/raw.json"; then
    note "the unmodified policy: unsupported (MCP rules), as documented"
  else
    note "the unmodified policy: exit $raw, not unsupported; the network view's premise needs review" >&2
    failed=1
  fi
fi

# 5. A pin bump: the base's prover gives every bot the same status.
if [ -n "$base" ] && [ -z "$given_prover" ] && git cat-file -e "$base:deploy/openshell-prover.lock" 2>/dev/null; then
  git show "$base:deploy/openshell-prover.lock" >"$WORK/base.lock"
  if ! cmp -s "$WORK/base.lock" "$lock" && [ -n "$(lock_value "$WORK/base.lock" PROVER_VERSION)" ]; then
    old=$(fetch_prover "$WORK/base.lock" "$WORK/old")
    if [ -z "$old" ]; then
      note "pin bump: the base's prover couldn't be fetched, so the statuses can't be compared" >&2
      failed=1
    else
      set +e
      "$PMOS" policy prove --root content --policies policies --boundary "$WORK/boundary.yaml" \
        --prover "$old" --lock "$WORK/base.lock" --format json >"$WORK/old.json"
      set -e
      if [ "$(statuses "$WORK/old.json")" != "$(statuses "$WORK/new.json")" ]; then
        note "pin bump: a bot's status changed between the old and the new prover (PRV-8):" >&2
        diff <(statuses "$WORK/old.json") <(statuses "$WORK/new.json") >&2 || true
        failed=1
      else
        note "pin bump: every bot's status is the same with the old and the new prover"
      fi
    fi
  fi
fi

if [ "$failed" = 0 ]; then
  note "policy-prover: passed"
else
  note "policy-prover: FAILED"
fi
exit "$failed"
