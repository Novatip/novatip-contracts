#!/usr/bin/env bash
set -euo pipefail

# Manage the admin-controlled token allowlist (#170).
#
# `ACTION=allow` adds TOKEN, `ACTION=disallow` removes it and `ACTION=check`
# reports whether it is allowed. The deploy-time token is always allowed and
# cannot be removed — the script refuses that locally with the same explanation
# the contract gives for CannotDisallowDefault.
#
# Required env (see .env.example):
#   NETWORK      testnet | mainnet | local
#   SOURCE       Stellar CLI identity used to sign (must be the contract admin)
#   TOKEN        C... Stellar Asset Contract id to allow/disallow/check
#   ACTION       allow | disallow | check
#   CONTRACT_ID  optional; defaults to .contract-id.<NETWORK>
#   USDC_TOKEN   optional; deploy-time default token, used for the local
#                disallow check (read from get_token() when not set)
#
# Usage:  ACTION=allow TOKEN=CXXXX... ./scripts/allowlist.sh
#         ACTION=disallow TOKEN=CXXXX... ./scripts/allowlist.sh
#         ACTION=check TOKEN=CXXXX... ./scripts/allowlist.sh

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"
. "${NOVATIP_ROOT}/scripts/lib/cli.sh"

require_env NETWORK SOURCE TOKEN ACTION
require_contract_id
require_stellar_cli

if [ "${ACTION}" != "allow" ] && [ "${ACTION}" != "disallow" ] && [ "${ACTION}" != "check" ]; then
  echo "error: ACTION must be 'allow', 'disallow' or 'check' (got '${ACTION}')" >&2
  exit 1
fi
if [[ ! "${TOKEN}" =~ ^C[A-Z2-7]{55}$ ]]; then
  echo "error: TOKEN must be a valid C... Stellar Asset Contract id (got '${TOKEN}')" >&2
  exit 1
fi

invoke() {
  stellar contract invoke \
    --id "${CONTRACT_ID}" \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    -- "$@"
}

if [ "${ACTION}" = "disallow" ]; then
  DEFAULT_TOKEN="${USDC_TOKEN:-}"
  if [ -z "${DEFAULT_TOKEN}" ]; then
    DEFAULT_TOKEN="$(invoke get_token)"
  fi
  DEFAULT_TOKEN="$(printf '%s' "${DEFAULT_TOKEN}" | tr -d '"[:space:]')"
  if [ "${TOKEN}" = "${DEFAULT_TOKEN}" ]; then
    echo "error: TOKEN=${TOKEN} is the deploy-time default token." >&2
    echo "       tip() always settles through it, so the contract refuses to remove it" >&2
    echo "       with CannotDisallowDefault. No transaction was sent." >&2
    exit 1
  fi
fi

BEFORE="$(invoke is_token_allowed --token "${TOKEN}")"
echo "==> ${TOKEN} is_token_allowed before: ${BEFORE}"
case "${ACTION}" in
  allow)
    invoke allow_token --token "${TOKEN}"
    EXPECT="true"
    ;;
  disallow)
    invoke disallow_token --token "${TOKEN}"
    EXPECT="false"
    ;;
  check)
    echo "==> Done. No transaction sent for ACTION=check."
    exit 0
    ;;
esac

AFTER="$(invoke is_token_allowed --token "${TOKEN}")"
echo "==> ${TOKEN} is_token_allowed after: ${AFTER}"
if [ "${AFTER}" != "${EXPECT}" ]; then
  echo "error: read-back is_token_allowed='${AFTER}', expected '${EXPECT}'" >&2
  exit 1
fi
echo "==> Done. ${TOKEN} ${ACTION/allow/allowed} successfully."
