#!/usr/bin/env bash
set -euo pipefail

# Send multiple tips in a single batch to a deployed tip_splitter contract.
#
# .env (repository root) is loaded automatically; exported variables win over
# it. The script works when started from any directory.
#
# Required env (see .env.example):
#   NETWORK, SOURCE, TIPS
#   CONTRACT_ID  (optional; defaults to .contract-id.<NETWORK>)
#   FROM         (optional; the tipper, defaults to SOURCE)
#   TOKEN        (optional; token address, defaults to the contract's default token)
#
# TIPS must be a JSON array of tip objects, one per jar:
#   [
#     {"jar_id":"@jar1","amount":100000,"message":"tip 1","reference":null},
#     {"jar_id":"@jar2","amount":200000,"message":"tip 2","reference":null}
#   ]
#
# reference is optional (pass null or omit it); all other fields are required.
#
# Usage:  TIPS='[{"jar_id":"@jar1","amount":100000,"message":"tip 1"}]' ./scripts/tip-many.sh
#         TIPS_FILE=tips.json ./scripts/tip-many.sh

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"

require_env NETWORK SOURCE
require_contract_id

# Allow TIPS to come from a file or inline
if [ -n "${TIPS_FILE:-}" ]; then
  if [ ! -f "${TIPS_FILE}" ]; then
    echo "error: TIPS_FILE '${TIPS_FILE}' not found" >&2
    exit 1
  fi
  TIPS=$(cat "${TIPS_FILE}")
elif [ -z "${TIPS:-}" ]; then
  echo "error: either TIPS or TIPS_FILE must be set" >&2
  exit 1
fi

# `tip_many` debits an account that authorises the call itself, and the CLI
# signs only for --source, so the tipper defaults to the submitting identity.
FROM="${FROM:-${SOURCE}}"

# Validate TIPS is valid JSON and is an array
if ! jq -e 'type == "array"' <<<"${TIPS}" >/dev/null 2>&1; then
  echo "error: TIPS must be a JSON array" >&2
  exit 1
fi

# Check that the batch is not empty
COUNT=$(jq 'length' <<<"${TIPS}")
if [ "${COUNT}" -eq 0 ]; then
  echo "error: TIPS array must not be empty" >&2
  exit 1
fi

# Max batch size per the contract (see MAX_BATCH_TIPS in contracts/tip-splitter/src/lib.rs)
MAX_BATCH=200
if [ "${COUNT}" -gt "${MAX_BATCH}" ]; then
  echo "error: TIPS array has ${COUNT} entries but max batch size is ${MAX_BATCH}" >&2
  exit 1
fi

# Build the array of TipRequest objects, filling in defaults
TIPS_NORMALIZED=$(jq '[.[] | {jar_id, amount, message, reference}]' <<<"${TIPS}")

echo "==> Sending ${COUNT} tip(s) from ${FROM}"
jq -r '.[] | "    - \(.jar_id): \(.amount) stroops: \(.message)"' <<<"${TIPS_NORMALIZED}"

stellar contract invoke \
  --id "${CONTRACT_ID}" \
  --source "${SOURCE}" \
  --network "${NETWORK}" \
  -- \
  tip_many \
  --from "${FROM}" \
  --tips "${TIPS_NORMALIZED}" \
  ${TOKEN:+--token "${TOKEN}"}

echo "==> Batch sent."
