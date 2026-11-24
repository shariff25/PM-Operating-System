# shellcheck shell=bash
# PM OS red team: what isolation.sh, exposure.sh and stack.sh share (slice H7,
# docs/openshell-any-host-spec.md §5, "Red-team checklist additions"). Sourced, never run.
#
# Every result is one line: PASS, FAIL, SKIP (not run: the feature isn't built, a precondition
# is missing, or a host-changing step wasn't allowed), HAND (to do by hand: not counted as a
# result) or, with --dry-run, DRY (what would be probed; nothing is). Exit status, the same in
# all three scripts:
#   0  every check that ran passed and none was skipped
#   1  a check failed (an inconclusive probe counts as failed)
#   2  the script couldn't run (usage, a missing tool, a precondition refused)
#   3  no check failed, but at least one was skipped: NOT a pass
# A dry run exits 0 after printing the probes: it is not a result.
#
# What these scripts never do: print a token, a key or a part of one (RT-18 prints only "found"
# or "not found"), print the hosted provider's host or URL, or print a log line or a message's
# text from a service (only counts and timestamps). A step that changes the host (a hosts-file
# line, Docker's restart, a firewall flush, a gateway restart) runs only with
# --allow-host-changes, and everything it changes is put back under a `trap`.

RT_LIB_HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# The checkout the scripts are in: the features they probe are read from its files.
REPO="$(cd "$RT_LIB_HERE/../.." && pwd)"
DRY=0
ALLOW_CHANGES=0
passes=0 fails=0 skips=0 by_hand=0
# fd 3 is the terminal's standard output, so DRY lines printed inside $(…) still show.
exec 3>&1

pass() { passes=$((passes + 1)); [ "$DRY" = 1 ] || printf 'PASS %-7s %s\n' "$1" "$2"; }
fail() { fails=$((fails + 1)); [ "$DRY" = 1 ] || printf 'FAIL %-7s %s\n' "$1" "$2"; }
skip() { skips=$((skips + 1)); printf 'SKIP %-7s %s\n' "$1" "$2" >&3; }
byhand() { by_hand=$((by_hand + 1)); printf 'HAND %-7s %s\n' "$1" "$2" >&3; }
note() { [ "$DRY" = 1 ] || printf '     %s\n' "$*"; }
dry() { printf 'DRY  %-7s %s\n' "$1" "$2" >&3; }
# A section's heading: in a dry run, the check it makes; otherwise nothing.
section() { [ "$DRY" = 0 ] || dry "$1" "$2"; }
# sleep, except in a dry run.
pause_for() { [ "$DRY" = 1 ] || sleep "$1"; }

finish() {
  echo
  if [ "$DRY" = 1 ]; then
    echo "dry run: nothing was probed or changed; this is not a result ($skips would be skipped)"
    exit 0
  fi
  echo "$passes passed, $fails failed, $skips skipped, $by_hand by hand"
  [ "$fails" = 0 ] || exit 1
  if [ "$skips" != 0 ]; then
    echo "not a pass: $skips check(s) skipped (see the SKIP lines)"
    exit 3
  fi
  exit 0
}

# --only ID,ID… : whether a check id was asked for (all, without --only).
ONLY=""
wanted() { [ -z "$ONLY" ] || [[ ",$ONLY," == *",$1,"* ]]; }

# A step that changes the host runs only with --allow-host-changes. Without it the step is
# skipped, and so is the check it belongs to. A dry run prints the change instead.
may_change() { # id what
  [ "$DRY" = 1 ] && { dry "$1" "would change the host: $2 (only with --allow-host-changes)"; return 0; }
  [ "$ALLOW_CHANGES" = 1 ] && return 0
  skip "$1" "not run: it changes the host ($2); run again with --allow-host-changes"
  return 1
}

# The features H7 probes but that other slices build. Read from this checkout's files, which
# must be the commit the host runs (the scripts print it).
#   H10, the admin listener: deploy/compose.yaml publishes 8772 on PMOS_WEB_ADMIN_PUBLISH.
#   H12, the hosted source: crates/gateway/approved-hosted.yaml and `pmos-admin hosted`.
#   An approved entry (H13): a url_sha256 digest in that list.
h10_built() { grep -q 'PMOS_WEB_ADMIN_PUBLISH' "$REPO/deploy/compose.yaml" 2>/dev/null; }
h12_built() {
  [ -f "$REPO/crates/gateway/approved-hosted.yaml" ] && grep -Eq '^  hosted\)' "$REPO/deploy/pmos-admin" 2>/dev/null
}
hosted_entry_exists() {
  h12_built && grep -Eq 'url_sha256:[[:space:]]*"?[0-9a-f]{64}' "$REPO/crates/gateway/approved-hosted.yaml"
}
commit_line() {
  local c
  c=$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null || echo unknown)
  printf 'checkout %s at %s\n' "$REPO" "$c"
}

# One setting from deploy/.env, read as data (never sourced), as deploy/pmos-admin's load_env
# reads it: the last KEY=VALUE line wins, one pair of quotes is removed, an unquoted value ends
# at " #". Prints the value; 1 when the file or the key is missing. Use it only inside $(…), and
# never print what it returns for a key or a URL.
ENV_FILE="$REPO/deploy/.env"
env_value() { # key
  local line v found=1 re
  [ -r "$ENV_FILE" ] || return 1
  re="^[[:space:]]*(export[[:space:]]+)?$1=(.*)$"
  while IFS= read -r line || [ -n "$line" ]; do
    line=${line%$'\r'}
    if [[ "$line" =~ $re ]]; then
      v=${BASH_REMATCH[2]}
      found=0
    fi
  done <"$ENV_FILE"
  [ "$found" = 0 ] || return 1
  if [[ "$v" =~ ^\"([^\"]*)\"([[:space:]]+#.*)?[[:space:]]*$ || "$v" =~ ^\'([^\']*)\'([[:space:]]+#.*)?[[:space:]]*$ ]]; then
    v=${BASH_REMATCH[1]}
  else
    v=${v%%[[:space:]]#*}
    v=${v%"${v##*[![:space:]]}"}
  fi
  printf '%s' "$v"
}

# The id of a container of the kit's Compose project (deploy/compose.yaml: `name: pmos`).
compose_container() { # service
  docker ps -aq --filter label=com.docker.compose.project=pmos --filter "label=com.docker.compose.service=$1" 2>/dev/null | head -1
}
