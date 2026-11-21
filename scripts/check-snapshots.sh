#!/usr/bin/env bash
# Fail when `cargo test` changed or created test snapshots that are not
# committed (#264). Run it right after `cargo test`.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

CHANGED=$(git status --porcelain -- ':(glob)**/test_snapshots/**')
if [ -n "$CHANGED" ]; then
  echo "error: cargo test changed test snapshots that are not committed:" >&2
  echo "$CHANGED" >&2
  echo "Commit everything under test_snapshots/ with your change, so storage and event changes show up in review." >&2
  exit 1
fi
echo "check-snapshots: OK (test snapshots match the committed files)"
