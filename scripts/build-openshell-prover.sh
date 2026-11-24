#!/usr/bin/env bash
# Builds OpenShell's policy prover from the SOURCE pinned in deploy/openshell-prover.lock
# (docs/openshell-prover-and-sandbox-audit-spec.md §A.7). Used by CI's `policy-prover` job
# (scripts/policy-prover-ci.sh) and usable on a host. Run from the repository root:
#
#   scripts/build-openshell-prover.sh --lock <lock> --out <dir>
#
# 1. Fetches the pinned tag (PROVER_SOURCE_TAG) from PROVER_SOURCE_REPO, shallowly, into
#    <dir>/src, and FAILS unless the fetched commit is exactly PROVER_SOURCE_COMMIT (a moved or
#    re-pointed tag is never built).
# 2. Installs the Rust toolchain the source pins in rust-toolchain.toml (minimal profile) and
#    fetches the crates exactly as the source's own Cargo.lock names them (`cargo fetch --locked`).
# 3. Stamps the version as OpenShell's own release build does (its workspace says 0.0.0; the
#    release sets it with sed before building), in Cargo.toml and Cargo.lock, so
#    `openshell-prover --version` prints PROVER_VERSION.
# 4. Builds `openshell-prover` (package openshell-prover-cli) with PROVER_Z3_FEATURE, OFFLINE and
#    with an EMPTY environment (only PATH, HOME, the Rust homes and the target directory): no
#    token or secret of the caller reaches the third-party build scripts. <dir>/target is the
#    Cargo target directory, so a cache of <dir> (keyed on the commit) makes a rebuild a no-op.
# 5. Copies the binary to <dir>/bin/openshell-prover and writes <dir>/bin/build.record:
#    PROVER_VERSION, PROVER_SOURCE_COMMIT and PROVER_BINARY_SHA256_<ARCH> of the binary just
#    built (read by `pmos policy prove --build-record`), and prints the SHA-256, `--version` and
#    rustc's version for the log.
# Prints the binary's path on standard output as its last line; exits non-zero (and prints
# nothing on standard output) when anything fails. Logs go to standard error.
#
# Tests only (scripts/test-policy-prover-ci.sh): --source-url <url> fetches from another
# repository (a local fixture) instead of PROVER_SOURCE_REPO; PMOS_PROVER_CARGO and
# PMOS_PROVER_RUSTUP name stand-ins for cargo and rustup. The workflow sets none of them.
set -euo pipefail

lock=""
out=""
source_url=""
while [ $# -gt 0 ]; do
  case "$1" in
    --lock) lock=${2:-}; shift 2 ;;
    --out) out=${2:-}; shift 2 ;;
    --source-url) source_url=${2:-}; shift 2 ;;
    *) echo "usage: $0 --lock <lock> --out <dir>" >&2; exit 2 ;;
  esac
done
[ -n "$lock" ] && [ -n "$out" ] || { echo "usage: $0 --lock <lock> --out <dir>" >&2; exit 2; }
log() { printf 'build-openshell-prover: %s\n' "$*" >&2; }

lock_value() { # file key: the value, read as data (never sourced)
  awk -F= -v k="$2" '$1 == k { sub(/^[^=]*=/, ""); print; exit }' "$1"
}
version=$(lock_value "$lock" PROVER_VERSION)
repo=$(lock_value "$lock" PROVER_SOURCE_REPO)
tag=$(lock_value "$lock" PROVER_SOURCE_TAG)
commit=$(lock_value "$lock" PROVER_SOURCE_COMMIT)
feature=$(lock_value "$lock" PROVER_Z3_FEATURE)
[ -n "$version" ] && [ -n "$repo" ] && [ -n "$tag" ] && [ -n "$commit" ] && [ -n "$feature" ] ||
  { log "$lock pins no prover source (PROVER_VERSION, PROVER_SOURCE_REPO, _TAG, _COMMIT, PROVER_Z3_FEATURE)"; exit 1; }
# The values are data; refuse anything that isn't the shape the pin allows.
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.]+)?$ ]] || { log "PROVER_VERSION isn't a version"; exit 1; }
[[ "$repo" =~ ^https://[A-Za-z0-9._/-]+$ ]] || { log "PROVER_SOURCE_REPO isn't a plain https URL"; exit 1; }
[[ "$tag" =~ ^[A-Za-z0-9][A-Za-z0-9.+-]*$ ]] || { log "PROVER_SOURCE_TAG isn't a tag name"; exit 1; }
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] || { log "PROVER_SOURCE_COMMIT isn't a full commit SHA"; exit 1; }
# Only vendored-z3: Z3 is then built from the z3-src crate, which the source's Cargo.lock pins by
# checksum. prebuilt-z3 downloads a Z3 release inside a build script (unpinned, and impossible in
# an offline build); no feature at all links the host's libz3, whatever version it is.
[ "$feature" = vendored-z3 ] || { log "PROVER_Z3_FEATURE must be vendored-z3"; exit 1; }
case "$(uname -m)" in
  x86_64 | amd64) arch=X86_64 ;;
  aarch64 | arm64) arch=AARCH64 ;;
  *) log "no build for $(uname -m)"; exit 1 ;;
