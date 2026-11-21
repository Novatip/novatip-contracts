#!/usr/bin/env bash
set -euo pipefail

# Pause or resume tipping for one jar. SOURCE must be the jar owner.
#
# Required env: NETWORK, SOURCE, SLUG, PAUSED=true|false
# Optional env: CONTRACT_ID (defaults to .contract-id.<NETWORK>)
#
# Usage: PAUSED=true ./scripts/pause-jar.sh
#        PAUSED=false ./scripts/pause-jar.sh

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"
. "${NOVATIP_ROOT}/scripts/lib/cli.sh"

require_env NETWORK SOURCE SLUG PAUSED
require_contract_id
require_stellar_cli

if [ "${PAUSED}" != "true" ] && [ "${PAUSED}" != "false" ]; then
  echo "error: PAUSED must be exactly 'true' or 'false' (got '${PAUSED}')" >&2
  exit 1
fi

if [[ ! "${SLUG}" =~ ^@[a-z0-9_-]{3,32}$ ]]; then
  echo "error: SLUG must be a canonical @-prefixed slug (3–32 lowercase letters, digits, '_' or '-')" >&2
  exit 1
fi

invoke() {
  stellar contract invoke \
    --id "${CONTRACT_ID}" \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    -- "$@"
}

echo "==> Setting ${SLUG} paused=${PAUSED}"
invoke set_jar_paused --jar_id "${SLUG}" --paused "${PAUSED}"

echo "==> Reading back paused state"
STATE=$(invoke is_jar_paused --jar_id "${SLUG}")
echo "    ${SLUG} paused=${STATE}"
if [ "${STATE}" != "${PAUSED}" ]; then
  echo "error: read-back state '${STATE}' does not match requested PAUSED=${PAUSED}" >&2
  exit 1
fi
