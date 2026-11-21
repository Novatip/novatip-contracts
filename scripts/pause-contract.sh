#!/usr/bin/env bash
set -euo pipefail

# Pause or resume tipping for the whole contract, or inspect its state (#292).
# SOURCE must be the contract admin.
#
# Required env (see .env.example):
#   NETWORK      testnet | mainnet | local
#   SOURCE       Stellar CLI identity used to sign (must be the contract admin)
#   ACTION       pause | resume | status
#   CONTRACT_ID  optional; defaults to .contract-id.<NETWORK>
#
# Usage:  ACTION=pause ./scripts/pause-contract.sh
#         ACTION=resume ./scripts/pause-contract.sh
#         ACTION=status ./scripts/pause-contract.sh
#
# ACTION is also used by other scripts (e.g. lock-splits.sh), so set it on the
# command line for each invocation rather than relying on a shared .env value.

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"
. "${NOVATIP_ROOT}/scripts/lib/cli.sh"

require_env NETWORK SOURCE ACTION
require_contract_id
require_stellar_cli

if [ "${ACTION}" != "pause" ] && [ "${ACTION}" != "resume" ] && [ "${ACTION}" != "status" ]; then
  echo "error: ACTION must be 'pause', 'resume' or 'status' (got '${ACTION}')" >&2
  exit 1
fi

invoke() {
  stellar contract invoke \
    --id "${CONTRACT_ID}" \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    -- "$@"
}

remind_entry_points() {
  echo "    Blocked while paused: tip, tip_with_ref and tip_in_token (ContractPaused)."
  echo "    Still available: every read, collect_recurring and withdraw_held."
}

BEFORE="$(invoke is_paused)"
echo "==> Contract is_paused before: ${BEFORE}"

if [ "${ACTION}" = "status" ]; then
  remind_entry_points
  echo "==> Done. No transaction sent for ACTION=status."
  exit 0
fi

echo "==> Calling ${ACTION}"
invoke "${ACTION}"

AFTER="$(invoke is_paused)"
echo "==> Contract is_paused after: ${AFTER}"
EXPECT="false"
if [ "${ACTION}" = "pause" ]; then
  EXPECT="true"
fi
if [ "${AFTER}" != "${EXPECT}" ]; then
  echo "error: read-back is_paused='${AFTER}', expected '${EXPECT}'" >&2
  exit 1
fi
remind_entry_points
echo "==> Done."
