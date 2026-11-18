#!/usr/bin/env bash
set -euo pipefail

# Check the built tip_splitter wasm against the size ceiling.
#
# Single source of truth for the limit: both CI (.github/workflows/ci.yml)
# and `make ci` call this script instead of carrying their own copy.
# Override with MAX_WASM_BYTES=... in the environment if needed.
#
# Usage: ./scripts/check-wasm-size.sh [path/to/tip_splitter.wasm]

# Hard ceiling for the artifact `stellar contract build` produces — the same
# wasm the deploy script ships (#131).
#
# This was 24 KiB when the contract built to ~15 KiB. Feature growth took it
# to ~65 KiB and the ceiling was temporarily raised to 128 KiB to unblock main.
# The spec was carrying every Rust doc comment plus two unused exported types;
# stripping those brought the artifact to ~34 KiB (#329). The ceiling is now
# the new size plus ~25%, enough room for ordinary changes while still catching
# a heavyweight dependency or profile regression.
#
# Raising it is fine when the growth is understood and intentional — the point
# is that the bump shows up in a diff and gets a reason.
MAX_WASM_BYTES="${MAX_WASM_BYTES:-43008}"

# Prefer the stellar-contract-build artifact (wasm32v1-none). Fall back to a
# path argument for local use.
WASM="${1:-}"
if [ -z "$WASM" ]; then
  for candidate in \
    target/wasm32v1-none/release/tip_splitter.wasm \
    target/wasm32v1-none/release/tip_splitter.optimized.wasm \
    target/wasm32-unknown-unknown/release/tip_splitter.wasm
  do
    if [ -f "$candidate" ]; then WASM="$candidate"; break; fi
  done
fi
WASM="${WASM:-target/wasm32v1-none/release/tip_splitter.wasm}"
if [ ! -f "$WASM" ]; then
  echo "error: expected wasm at $WASM but it was not built" >&2
  exit 1
fi
SIZE=$(wc -c < "$WASM")
echo "tip_splitter.wasm: $SIZE bytes ($((SIZE / 1024)) KiB)"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### Contract size"
    echo
    echo "| Artifact | Bytes | KiB | Limit (bytes) |"
    echo "|----------|------:|----:|--------------:|"
    echo "| \`tip_splitter.wasm\` | $SIZE | $((SIZE / 1024)) | $MAX_WASM_BYTES |"
  } >> "$GITHUB_STEP_SUMMARY"
fi
if [ "$SIZE" -gt "$MAX_WASM_BYTES" ]; then
  echo "::error::tip_splitter.wasm is $SIZE bytes, over the $MAX_WASM_BYTES byte limit." >&2
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    echo "Shrink the contract, or raise MAX_WASM_BYTES in scripts/check-wasm-size.sh with a reason." >> "$GITHUB_STEP_SUMMARY"
  fi
  exit 1
fi