esac
[ -n "$source_url" ] || source_url=$repo
cargo_bin=${PMOS_PROVER_CARGO:-cargo}

mkdir -p "$out"
out=$(cd "$out" && pwd)
src="$out/src"
target="$out/target"
bin="$out/bin"
rm -rf "$src" "$bin"
mkdir -p "$src" "$target" "$bin"

# 1. Exactly the pinned commit.
git -C "$src" init -q
git -C "$src" -c advice.detachedHead=false fetch -q --depth 1 --no-tags "$source_url" "refs/tags/$tag" ||
  { log "couldn't fetch tag $tag from $source_url"; exit 1; }
fetched=$(git -C "$src" rev-parse --verify --quiet 'FETCH_HEAD^{commit}' || true)
if [ "$fetched" != "$commit" ]; then
  log "the fetched tag $tag is commit ${fetched:-(none)}, not the pinned $commit: not built"
  exit 1
fi
git -C "$src" -c advice.detachedHead=false checkout -q "$commit"
log "source $source_url tag $tag at commit $fetched (the pinned one)"

# 2. The toolchain the source pins (rust-toolchain.toml), then the crates as the source's
#    Cargo.lock names them (network: our own tools, not the third-party build).
channel=$(awk -F'"' '/^channel[[:space:]]*=/ { print $2; exit }' "$src/rust-toolchain.toml" 2>/dev/null || true)
[[ "$channel" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { log "the source pins no exact Rust toolchain (rust-toolchain.toml)"; exit 1; }
"${PMOS_PROVER_RUSTUP:-rustup}" toolchain install "$channel" --profile minimal --no-self-update >&2 ||
  { log "couldn't install Rust $channel"; exit 1; }
export RUSTUP_TOOLCHAIN=$channel
(cd "$src" && "$cargo_bin" fetch --locked >&2) || { log "cargo fetch --locked failed"; exit 1; }

# 3. The version, as OpenShell's release build stamps it. Only the workspace's own 0.0.0 lines.
sed -i "s/^version = \"0\.0\.0\"$/version = \"$version\"/" "$src/Cargo.toml"
[ -f "$src/Cargo.lock" ] && sed -i "s/^version = \"0\.0\.0\"$/version = \"$version\"/" "$src/Cargo.lock"

# 4. The build: offline, with an empty environment but for what Rust needs.
cargo_home=${CARGO_HOME:-$HOME/.cargo}
rustup_home=${RUSTUP_HOME:-$HOME/.rustup}
start=$(date +%s)
(cd "$src" && env -i PATH="$cargo_home/bin:/usr/local/bin:/usr/bin:/bin" HOME="$HOME" \
  CARGO_HOME="$cargo_home" RUSTUP_HOME="$rustup_home" RUSTUP_TOOLCHAIN="$channel" \
  CARGO_TARGET_DIR="$target" CARGO_INCREMENTAL=0 \
  "$cargo_bin" build --locked --offline --release -j "${PMOS_PROVER_JOBS:-$(nproc)}" \
  -p openshell-prover-cli --bin openshell-prover --features "$feature" >&2) ||
  { log "the prover build failed"; exit 1; }
log "built in $(( $(date +%s) - start )) seconds"
built="$target/release/openshell-prover"
[ -f "$built" ] && [ -x "$built" ] || { log "the build left no openshell-prover binary"; exit 1; }

# 5. The binary and its record.
cp "$built" "$bin/openshell-prover"
sum=$(sha256sum "$bin/openshell-prover" | cut -d' ' -f1)
reported=$(env -i PATH=/usr/bin:/bin "$bin/openshell-prover" --version 2>/dev/null || true)
rustc_version=$(cd "$src" && env -i PATH="$cargo_home/bin:/usr/local/bin:/usr/bin:/bin" HOME="$HOME" \
  CARGO_HOME="$cargo_home" RUSTUP_HOME="$rustup_home" RUSTUP_TOOLCHAIN="$channel" rustc --version 2>/dev/null || echo unknown)
{
  printf '# Written by scripts/build-openshell-prover.sh; read by pmos policy prove --build-record.\n'
  printf 'PROVER_VERSION=%s\n' "$version"
  printf 'PROVER_SOURCE_COMMIT=%s\n' "$commit"
  printf 'PROVER_BINARY_SHA256_%s=%s\n' "$arch" "$sum"
} >"$bin/build.record"
log "openshell-prover SHA-256 $sum"
log "openshell-prover --version: ${reported:-(nothing)}"
log "rustc: $rustc_version; Z3 feature: $feature"
printf '%s\n' "$bin/openshell-prover"
