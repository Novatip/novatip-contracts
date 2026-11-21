#!/usr/bin/env bash
set -euo pipefail

# Delete a jar permanently. The owner must sign, and the slug is released for
# anyone to register again, so the operator has to type the exact slug to
# confirm. Set YES=1 to skip that prompt in automation.
#
# The jar's configuration and totals are printed before the call so a wrong
# slug or contract is visible, and jar_exists is read back afterwards.
#
# Required env (see .env.example):
#   NETWORK      testnet | mainnet | local
#   SOURCE       Stellar CLI identity holding the jar owner's key
#   SLUG         the jar's public id, e.g. @alice
#   CONTRACT_ID  optional; defaults to .contract-id.<NETWORK>
#   YES=1        skip the typed slug confirmation (CI only)
#
# Usage:  ./scripts/delete-jar.sh
#         YES=1 SLUG=@alice ./scripts/delete-jar.sh

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"
. "${NOVATIP_ROOT}/scripts/lib/cli.sh"

require_env NETWORK SOURCE SLUG
require_contract_id
require_stellar_cli

if [[ ! "${SLUG}" =~ ^@[a-z0-9_-]{3,32}$ ]]; then
  echo "error: SLUG must be a canonical @-prefixed slug (3-32 lowercase letters, digits, '_' or '-')" >&2
  exit 1
fi

invoke() {
  stellar contract invoke \
    --id "${CONTRACT_ID}" \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    -- "$@"
}

echo "==> Jar ${SLUG} on ${CONTRACT_ID} (${NETWORK}) before deletion"
echo "--- get_jar ---"
invoke get_jar --jar_id "${SLUG}"
echo "--- get_jar_stats ---"
invoke get_jar_stats --jar_id "${SLUG}"

if [ "${YES:-}" = "1" ]; then
  echo "==> YES=1: skipping the typed confirmation"
else
  echo ""
  echo "=== DELETE JAR CONFIRMATION ==="
  echo "Deleting ${SLUG} removes its configuration and totals for good, and frees the slug for anyone to claim."
  printf "Type the slug '%s' to confirm: " "${SLUG}"
  read -r confirmation
  if [ "${confirmation}" != "${SLUG}" ]; then
    echo "error: '${confirmation}' does not match '${SLUG}'; nothing was deleted" >&2
    exit 1
  fi
fi

echo "==> Deleting ${SLUG}"
invoke delete_jar --jar_id "${SLUG}"

echo "==> Confirming the slug is free"
EXISTS=$(invoke jar_exists --jar_id "${SLUG}")
echo "    jar_exists(${SLUG}) = ${EXISTS}"
if [ "${EXISTS}" != "false" ]; then
  echo "error: jar_exists still reports ${EXISTS} after delete_jar" >&2
  exit 1
fi

echo "==> Done. ${SLUG} was deleted and its slug is available again."
