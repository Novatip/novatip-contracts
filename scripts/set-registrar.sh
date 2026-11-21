#!/usr/bin/env bash
set -euo pipefail

# Register or clear the contract's optional jar registrar (#295).
#
# Required env (see .env.example):
#   NETWORK      testnet | mainnet | local
#   SOURCE       Stellar CLI identity used to sign (must be the contract admin)
#   REGISTRAR    optional; G... or C... address whose co-signature create_jar
#                requires. Unset, empty, or "none" clears it (Option::None).
#   CONTRACT_ID  optional; defaults to .contract-id.<NETWORK>
#
# Usage:  ./scripts/set-registrar.sh
#         REGISTRAR=G... ./scripts/set-registrar.sh
#         REGISTRAR=none ./scripts/set-registrar.sh

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"
. "${NOVATIP_ROOT}/scripts/lib/cli.sh"

require_env NETWORK SOURCE
require_contract_id
require_stellar_cli

ARGS=(set_registrar)

if [ -z "${REGISTRAR:-}" ] || [ "${REGISTRAR}" = "none" ]; then
  echo "==> Clearing registrar on ${CONTRACT_ID} (no --registrar passed => Option::None)"
else
  if [[ ! "${REGISTRAR}" =~ ^[GC][A-Z2-7]{55}$ ]]; then
    echo "error: REGISTRAR must be a valid Stellar address (G... or C..., 56 chars) or 'none' (got '${REGISTRAR}')" >&2
    exit 1
  fi
  echo "==> Setting registrar on ${CONTRACT_ID} to ${REGISTRAR}"
  ARGS+=(--registrar "${REGISTRAR}")
fi

stellar contract invoke \
  --id "${CONTRACT_ID}" \
  --source "${SOURCE}" \
  --network "${NETWORK}" \
  -- \
  "${ARGS[@]}"

echo "==> Reading back configured registrar"
stellar contract invoke \
  --id "${CONTRACT_ID}" \
  --source "${SOURCE}" \
  --network "${NETWORK}" \
  -- \
  get_registrar

echo "==> Done."
