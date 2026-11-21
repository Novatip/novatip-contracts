#!/usr/bin/env bash
set -euo pipefail

# Rename a tip jar. SOURCE must be the jar owner, NEW_SLUG must satisfy the
# same canonical rule the contract enforces, and the new slug must be free.
# The old slug and the jar's stats move with it; the jar is read back under the
# new slug so a silent failure cannot pass as success.
#
# Required env (see .env.example):
#   NETWORK      testnet | mainnet | local
#   SOURCE       Stellar CLI identity holding the jar owner's key
#   SLUG         the jar's current public id, e.g. @alice
#   NEW_SLUG     the canonical slug to rename it to, e.g. @alice-new
#   CONTRACT_ID  optional; defaults to .contract-id.<NETWORK>
#
# Usage:  SLUG=@alice NEW_SLUG=@alice-new ./scripts/rename-jar.sh

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"
. "${NOVATIP_ROOT}/scripts/lib/cli.sh"

require_env NETWORK SOURCE SLUG NEW_SLUG
require_contract_id
require_stellar_cli

validate_slug() {
  local name="$1" value="$2"
  if [[ ! "${value}" =~ ^@[a-z0-9_-]{3,32}$ ]]; then
    echo "error: ${name} must be a canonical @-prefixed slug (3-32 lowercase letters, digits, '_' or '-')" >&2
    exit 1
  fi
}

validate_slug SLUG "${SLUG}"
validate_slug NEW_SLUG "${NEW_SLUG}"
if [ "${SLUG}" = "${NEW_SLUG}" ]; then
  echo "error: NEW_SLUG must differ from SLUG" >&2
  exit 1
fi

invoke() {
  stellar contract invoke \
    --id "${CONTRACT_ID}" \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    -- "$@"
}

echo "==> Jar ${SLUG} before rename"
invoke get_jar --jar_id "${SLUG}"

echo "==> Checking that ${NEW_SLUG} is free"
USED=$(invoke jar_exists --jar_id "${NEW_SLUG}")
if [ "${USED}" = "true" ]; then
  echo "error: ${NEW_SLUG} is already in use; no transaction was sent" >&2
  exit 1
fi
if [ "${USED}" != "false" ]; then
  echo "error: unexpected jar_exists result '${USED}' for ${NEW_SLUG}" >&2
  exit 1
fi

echo "==> Renaming ${SLUG} to ${NEW_SLUG}"
invoke rename_jar --jar_id "${SLUG}" --new_jar_id "${NEW_SLUG}"

echo "==> Reading back ${NEW_SLUG}"
invoke get_jar --jar_id "${NEW_SLUG}"
OLD_EXISTS=$(invoke jar_exists --jar_id "${SLUG}")
if [ "${OLD_EXISTS}" != "false" ]; then
  echo "error: jar_exists still reports ${OLD_EXISTS} for the old slug ${SLUG}" >&2
  exit 1
fi

echo "==> Done. ${SLUG} is now ${NEW_SLUG}."
