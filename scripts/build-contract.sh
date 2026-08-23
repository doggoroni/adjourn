#!/usr/bin/env bash
#
# The canonical contract build.
#
# The contract key is the hash of these bytes, so the build has to be
# reproducible across machines. Two things would otherwise break that:
#
#   * absolute paths (the checkout directory, $HOME/.cargo/registry) are baked
#     into the WASM through panic locations, so the bytes would depend on who
#     built it and where;
#   * a dependency drifting under a caret range would change the bytes.
#
# `--remap-path-prefix` fixes the first, `--locked` plus exact `=` pins in
# Cargo.toml fix the second. Build the contract ONLY through this script — a
# bare `cargo build --release` produces a DIFFERENT and unshippable key.
#
# Cargo's `trim-paths` profile option would replace the remapping below, but it
# is unstable as of 1.97.1 and would force nightly on the contract build.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

CARGO_HOME_PATH="${CARGO_HOME:-$HOME/.cargo}"

# rustc sees NATIVE paths. Under Git Bash / MSYS a POSIX path like
# /c/Users/... never matches the C:\Users\... that rustc embeds, so the remap
# silently does nothing and the leak check silently passes. Convert first.
native_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -w "$1"
  else
    printf '%s' "$1"
  fi
}

ROOT_NATIVE="$(native_path "$ROOT")"
CARGO_NATIVE="$(native_path "$CARGO_HOME_PATH")"

# The remapped targets are arbitrary but must be STABLE — they are part of the
# input to the contract key.
export RUSTFLAGS="--remap-path-prefix=$ROOT_NATIVE=/build --remap-path-prefix=$CARGO_NATIVE=/cargo ${RUSTFLAGS:-}"

cargo build \
  -p adjourn-contract \
  --target wasm32-unknown-unknown \
  --release \
  --locked

WASM="target/wasm32-unknown-unknown/release/adjourn_contract.wasm"

# Fail loudly rather than shipping a key that only this machine can reproduce.
# Both spellings, because the check passing for the wrong reason is how this
# bug hides.
for leak in "$ROOT" "$CARGO_HOME_PATH" "$ROOT_NATIVE" "$CARGO_NATIVE"; do
  if grep -qF "$leak" "$WASM" 2>/dev/null; then
    echo "error: build path '$leak' is embedded in $WASM" >&2
    echo "the contract key would differ on another machine" >&2
    exit 1
  fi
done

if command -v sha256sum >/dev/null 2>&1; then
  SUM="$(sha256sum "$WASM" | cut -d' ' -f1)"
else
  SUM="$(shasum -a 256 "$WASM" | cut -d' ' -f1)"
fi

echo "wasm:   $WASM"
echo "size:   $(wc -c <"$WASM") bytes"
echo "sha256: $SUM"
echo
echo "This hash is the contract key input. If it changed unexpectedly, something"
echo "in the toolchain or dependency graph moved — find out what before shipping."
