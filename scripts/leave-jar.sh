#!/usr/bin/env bash
set -euo pipefail

# Leave a jar as one of its recipients. The departing share is reassigned by
# the contract to the final remaining split. SOURCE must be RECIPIENT.
#
# Required env: NETWORK, SOURCE, SLUG
# Optional env: RECIPIENT (defaults to SOURCE's address), CONTRACT_ID
#
# Usage: ./scripts/leave-jar.sh
#        RECIPIENT=G... ./scripts/leave-jar.sh

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"
. "${NOVATIP_ROOT}/scripts/lib/cli.sh"

require_env NETWORK SOURCE SLUG
require_contract_id
require_stellar_cli

if [[ ! "${SLUG}" =~ ^@[a-z0-9_-]{3,32}$ ]]; then
  echo "error: SLUG must be a canonical @-prefixed slug (3–32 lowercase letters, digits, '_' or '-')" >&2
  exit 1
fi

if ! SOURCE_ADDRESS=$(stellar keys address "${SOURCE}"); then
  echo "error: could not resolve SOURCE identity '${SOURCE}' to a Stellar address" >&2
  exit 1
fi
RECIPIENT_ADDRESS="${RECIPIENT:-${SOURCE_ADDRESS}}"
if [[ ! "${RECIPIENT_ADDRESS}" =~ ^G[A-Z2-7]{55}$ ]]; then
  echo "error: RECIPIENT must be a valid Stellar G... account address" >&2
  exit 1
fi
if [ "${SOURCE_ADDRESS}" != "${RECIPIENT_ADDRESS}" ]; then
  echo "error: leave_jar requires the recipient's signature; SOURCE resolves to ${SOURCE_ADDRESS}, RECIPIENT is ${RECIPIENT_ADDRESS}" >&2
  exit 1
fi

invoke() {
  stellar contract invoke \
    --id "${CONTRACT_ID}" \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    -- "$@"
}

echo "==> Checking that ${RECIPIENT_ADDRESS} is a recipient of ${SLUG}"
IS_RECIPIENT=$(invoke is_recipient --jar_id "${SLUG}" --address "${RECIPIENT_ADDRESS}")
if [ "${IS_RECIPIENT}" != "true" ]; then
  echo "error: ${RECIPIENT_ADDRESS} is not a recipient of ${SLUG}; no transaction sent" >&2
  exit 1
fi

if ! BEFORE=$(invoke get_jar --jar_id "${SLUG}" 2>&1); then
  echo "error: reading the jar splits failed:" >&2
  echo "${BEFORE}" >&2
  exit 1
fi
echo "==> Splits before leaving ${SLUG}"
jq -r '.splits[] | "    \(.to)  \(.bps) bps"' <<<"${BEFORE}"

echo "==> Leaving ${SLUG} as ${RECIPIENT_ADDRESS}"
invoke leave_jar --jar_id "${SLUG}" --recipient "${RECIPIENT_ADDRESS}"

if ! AFTER=$(invoke get_jar --jar_id "${SLUG}" 2>&1); then
  echo "error: leave_jar completed but reading the updated splits failed:" >&2
  echo "${AFTER}" >&2
  exit 1
fi
echo "==> Splits after leaving ${SLUG}"
jq -r '.splits[] | "    \(.to)  \(.bps) bps"' <<<"${AFTER}"
