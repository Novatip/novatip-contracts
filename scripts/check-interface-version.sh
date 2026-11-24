#!/usr/bin/env bash
# Fail when contracts/tip-splitter/interface.json differs from the base branch
# but INTERFACE_VERSION in contracts/tip-splitter/src/lib.rs and CHANGELOG.md do not (#309).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

CURRENT_INTERFACE="contracts/tip-splitter/interface.json"
CURRENT_LIB="contracts/tip-splitter/src/lib.rs"
CURRENT_CHANGELOG="CHANGELOG.md"

if [ ! -f "$CURRENT_INTERFACE" ]; then
  echo "error: $CURRENT_INTERFACE is missing" >&2
  exit 1
fi
if [ ! -f "$CURRENT_LIB" ]; then
  echo "error: $CURRENT_LIB is missing" >&2
  exit 1
fi

TMP_DIR=""
cleanup() {
  if [ -n "$TMP_DIR" ] && [ -d "$TMP_DIR" ]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

if [ -n "${1:-}" ]; then
  BASE_DIR="$1"
  if [ ! -d "$BASE_DIR" ]; then
    echo "error: base directory '$BASE_DIR' does not exist" >&2
    exit 1
  fi
  BASE_INTERFACE="$BASE_DIR/$CURRENT_INTERFACE"
  BASE_LIB="$BASE_DIR/$CURRENT_LIB"
  BASE_CHANGELOG="$BASE_DIR/$CURRENT_CHANGELOG"
elif [ -d "base" ]; then
  BASE_DIR="base"
  BASE_INTERFACE="$BASE_DIR/$CURRENT_INTERFACE"
  BASE_LIB="$BASE_DIR/$CURRENT_LIB"
  BASE_CHANGELOG="$BASE_DIR/$CURRENT_CHANGELOG"
else
  BASE_REF=""
  for candidate in "origin/main" "upstream/main" "main"; do
    if git rev-parse --verify "$candidate" >/dev/null 2>&1; then
      if [ "$(git rev-parse HEAD)" != "$(git rev-parse "$candidate")" ]; then
        BASE_REF="$candidate"
        break
      fi
    fi
  done

  if [ -n "$BASE_REF" ]; then
    TMP_DIR="$(mktemp -d)"
    mkdir -p "$TMP_DIR/contracts/tip-splitter/src"
    git show "$BASE_REF:$CURRENT_INTERFACE" > "$TMP_DIR/$CURRENT_INTERFACE" 2>/dev/null || true
    git show "$BASE_REF:$CURRENT_LIB" > "$TMP_DIR/$CURRENT_LIB" 2>/dev/null || true
    git show "$BASE_REF:$CURRENT_CHANGELOG" > "$TMP_DIR/$CURRENT_CHANGELOG" 2>/dev/null || true

    BASE_INTERFACE="$TMP_DIR/$CURRENT_INTERFACE"
    BASE_LIB="$TMP_DIR/$CURRENT_LIB"
    BASE_CHANGELOG="$TMP_DIR/$CURRENT_CHANGELOG"
  else
    echo "check-interface-version: SKIPPED (no base directory or git base branch found; CI runs this on pull requests)"
    exit 0
  fi
fi

if [ -f "$BASE_INTERFACE" ] && diff -q "$BASE_INTERFACE" "$CURRENT_INTERFACE" >/dev/null 2>&1; then
  echo "check-interface-version: OK — public interface unchanged from base"
  exit 0
fi

# Public interface changed: verify INTERFACE_VERSION was bumped
CURRENT_VERSION=$(sed -nE 's/^[[:space:]]*(pub[[:space:]]+)?const[[:space:]]+INTERFACE_VERSION[[:space:]]*:[[:space:]]*u32[[:space:]]*=[[:space:]]*([0-9]+).*/\2/p' "$CURRENT_LIB" | head -n 1)
if [ -z "$CURRENT_VERSION" ]; then
  echo "error: could not read INTERFACE_VERSION from $CURRENT_LIB" >&2
  exit 1
fi

BASE_VERSION=""
if [ -f "$BASE_LIB" ]; then
  BASE_VERSION=$(sed -nE 's/^[[:space:]]*(pub[[:space:]]+)?const[[:space:]]+INTERFACE_VERSION[[:space:]]*:[[:space:]]*u32[[:space:]]*=[[:space:]]*([0-9]+).*/\2/p' "$BASE_LIB" | head -n 1)
fi

VERSION_BUMPED=false
if [ -n "$BASE_VERSION" ]; then
  if [ "$CURRENT_VERSION" -gt "$BASE_VERSION" ]; then
    VERSION_BUMPED=true
  fi
else
  VERSION_BUMPED=true
fi

# Verify CHANGELOG.md was updated
CHANGELOG_UPDATED=false
if [ -f "$BASE_CHANGELOG" ] && [ -f "$CURRENT_CHANGELOG" ]; then
  if ! diff -q "$BASE_CHANGELOG" "$CURRENT_CHANGELOG" >/dev/null 2>&1; then
    CHANGELOG_UPDATED=true
  fi
elif [ ! -f "$BASE_CHANGELOG" ] && [ -f "$CURRENT_CHANGELOG" ]; then
  CHANGELOG_UPDATED=true
fi

FAILED=false
if [ "$VERSION_BUMPED" != "true" ]; then
  FAILED=true
  echo "error: contracts/tip-splitter/interface.json differs from base branch, but INTERFACE_VERSION in $CURRENT_LIB was not bumped (base: ${BASE_VERSION:-unknown}, current: $CURRENT_VERSION)" >&2
  echo "When the public interface changes, increment const INTERFACE_VERSION in $CURRENT_LIB." >&2
fi

if [ "$CHANGELOG_UPDATED" != "true" ]; then
  FAILED=true
  echo "error: contracts/tip-splitter/interface.json differs from base branch, but $CURRENT_CHANGELOG was not updated" >&2
  echo "When the public interface changes, add an entry to $CURRENT_CHANGELOG under [Unreleased]." >&2
fi

if [ "$FAILED" = "true" ]; then
  exit 1
fi

echo "check-interface-version: OK — interface changed, INTERFACE_VERSION bumped (${BASE_VERSION} -> ${CURRENT_VERSION}) and CHANGELOG.md updated"
