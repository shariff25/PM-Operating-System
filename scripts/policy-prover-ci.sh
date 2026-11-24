#!/usr/bin/env bash
# CI's `policy-prover` job (docs/openshell-prover-and-sandbox-audit-spec.md §A.1, §A.6, §A.7,
# slice A2). Run from the repository root:
#
#   scripts/policy-prover-ci.sh [--base <ref>]
#
# 1. The boundary-alone rule (PRV-5): with --base, a change to policies/boundary/ may change no
#    other file (`pmos policy boundary-alone` on `git diff --name-only <base>...HEAD`).
# 2. The pinned prover (PRV-7): deploy/openshell-prover.lock pins the prover's SOURCE (repository,
#    tag, full commit SHA, version, Z3 feature). scripts/build-openshell-prover.sh fetches the tag,
#    refuses any commit but the pinned one, builds it (offline, empty environment) and writes a
#    build record (commit, version, the binary's SHA-256) that `pmos policy prove --build-record`
#    checks the binary against. Optional, only if filled in: a release archive whose SHA-256 and
#    binary SHA-256 are in the lock. Nothing pinned, or the fetch, the commit check or the build
#    fails, or no binary comes out: every bot is `not_available` and the job FAILS with "prover
#    not available" (it is never a pass). The binary's SHA-256, `--version` and the build record
#    are printed, and the whole JSON report (the proof record) too.
# 3. Every bot's policy is proved against the BASE branch's boundary (so a pull request can't widen
#    a policy and its boundary together); without --base, or when the base has no boundary yet
#    (the pull request that adds it), the checked-out one, and the job says so.
# 4. With the real prover, the acceptance checks of §A.8: the unmodified policy is `unsupported`
#    (credential bindings, then MCP); planted widenings (a fourth host, a /24, a new gateway route,
#    best_effort, root) are `exceeds`, and a writable /sandbox is `unsupported`
#    (unresolved_filesystem_path); see below. None of them is ever `proved`.
# 5. A pin bump (PRV-8): when the base's lock pins another prover, both run and the job fails if
#    any bot's status differs.
#
# Tests only (scripts/test-policy-prover-ci.sh): --prover <binary> [--build-record <file>]
# --lock <file> use a given binary and pin instead of building; the binary must still match that
# pin. --source-url <url> builds from another repository (a local fixture). The workflow never
# passes them. The job has no secrets and no write token.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
base=""
given_prover=""
given_record=""
source_url=""
lock="deploy/openshell-prover.lock"
while [ $# -gt 0 ]; do
  case "$1" in
    --base) [ $# -ge 2 ] || { echo "usage: $0 [--base <ref>]" >&2; exit 2; }; base=$2; shift 2 ;;
    --prover) given_prover=$2; shift 2 ;;
    --build-record) given_record=$2; shift 2 ;;
    --lock) lock=$2; shift 2 ;;
    --source-url) source_url=$2; shift 2 ;;
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
# Where the prover is built (its source, Cargo target and binary). The workflow sets
# PMOS_PROVER_DIR to a directory it caches by the pinned commit, so the long Z3 build runs once.
PROVER_DIR=${PMOS_PROVER_DIR:-$WORK/prover}
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

# 2. The pinned prover: built from the pinned source commit (the default), or, only when the lock
#    also pins a release archive and its binary, downloaded and checked against the lock.
# obtain_prover lock-file work-dir: prints the binary's path, or nothing. A source build leaves
# its build record beside the binary (build.record).
obtain_prover() {
  local l=$1 d=$2 version commit tag sum url built
  version=$(lock_value "$l" PROVER_VERSION)
  commit=$(lock_value "$l" PROVER_SOURCE_COMMIT)
  [ -n "$version" ] || return 0
  if [ -n "$commit" ]; then
    built=$(scripts/build-openshell-prover.sh --lock "$l" --out "$d" ${source_url:+--source-url "$source_url"}) ||
      { note "the pinned prover source couldn't be fetched and built (see above)" >&2; return 0; }
    [ -n "$built" ] && [ -x "$built" ] && [ -f "$(dirname "$built")/build.record" ] || return 0
    printf '%s\n' "$built"
    return 0
  fi
  tag=$(lock_value "$l" PROVER_RELEASE_TAG)
  sum=$(lock_value "$l" "PROVER_ARCHIVE_SHA256_$ARCH_KEY")
  [ -n "$tag" ] && [ -n "$sum" ] || return 0
  mkdir -p "$d"
  # Optional path, unverified: no one has confirmed the release archives' names or contents.
  url="https://github.com/NVIDIA/OpenShell/releases/download/$tag/openshell-prover-$arch-unknown-linux-musl.tar.gz"
  curl -fsSL --proto '=https' --retry 3 -o "$d/prover.tar.gz" "$url" || { note "the pinned prover couldn't be downloaded from $url" >&2; return 0; }
  printf '%s  %s\n' "$sum" "$d/prover.tar.gz" | sha256sum -c --quiet - || { note "the downloaded archive's SHA-256 isn't the pinned one: not run" >&2; return 0; }
  tar -xzf "$d/prover.tar.gz" -C "$d"
  find "$d" -type f -name openshell-prover -perm -u+x | head -n1
}
record=""
if [ -n "$given_prover" ]; then
  prover=$given_prover
  record=$given_record
