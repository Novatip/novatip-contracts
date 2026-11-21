#!/usr/bin/env bash
set -euo pipefail

# Lock or inspect/unlock a jar's split protections. SOURCE must be the owner.
# Lock is submitted directly. Unlock requires signatures from the owner and
# every current recipient, so ACTION=unlock prints a build/sign/send guide and
# never submits a transaction with an incomplete signer set.
#
# Required env: NETWORK, SOURCE, SLUG, ACTION=lock|unlock
# Optional env: CONTRACT_ID (defaults to .contract-id.<NETWORK>)
#
# Usage: ACTION=lock ./scripts/lock-splits.sh
#        ACTION=unlock ./scripts/lock-splits.sh

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"
. "${NOVATIP_ROOT}/scripts/lib/cli.sh"

require_env NETWORK SOURCE SLUG ACTION
require_contract_id
require_stellar_cli

if [ "${ACTION}" != "lock" ] && [ "${ACTION}" != "unlock" ]; then
  echo "error: ACTION must be exactly 'lock' or 'unlock' (got '${ACTION}')" >&2
  exit 1
fi
if [[ ! "${SLUG}" =~ ^@[a-z0-9_-]{3,32}$ ]]; then
  echo "error: SLUG must be a canonical @-prefixed slug (3–32 lowercase letters, digits, '_' or '-')" >&2
  exit 1
fi

if ! SOURCE_ADDRESS=$(stellar keys address "${SOURCE}"); then
  echo "error: could not resolve SOURCE identity '${SOURCE}' to a Stellar address" >&2
  exit 1
fi

invoke() {
  stellar contract invoke \
    --id "${CONTRACT_ID}" \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    -- "$@"
}

if ! JAR=$(invoke get_jar --jar_id "${SLUG}" 2>&1); then
  echo "error: reading the jar failed:" >&2
  echo "${JAR}" >&2
  exit 1
fi
OWNER=$(jq -r '.owner' <<<"${JAR}")
if [ "${OWNER}" != "${SOURCE_ADDRESS}" ]; then
  echo "error: ${SLUG} is owned by ${OWNER}, but SOURCE '${SOURCE}' is ${SOURCE_ADDRESS}" >&2
  exit 1
fi

if ! LOCKED=$(invoke is_splits_locked --jar_id "${SLUG}" 2>&1); then
  echo "error: reading the current locked state failed:" >&2
  echo "${LOCKED}" >&2
  exit 1
fi
echo "==> ${SLUG} locked=${LOCKED}"

if [ "${ACTION}" = "lock" ]; then
  if [ "${LOCKED}" = "true" ]; then
    echo "==> Splits are already locked; no transaction sent."
    exit 0
  fi
  echo "==> Locking splits for ${SLUG}"
  invoke lock_splits --jar_id "${SLUG}"
  LOCKED=$(invoke is_splits_locked --jar_id "${SLUG}")
  echo "    ${SLUG} locked=${LOCKED}"
  if [ "${LOCKED}" != "true" ]; then
    echo "error: read-back state is not locked" >&2
    exit 1
  fi
  exit 0
fi

if [ "${LOCKED}" = "false" ]; then
  echo "==> Splits are already unlocked; no transaction sent."
  exit 0
fi

echo "==> Unlock requires the owner's signature and every current recipient's signature."
echo "    No unlock transaction was submitted; the jar remains locked until all signatures are collected."
echo "    Current recipients:"
jq -r '.splits[].to | "      " + .' <<<"${JAR}"
echo "    1. Build an unsigned envelope:"
printf '       stellar contract invoke --build-only --id %q --source %q --network %q -- unlock_splits --jar_id %q > unlock.xdr\n' \
  "${CONTRACT_ID}" "${SOURCE}" "${NETWORK}" "${SLUG}"
echo "    2. The owner and every current recipient sign the same envelope in sequence:"
echo "       stellar tx sign --network ${NETWORK} --sign-with-key <IDENTITY> < unlock.xdr > unlock-signed.xdr"
echo "       Pass each signer's output XDR as the next signer's input."
echo "    3. Submit only after all required signatures are present:"
echo "       stellar tx send --network ${NETWORK} < unlock-signed.xdr"
echo "    4. Confirm with ACTION=unlock ./scripts/lock-splits.sh; the script reads is_splits_locked."
