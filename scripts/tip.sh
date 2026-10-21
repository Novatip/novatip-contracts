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
#
# Usage:  ./scripts/tip.sh
#         JAR_ID=@bob AMOUNT=100 ./scripts/tip.sh

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"

require_env NETWORK SOURCE JAR_ID AMOUNT
require_contract_id

# `tip` debits an account that authorises the call itself, and the CLI signs
# only for --source, so the tipper defaults to the submitting identity.
FROM="${FROM:-${SOURCE}}"
# The contract takes the message as a plain String and the CLI rejects an empty
# value, so the wrapper supplies text rather than failing on a missing flag.
MESSAGE="${MESSAGE:-tipped via scripts/tip.sh}"

echo "==> Sending tip of ${AMOUNT} to jar ${JAR_ID} on ${CONTRACT_ID}"
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

echo "==> Tip sent."