else
  prover=$(obtain_prover "$lock" "$PROVER_DIR/new") || true
  if [ -n "$prover" ] && [ -n "$(lock_value "$lock" PROVER_SOURCE_COMMIT)" ]; then
    record="$(dirname "$prover")/build.record"
  fi
fi
if [ -z "$prover" ]; then
  note "prover not available: $lock pins no prover for $arch, or it couldn't be fetched, built and checked. Every bot is not_available; this job fails until it is (see the lock file)."
  prover="$WORK/no-prover"
else
  note "prover: $prover"
  note "prover SHA-256: $(sha256sum "$prover" | cut -d' ' -f1)"
  note "prover --version: $(env -i PATH=/usr/bin:/bin "$prover" --version 2>/dev/null || echo '(failed)')"
  if [ -n "$record" ]; then note "build record ($record):"; grep -v '^#' "$record" | sed 's/^/  /'; fi
fi
record_args=()
[ -n "$record" ] && record_args=(--build-record "$record")

# Every bot, against the boundary. `pmos policy prove` checks the binary's SHA-256 (against the
# build record of the pinned commit, or the lock) and version itself, and reports `not_available`
# or `version_mismatch` if they differ.
set +e
"$PMOS" policy prove --root content --policies policies --boundary "$WORK/boundary.yaml" \
  --prover "$prover" --lock "$lock" "${record_args[@]}"
code=$?
"$PMOS" policy prove --root content --policies policies --boundary "$WORK/boundary.yaml" \
  --prover "$prover" --lock "$lock" "${record_args[@]}" --format json >"$WORK/new.json"
