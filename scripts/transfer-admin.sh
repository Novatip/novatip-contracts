#!/usr/bin/env bash
set -euo pipefail

# Hand contract admin control to a new account (#293).
#
# Rotating the admin is irreversible (there is no acceptance step), so this
# script verifies that NEW_ADMIN resolves to a real account on the network and
# asks for the address to be typed before signing. Use YES=1 in non-interactive
# environments after checking the address yourself.
#
# Required env (see .env.example):
#   NETWORK      testnet | mainnet | local
#   SOURCE       Stellar CLI identity used to sign (the current admin)
#   NEW_ADMIN    G... account address, or a configured CLI identity name
#   CONTRACT_ID  optional; defaults to .contract-id.<NETWORK>
#   HORIZON_URL  optional; overrides the Horizon endpoint used to check the account
#   YES          set to 1 to skip the typed confirmation
#
# Usage:  set -a; source .env; set +a; ./scripts/transfer-admin.sh
#         YES=1 NEW_ADMIN=GXXXX... ./scripts/transfer-admin.sh

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"
. "${NOVATIP_ROOT}/scripts/lib/cli.sh"

require_env NETWORK SOURCE NEW_ADMIN
require_contract_id
require_stellar_cli

# A new admin must be a classic account: contract addresses cannot sign the
# follow-up admin calls. Identity names are resolved with `stellar keys`.
NEW_ADMIN_ADDRESS="${NEW_ADMIN}"
if [[ ! "${NEW_ADMIN_ADDRESS}" =~ ^G[A-Z2-7]{55}$ ]]; then
  if ! NEW_ADMIN_ADDRESS="$(stellar keys address "${NEW_ADMIN}" 2>/dev/null)"; then
    echo "error: NEW_ADMIN='${NEW_ADMIN}' is neither a G... address nor a configured CLI identity" >&2
    exit 1
  fi
  NEW_ADMIN_ADDRESS="$(printf '%s' "${NEW_ADMIN_ADDRESS}" | tr -d '"[:space:]')"
  if [[ ! "${NEW_ADMIN_ADDRESS}" =~ ^G[A-Z2-7]{55}$ ]]; then
    echo "error: '${NEW_ADMIN}' resolved to '${NEW_ADMIN_ADDRESS}', which is not a G... account address" >&2
    exit 1
  fi
fi

case "${NETWORK}" in
  testnet)  HORIZON_URL="${HORIZON_URL:-https://horizon-testnet.stellar.org}" ;;
  mainnet)  HORIZON_URL="${HORIZON_URL:-https://horizon.stellar.org}" ;;
  local)    HORIZON_URL="${HORIZON_URL:-http://localhost:8000}" ;;
  *)        echo "error: set HORIZON_URL for unknown NETWORK '${NETWORK}'" >&2; exit 1 ;;
esac

command -v curl >/dev/null 2>&1 || { echo "error: curl is required to check NEW_ADMIN on ${NETWORK}" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "error: jq is required to check NEW_ADMIN on ${NETWORK}" >&2; exit 1; }

echo "==> Checking that ${NEW_ADMIN_ADDRESS} exists on ${NETWORK}"
if ! ACCOUNT="$(curl -fsS "${HORIZON_URL}/accounts/${NEW_ADMIN_ADDRESS}" 2>/dev/null)"; then
  echo "error: ${NEW_ADMIN_ADDRESS} was not found on ${NETWORK} (${HORIZON_URL})." >&2
  echo "       Fund it on this network before rotating the admin to it." >&2
  exit 1
fi
ACCOUNT_ID="$(printf '%s' "${ACCOUNT}" | jq -r '.account_id // empty')"
if [ "${ACCOUNT_ID}" != "${NEW_ADMIN_ADDRESS}" ]; then
  echo "error: Horizon returned account '${ACCOUNT_ID}', expected '${NEW_ADMIN_ADDRESS}'" >&2
  exit 1
fi

invoke() {
  stellar contract invoke \
    --id "${CONTRACT_ID}" \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    -- "$@"
}

CURRENT_ADMIN="$(invoke get_admin)"
echo "==> Current admin: ${CURRENT_ADMIN}"
if [ "${CURRENT_ADMIN}" = "${NEW_ADMIN_ADDRESS}" ]; then
  echo "==> Done. ${NEW_ADMIN_ADDRESS} is already the admin; no transaction sent."
  exit 0
fi

if [ "${YES:-}" != "1" ]; then
  echo "==> This cannot be undone. Type the new admin address to confirm:"
  if ! IFS= read -r CONFIRMATION; then
    echo "error: confirmation required; set YES=1 only after checking the address" >&2
    exit 1
  fi
  if [ "${CONFIRMATION}" != "${NEW_ADMIN_ADDRESS}" ]; then
    echo "error: confirmation did not match ${NEW_ADMIN_ADDRESS}; no transaction sent" >&2
    exit 1
  fi
fi

echo "==> Transferring admin to ${NEW_ADMIN_ADDRESS}"
invoke transfer_admin --new_admin "${NEW_ADMIN_ADDRESS}"

ADMIN_AFTER="$(invoke get_admin)"
echo "==> New admin read back: ${ADMIN_AFTER}"
if [ "${ADMIN_AFTER}" != "${NEW_ADMIN_ADDRESS}" ]; then
  echo "error: read-back admin '${ADMIN_AFTER}' does not match ${NEW_ADMIN_ADDRESS}" >&2
  exit 1
fi
echo "==> Done."
