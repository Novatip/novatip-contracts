#!/usr/bin/env bash
set -euo pipefail

# Print and withdraw a recipient's escrowed share from a past tip (#171).
#
# A recipient without a USDC trustline does not make a tip fail: the contract
# holds their share until they can receive it. This script reads that amount,
# checks the recipient's trustline with the same logic as
# `scripts/trustline.sh --check`, and withdraws the held funds.
#
# Required env (see .env.example):
#   NETWORK      testnet | mainnet | local
#   SOURCE       Stellar CLI identity used to sign (must be the recipient)
#   RECIPIENT    G... address owed the held funds
#   USDC_TOKEN   USDC Stellar Asset Contract id (C...), for the trustline check
#   CONTRACT_ID  optional; defaults to .contract-id.<NETWORK>
#   HORIZON_URL  optional; overrides the Horizon endpoint for the trustline check
#
# Usage:  set -a; source .env; set +a; ./scripts/withdraw-held.sh
#         RECIPIENT=GXXXX... ./scripts/withdraw-held.sh

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"
. "${NOVATIP_ROOT}/scripts/lib/cli.sh"

require_env NETWORK SOURCE RECIPIENT USDC_TOKEN
require_contract_id
require_stellar_cli

if [[ ! "${RECIPIENT}" =~ ^G[A-Z2-7]{55}$ ]]; then
  echo "error: RECIPIENT must be a valid Stellar G... account address (got '${RECIPIENT}')" >&2
  exit 1
fi

invoke() {
  stellar contract invoke \
    --id "${CONTRACT_ID}" \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    -- "$@"
}

# The CLI may quote the i128 or print separators; compare canonical digits so a
# formatted "0" still counts as nothing held.
canonical_amount() {
  local amount
  amount="$(printf '%s' "$1" | tr -d '[:space:]_,"')"
  if [[ ! "${amount}" =~ ^[0-9]+$ ]]; then
    echo "error: unexpected held amount '${1}' from get_held" >&2
    return 1
  fi
  amount="${amount#"${amount%%[!0]*}"}"
  [ -n "${amount}" ] || amount="0"
  printf '%s' "${amount}"
}

echo "==> Reading held balance for ${RECIPIENT}"
HELD="$(canonical_amount "$(invoke get_held --recipient "${RECIPIENT}")")"
echo "    held: ${HELD} smallest units"
if [ "${HELD}" = "0" ]; then
  echo "==> Done. Nothing is held for ${RECIPIENT}; no transaction sent."
  exit 0
fi

echo "==> Checking ${RECIPIENT}'s USDC trustline"
if ! IDENTITIES="${RECIPIENT}" "${NOVATIP_ROOT}/scripts/trustline.sh" --check; then
  echo "    warning: ${RECIPIENT} cannot receive USDC yet; withdraw_held will fail until the trustline exists." >&2
  echo "             Run IDENTITIES=${RECIPIENT} ./scripts/trustline.sh to add it." >&2
fi

echo "==> Withdrawing ${HELD} smallest units to ${RECIPIENT}"
invoke withdraw_held --recipient "${RECIPIENT}"

echo "==> Reading back the held balance"
HELD_AFTER="$(canonical_amount "$(invoke get_held --recipient "${RECIPIENT}")")"
echo "    held after withdrawal: ${HELD_AFTER}"
if [ "${HELD_AFTER}" != "0" ]; then
  echo "error: get_held still reports ${HELD_AFTER} after withdraw_held" >&2
  exit 1
fi

echo "==> Done. ${HELD} smallest units were released to ${RECIPIENT}."
