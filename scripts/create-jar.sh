#!/usr/bin/env bash
set -euo pipefail

# Create a single-recipient tip jar on a deployed tip_splitter contract.
#
# Required env (see .env.example):
#   NETWORK, SOURCE, OWNER, SLUG, RECIPIENT
#   CONTRACT_ID  (optional; defaults to the contents of .contract-id)
#
# Usage:  set -a; source .env; set +a; ./scripts/create-jar.sh

: "${NETWORK:?set NETWORK}"
: "${SOURCE:?set SOURCE}"
: "${OWNER:?set OWNER}"
: "${SLUG:?set SLUG}"
: "${RECIPIENT:?set RECIPIENT}"
CONTRACT_ID="${CONTRACT_ID:-$(cat .contract-id)}"

echo "==> Creating jar ${SLUG} on ${CONTRACT_ID} (100% -> ${RECIPIENT})"
stellar contract invoke \
  --id "${CONTRACT_ID}" \
  --source "${SOURCE}" \
  --network "${NETWORK}" \
  -- \
  create_jar \
  --owner "${OWNER}" \
  --jar_id "${SLUG}" \
  --splits "[{\"to\":\"${RECIPIENT}\",\"bps\":10000}]"

echo "==> Done."
