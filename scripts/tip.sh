#!/usr/bin/env bash
set -euo pipefail

# Send a tip to a jar on a deployed tip_splitter contract.
#
# .env (repository root) is loaded automatically; exported variables win over
# it. The script works when started from any directory.
#
# Required env (see .env.example):
#   NETWORK, SOURCE, JAR_ID, AMOUNT
#   CONTRACT_ID  (optional; defaults to .contract-id.<NETWORK>)
#   FROM         (optional; the tipper, defaults to SOURCE)
#   MESSAGE      (optional; non-empty free text published in the tip event)
#   REFERENCE    (optional; 64 hex chars (32 bytes) sent via tip_with_ref)
#   TOKEN        (optional; C... Stellar Asset Contract sent via tip_in_token)
#
# Usage:  ./scripts/tip.sh
#         JAR_ID=@bob AMOUNT=100 ./scripts/tip.sh
#         REFERENCE=0123...cdef ./scripts/tip.sh
#         TOKEN=C... ./scripts/tip.sh

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"
. "${NOVATIP_ROOT}/scripts/lib/cli.sh"

require_env NETWORK SOURCE JAR_ID AMOUNT
require_contract_id
require_stellar_cli

if [ -n "${REFERENCE:-}" ] && [ -n "${TOKEN:-}" ]; then
  echo "error: REFERENCE and TOKEN cannot be combined in a single tip call" >&2
  exit 1
fi

if [ -n "${REFERENCE:-}" ] && [[ ! "${REFERENCE}" =~ ^[0-9a-fA-F]{64}$ ]]; then
  echo "error: REFERENCE must be 64 hex characters (32 bytes) (got '${REFERENCE}')" >&2
  exit 1
fi

if [ -n "${TOKEN:-}" ] && [[ ! "${TOKEN}" =~ ^C[A-Z2-7]{55}$ ]]; then
  echo "error: TOKEN must be a valid Stellar contract address (C..., 56 chars) (got '${TOKEN}')" >&2
  exit 1
fi

# `tip` debits an account that authorises the call itself, and the CLI signs
# only for --source, so the tipper defaults to the submitting identity.
FROM="${FROM:-${SOURCE}}"
# The contract takes the message as a plain String and the CLI rejects an empty
# value, so the wrapper supplies text rather than failing on a missing flag.
MESSAGE="${MESSAGE:-tipped via scripts/tip.sh}"

echo "==> Sending tip of ${AMOUNT} to jar ${JAR_ID} on ${CONTRACT_ID}"
if [ -n "${REFERENCE:-}" ]; then
  stellar contract invoke \
    --id "${CONTRACT_ID}" \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    -- \
    tip_with_ref \
    --from "${FROM}" \
    --jar_id "${JAR_ID}" \
    --amount "${AMOUNT}" \
    --message "${MESSAGE}" \
    --reference "${REFERENCE}"
elif [ -n "${TOKEN:-}" ]; then
  stellar contract invoke \
    --id "${CONTRACT_ID}" \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    -- \
    tip_in_token \
    --from "${FROM}" \
    --jar_id "${JAR_ID}" \
    --token "${TOKEN}" \
    --amount "${AMOUNT}" \
    --message "${MESSAGE}"
else
  stellar contract invoke \
    --id "${CONTRACT_ID}" \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    -- \
    tip \
    --from "${FROM}" \
    --jar_id "${JAR_ID}" \
    --amount "${AMOUNT}" \
    --message "${MESSAGE}"
fi

echo "==> Tip sent."
