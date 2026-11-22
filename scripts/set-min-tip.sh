#!/usr/bin/env bash
set -euo pipefail

# Set or clear a tip jar's minimum tip amount.
#
# Required env (see .env.example):
#   NETWORK      testnet | mainnet | local
#   SOURCE       Stellar CLI identity used to sign (must be the jar owner)
#   SLUG         the jar's public id, e.g. @alice
#   MIN_TIP      positive integer amount in the token's smallest unit (USDC:
#                1_000_000 == 1 USDC). Unset, empty, or "none" clears the
#                minimum so any positive tip is accepted again.
#   CONTRACT_ID  optional; defaults to .contract-id.<NETWORK>
#
# Usage:  set -a; source .env; set +a; ./scripts/set-min-tip.sh
#         MIN_TIP=5000000 ./scripts/set-min-tip.sh   # 5 USDC floor
#         MIN_TIP=none   ./scripts/set-min-tip.sh   # clear the floor
#
# Passing "no minimum" to the CLI
# ------------------------------
# `set_min_tip_amount(jar_id, min_amount: Option<i128>)` takes an optional
# argument, and the Stellar CLI derives that from the contract spec: an
# `Option<T>` parameter becomes an *omittable* flag. Omitting `--min_amount`
# entirely is what sends `None`; it is not the same as passing an empty value,
# and there is no `--min_amount none`-style spelling to reach. So clearing a
# minimum means invoking without the flag at all, which is what this script
# does when MIN_TIP is unset or "none" — it builds the argument list itself
# rather than always passing `--min_amount`.
#
# The consequence of getting this wrong is silent: `--min_amount 0` is rejected
# by the contract with `InvalidMinimumAmount`, and leaving the flag out is the only
# way back to the "no floor" default.

: "${NETWORK:?set NETWORK}"
: "${SOURCE:?set SOURCE}"
: "${SLUG:?set SLUG}"
require_contract_id

# Build the method argument list. `set -u` friendly: MIN_TIP is allowed to be
# unset, which means "clear".
ARGS=(set_min_tip_amount --jar_id "${SLUG}")

if [ -z "${MIN_TIP:-}" ] || [ "${MIN_TIP}" = "none" ]; then
  echo "==> Clearing the minimum tip amount for ${SLUG} (no --min_amount passed => Option::None)"
else
  # Reject anything the contract would only reject later, before paying for a
  # network call. The contract takes a strictly positive i128, so a value that
  # is not a plain integer, or is zero or negative, cannot succeed.
  if [[ ! "${MIN_TIP}" =~ ^-?[0-9]+$ ]]; then
    echo "error: MIN_TIP must be an integer amount in the token's smallest unit (got '${MIN_TIP}')" >&2
    echo "       e.g. 5000000 for 5.00 USDC, or 'none' to clear the minimum" >&2
    exit 1
  fi
  if [ "${MIN_TIP:0:1}" = "-" ]; then
    echo "error: MIN_TIP must be positive, '${MIN_TIP}' is negative" >&2
    exit 1
  fi
  if [ "${MIN_TIP}" = "0" ]; then
    echo "error: MIN_TIP=0 would be rejected with InvalidMinimumAmount; use 'none' (or unset MIN_TIP) to clear the minimum" >&2
    exit 1
  fi
  # Strip leading zeros so the echo below and the CLI both see a canonical
  # integer; "000500" is the same floor as "500".
  MIN_TIP_CANON="${MIN_TIP#"${MIN_TIP%%[!0]*}"}"
  echo "==> Setting the minimum tip amount for ${SLUG} to ${MIN_TIP_CANON} smallest units"
  ARGS+=(--min_amount "${MIN_TIP_CANON}")
fi

stellar contract invoke \
  --id "${CONTRACT_ID}" \
  --source "${SOURCE}" \
  --network "${NETWORK}" \
  -- \
  "${ARGS[@]}"

# Read the value back: a clear is a no-op if the jar had no floor to begin
# with, and a set is worth confirming, since the CLI prints nothing on success.
echo "==> Reading back the configured minimum"
stellar contract invoke \
  --id "${CONTRACT_ID}" \
  --source "${SOURCE}" \
  --network "${NETWORK}" \
  -- \
  get_min_tip_amount \
  --jar_id "${SLUG}"

echo "==> Done. ${SLUG}: minimum is now $([ -z "${MIN_TIP:-}" ] || [ "${MIN_TIP}" = "none" ] && echo "cleared" || echo "${MIN_TIP_CANON} smallest units")."
