#!/usr/bin/env bash
# Finds promises about where model prompts go that were written when PM OS ran only on local
# models ("prompts never leave the host", "open-weight models only", ...). Since the owner's
# decisions of 2026-11-22, real data may be processed by a local model on a GPU host the owner
# controls or by one hosted model the owner has approved, so such a sentence tells a PM or an
# operator something that is no longer true (docs/openshell-any-host-spec.md §3.9, "H6's test").
#
#   scripts/check-model-promises.sh              check the repository (run from anywhere)
#   scripts/check-model-promises.sh --self-test  check the matcher itself
#
# Exact phrases, case-insensitive. Each file is read with its line breaks and leading comment
# markers (//!, ///, //, #) folded into single spaces before matching, so a phrase split over two
# lines is found. A loose word such as "never leave" is not used: "a crash never leaves half a
# file" is an unrelated comment. Exits 1 naming the file and line of each unexpected match.
set -euo pipefail

PHRASES=(
  "prompts never leave"
  "never leave the spark"
  "never leave the host"
  "open-weight models only"
  "open-weight only"
  "local and open-weight only"
  "no frontier api"
  "local models only"
  "no outbound model route"
  "sent off the spark"
)

# Paths searched (git pathspecs).
SCOPE=(README.md docs config deploy ':(glob)crates/*/src/**' content)

# Left out, each with its reason: dated records that state what was decided when.
EXCLUDED=(
  "docs/openshell-any-host-spec.md"          # the any-host design; quotes the old promises to replace them
  "docs/security-fixes-spec.md"              # dated spec of record
  "docs/bot-marketplace-spec.md"             # dated spec of record
  "docs/bot-runtime-spec.md"                 # dated spec of record
  "docs/improvement-loop-spec.md"            # dated spec of record
  "docs/user-database-spec.md"               # dated spec of record
  "docs/onboarding-spec.md"                  # dated spec of record
  "docs/research/"                           # research reports of record
)

# Expected matches: reported, not failed.
EXPECTED=(
  # The team's platform decision is the owner's content; its amended wording (spec §3.9) waits
  # on the owner, through the content admin flow.
  "content/context/decisions/2026-10-product-agent-platform.md"
)

# Prints "path:line: phrase" for every match in the files named on stdin (one per line).
scan() {
  local phrases
  phrases=$(printf '%s\n' "${PHRASES[@]}")
  PHRASES_TEXT="$phrases" perl -e '
    use strict; use warnings;
    my @phrases = grep { length } split /\n/, $ENV{PHRASES_TEXT};
    while (my $path = <STDIN>) {
      chomp $path;
      open my $fh, "<", $path or next;
      my (@starts, $folded);
      $folded = "";
      my $n = 0;
      while (my $line = <$fh>) {
        $n++;
        $line =~ s/\r?\n\z//;
        $line =~ s{^\s*(?://!|///|//|#)?\s*}{};
        $line =~ s/\s+$//;
        $folded .= " " if length $folded;
        push @starts, [length $folded, $n];
        $folded .= $line;
      }
      close $fh;
      my $lc = lc $folded;
      $lc =~ s/[ \t]/ /g;
      for my $p (@phrases) {
        my $q = lc $p;
        my $at = 0;
        while ((my $i = index($lc, $q, $at)) >= 0) {
          my $ln = 1;
          for my $s (@starts) { last if $s->[0] > $i; $ln = $s->[1]; }
          print "$path:$ln: $p\n";
          $at = $i + 1;
        }
      }
    }
  '
}

self_test() {
  local dir out
  dir=$(mktemp -d)
  trap 'rm -rf "$dir"' RETURN
  printf '%s\n' "The production rule (local open-weight" "models only) is unchanged." > "$dir/split.md"
  printf '%s\n' "    // so a crash never leaves half a file" > "$dir/reply.rs"
  printf '%s\n' "/// Model servers must be local; prompts" "/// never leave the box." > "$dir/comment.rs"
  printf '%s\n' "# No Frontier" "#   API for any data class." > "$dir/hash.yaml"
  out=$(printf '%s\n' "$dir/split.md" "$dir/reply.rs" "$dir/comment.rs" "$dir/hash.yaml" | scan)
  local ok=1
  grep -q "split.md:1: open-weight models only" <<<"$out" || { echo "FAIL: phrase split over two lines not found"; ok=0; }
  grep -q "reply.rs" <<<"$out" && { echo "FAIL: 'a crash never leaves half a file' matched"; ok=0; }
  grep -q "comment.rs:1: prompts never leave" <<<"$out" || { echo "FAIL: phrase across /// comment lines not found"; ok=0; }
  grep -q "hash.yaml:1: no frontier api" <<<"$out" || { echo "FAIL: phrase across # comment lines not found"; ok=0; }
  if [[ $ok == 1 ]]; then echo "check-model-promises self-test: ok"; else return 1; fi
}

main() {
  cd "$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
  local files matches unexpected=0 line path skip
  files=$(git ls-files -- "${SCOPE[@]}")
  matches=$(printf '%s\n' "$files" | scan)
  while IFS= read -r line; do
    [[ -z $line ]] && continue
    path=${line%%:*}
    skip=0
    for e in "${EXCLUDED[@]}"; do [[ $path == "$e"* ]] && skip=1; done
    [[ $skip == 1 ]] && continue
    for e in "${EXPECTED[@]}"; do [[ $path == "$e" ]] && skip=2; done
    if [[ $skip == 2 ]]; then
      echo "expected (waits on the owner): $line"
    else
      echo "stale promise: $line"
      unexpected=1
    fi
  done <<<"$matches"
  if [[ $unexpected == 1 ]]; then
    echo "Rewrite each promise as what PM OS does on PM OS's host (docs/openshell-any-host-spec.md §3.9)." >&2
    exit 1
  fi
  echo "check-model-promises: no stale promises"
}

case "${1:-}" in
  --self-test) self_test ;;
  "") main ;;
  *) echo "usage: $0 [--self-test]" >&2; exit 2 ;;
esac