set -e
# The proof record: the whole JSON report (prover path, version, SHA-256, source commit, boundary
# and candidate hashes, every result), in the log and, with PMOS_PROOF_RECORD, in that file (the
# workflow uploads it).
note "proof record:"
cat "$WORK/new.json"
if [ -n "${PMOS_PROOF_RECORD:-}" ]; then cp "$WORK/new.json" "$PMOS_PROOF_RECORD"; fi
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
  # Each planted widening must be refused as what the real prover says of it (verified against
  # the prover built from source at v0.1.10 on 2026-11-24): a new host, a wider range, a new REST
  # route, best_effort and root are `exceeds`. A new writable path is `unsupported` with
  # `unresolved_filesystem_path`, not `exceeds`: the prover compares write paths exactly and
  # won't decide a path the boundary doesn't list (Landlock resolves paths inside the sandbox),
  # unless the boundary allows no writes at all. Either way it is never `proved`.
  plant() { # name expected-status expected-reason-code-or-"-" sed-expression
    sed -e "$4" policies/bots/docs.yaml >"$WORK/planted-$1.yaml"
    cmp -s policies/bots/docs.yaml "$WORK/planted-$1.yaml" && { note "planted widening $1 changed nothing" >&2; failed=1; return; }
    set +e
    "$PMOS" policy prove --root content --boundary "$WORK/boundary.yaml" --prover "$prover" --lock "$lock" \
      "${record_args[@]}" --candidate "$WORK/planted-$1.yaml" --bot docs --format json >"$WORK/planted-$1.json"
    set -e
    got=$(python3 -I -c 'import json,sys; r=json.load(open(sys.argv[1]))["results"][0]; print(r["status"], r.get("reason_code") or "-", r.get("counterexample") or "")' "$WORK/planted-$1.json")
    read -r status code cx <<<"$got"
    if [ "$status" = "$2" ] && { [ "$3" = - ] || [ "$code" = "$3" ]; }; then
      shown=$status
      [ "$code" = - ] || shown="$status [$code]"
      note "planted widening $1: $shown, as it must${cx:+: $cx}"
    else
      note "planted widening $1: $status [$code], not $2 [$3]" >&2
      failed=1
    fi
  }
  plant fourth-host exceeds - 's|^  pmos_toolserver:$|  pmos_planted:\n    binaries:\n      - path: /usr/local/bin/node\n    endpoints:\n      - host: planted.example\n        port: 443\n  pmos_toolserver:|'
  plant range-24 exceeds - 's|10\.201\.77\.11/32|10.201.77.0/24|'
  plant writable-sandbox unsupported unresolved_filesystem_path 's|^    - /sandbox/workspace$|    - /sandbox|'
  plant best-effort exceeds - 's|compatibility: hard_requirement|compatibility: best_effort|'
  plant root exceeds - 's|run_as_user: sandbox|run_as_user: root|'
  plant gateway-route exceeds - 's|path: /v1/chat/completions|path: /v1/models|'
  # The unmodified policy, straight to the prover (no view): unsupported. Its credential bindings
  # are refused first ("authority outside the initial model"); with them removed and the MCP rules
  # kept, it must still be unsupported, naming the MCP endpoint: the premise of the network view.
  raw_check() { # file: prints "exit result reason"
    set +e
    env -i PATH=/usr/bin:/bin "$prover" check "$1" --boundary "$WORK/boundary.yaml" --output json --timeout 60s >"$1.json"
    local e=$?
    set -e
    python3 -I -c 'import json,sys; d=json.load(open(sys.argv[1])); print(sys.argv[2], d.get("result"), d.get("reason") or "")' "$1.json" "$e" 2>/dev/null || echo "$e none"
  }
  cp policies/bots/docs.yaml "$WORK/raw.yaml"
  read -r raw raw_result raw_reason <<<"$(raw_check "$WORK/raw.yaml")"
  sed -e '/^        credential_binding:$/,+1d' policies/bots/docs.yaml >"$WORK/raw-mcp.yaml"
  read -r mcp mcp_result mcp_reason <<<"$(raw_check "$WORK/raw-mcp.yaml")"
  if [ "$raw" = 3 ] && [ "$raw_result" = unsupported ] && [ "$mcp" = 3 ] && [ "$mcp_result" = unsupported ] &&
    ! grep -q credential_binding "$WORK/raw-mcp.yaml" && [[ "$mcp_reason" == *"'pmos_toolserver'"* ]]; then
    note "the unmodified policy: unsupported ($raw_reason); without its credential bindings, still unsupported for the MCP endpoint ($mcp_reason)"
  else
    note "the unmodified policy: exit $raw $raw_result ($raw_reason); without credential bindings: exit $mcp $mcp_result ($mcp_reason): the network view's premise needs review" >&2
    failed=1
  fi
fi

# 5. A pin bump: the base's prover gives every bot the same status.
if [ -n "$base" ] && [ -z "$given_prover" ] && git cat-file -e "$base:deploy/openshell-prover.lock" 2>/dev/null; then
  git show "$base:deploy/openshell-prover.lock" >"$WORK/base.lock"
  if ! cmp -s "$WORK/base.lock" "$lock" && [ -n "$(lock_value "$WORK/base.lock" PROVER_VERSION)" ]; then
    old=$(obtain_prover "$WORK/base.lock" "$PROVER_DIR/old") || true
    if [ -z "$old" ]; then
      note "pin bump: the base's prover couldn't be obtained, so the statuses can't be compared" >&2
      failed=1
    else
      old_record=()
      if [ -n "$(lock_value "$WORK/base.lock" PROVER_SOURCE_COMMIT)" ]; then
        old_record=(--build-record "$(dirname "$old")/build.record")
      fi
      set +e
      "$PMOS" policy prove --root content --policies policies --boundary "$WORK/boundary.yaml" \
        --prover "$old" --lock "$WORK/base.lock" "${old_record[@]}" --format json >"$WORK/old.json"
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
