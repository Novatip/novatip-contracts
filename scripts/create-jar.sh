#!/usr/bin/env bash
set -euo pipefail

# Create a tip jar on a deployed tip_splitter contract.
#
# .env (repository root) is loaded automatically; exported variables win over
# it. The script works when started from any directory.
#
# Required env (see .env.example):
#   NETWORK, SOURCE, OWNER, SLUG
#   CONTRACT_ID  (optional; defaults to .contract-id.<NETWORK>)
#
# Recipients — either one of:
#   RECIPIENT  single-recipient shorthand; builds the 100% split for you
#   SPLITS     full splits JSON, e.g.
#              '[{"to":"G...A","bps":6000},{"to":"G...B","bps":4000}]'
#              bps values must sum to 10000; at most 20 recipients
#
# Validation rules (enforced locally before the transaction, #124):
#   - SLUG is 4–33 bytes (`@` plus 3–32 characters)
#   - Each split entry: `to` is a non-empty string, `bps` is an integer 1..10000
#   - No duplicate `to` addresses
#   - bps values sum to 10000
#
# Usage:  ./scripts/create-jar.sh
#         SPLITS='[{"to":"G...A","bps":7000},{"to":"G...B","bps":3000}]' \
#           ./scripts/create-jar.sh

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"
# env.sh may have cd'ed to the repository root; resolve libs from there.
. "${NOVATIP_ROOT}/scripts/lib/splits.sh"

require_env NETWORK SOURCE OWNER SLUG
require_contract_id

# Validate SLUG: 4–33 bytes (#124).
SLUG_LEN=$(printf '%s' "${SLUG}" | wc -c)
if [ "${SLUG_LEN}" -lt 4 ] || [ "${SLUG_LEN}" -gt 33 ]; then
  echo "error: SLUG must be 4–33 bytes (got ${SLUG_LEN})" >&2
  exit 1
fi

resolve_splits || exit 1

echo "==> Creating jar ${SLUG} on ${CONTRACT_ID} (${SPLIT_COUNT} recipient(s))"
stellar contract invoke \
  --id "${CONTRACT_ID}" \
  --source "${SOURCE}" \
  --network "${NETWORK}" \
  -- \
  create_jar \
  --owner "${OWNER}" \
  --jar_id "${SLUG}" \
  --splits "${SPLITS_JSON}"

echo "==> Done."
