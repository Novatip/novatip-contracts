#!/usr/bin/env bash
set -euo pipefail

# Register or clear the contract's optional post-tip hook (#295).
#
# Required env (see .env.example):
#   NETWORK      testnet | mainnet | local
#   SOURCE       Stellar CLI identity used to sign (must be the contract admin)
#   HOOK         optional; C... contract address for the post-tip hook.
#                Unset, empty, or "none" clears the hook (Option::None).
#   CONTRACT_ID  optional; defaults to .contract-id.<NETWORK>
#
# Usage:  ./scripts/set-hook.sh
#         HOOK=C... ./scripts/set-hook.sh
#         HOOK=none ./scripts/set-hook.sh

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"
. "${NOVATIP_ROOT}/scripts/lib/cli.sh"

require_env NETWORK SOURCE
require_contract_id
require_stellar_cli

ARGS=(set_tip_hook)

if [ -z "${HOOK:-}" ] || [ "${HOOK}" = "none" ]; then
  echo "==> Clearing post-tip hook on ${CONTRACT_ID} (no --hook passed => Option::None)"
else
  if [[ ! "${HOOK}" =~ ^C[A-Z2-7]{55}$ ]]; then
    echo "error: HOOK must be a valid Stellar contract address (C..., 56 chars) or 'none' (got '${HOOK}')" >&2
    exit 1
  fi
  echo "==> Setting post-tip hook on ${CONTRACT_ID} to ${HOOK}"
  ARGS+=(--hook "${HOOK}")
fi

stellar contract invoke \
  --id "${CONTRACT_ID}" \
  --source "${SOURCE}" \
  --network "${NETWORK}" \
  -- \
  "${ARGS[@]}"

echo "==> Reading back configured post-tip hook"
stellar contract invoke \
  --id "${CONTRACT_ID}" \
  --source "${SOURCE}" \
  --network "${NETWORK}" \
  -- \
  get_tip_hook

echo "==> Done."
