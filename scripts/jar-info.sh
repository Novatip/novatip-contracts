#!/usr/bin/env bash
set -euo pipefail

# Print a jar's on-chain configuration: owner, each recipient with its share
# as a percentage, the minimum tip if one is set, and — with AMOUNT given —
# what a tip of that size would pay each recipient.
#
# This script submits no transactions. Every call below is read-only: the
# Stellar CLI simulates them, so there is no fee and no ledger change. A
# simulation still needs a signing identity, so SOURCE is required, but any
# identity works — it does not have to own or be listed in the jar.
#
# .env (repository root) is loaded automatically; exported variables win over
# it. The script works when started from any directory.
#
# Required env (see .env.example):
#   NETWORK, SOURCE, SLUG
#   CONTRACT_ID  (optional; defaults to .contract-id.<NETWORK>)
#   AMOUNT       (optional) tip size in stroops to preview
#
# Usage:  ./scripts/jar-info.sh
#         AMOUNT=100000 ./scripts/jar-info.sh

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"

require_env NETWORK SOURCE SLUG
require_contract_id

invoke() {
  stellar contract invoke \
    --id "${CONTRACT_ID}" \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    -- "$@"
}

# get_jar panics with Error(Contract, #3) for a free slug; translate that into
# one readable line instead of the CLI's raw error dump.
if ! JAR=$(invoke get_jar --jar_id "${SLUG}" 2>&1); then
  if grep -q 'Error(Contract, #3)' <<<"${JAR}"; then
    echo "no jar named ${SLUG} on ${CONTRACT_ID} (${NETWORK})" >&2
    exit 1
  fi
  echo "error: reading the jar failed:" >&2
  echo "${JAR}" >&2
  exit 1
fi

echo "Jar ${SLUG} on ${CONTRACT_ID} (${NETWORK})"
echo "  owner: $(jq -r '.owner' <<<"${JAR}")"

if ! MIN_TIP=$(invoke get_min_tip_amount --jar_id "${SLUG}" 2>&1); then
  echo "error: reading the minimum tip amount failed:" >&2
  echo "${MIN_TIP}" >&2
  exit 1
fi
if [ "$(jq -r 'type' <<<"${MIN_TIP}")" = "null" ] || [ "${MIN_TIP}" = "null" ]; then
  echo "  minimum tip: none"
else
  echo "  minimum tip: ${MIN_TIP} stroops"
fi

echo "  recipients ($(jq -r '.splits | length' <<<"${JAR}}"):"
jq -r '.splits[] | "    - \(.to)  (\(.bps / 100)%)"' <<<"${JAR}"
echo "  total percentage: $(jq -r '[.splits[].bps] | add / 100' <<<"${JAR}")%"

if ! JAR_STATE=$(invoke get_jar_state --jar_id "${SLUG}" 2>&1); then
  echo "error: reading the jar state failed:" >&2
  echo "${JAR_STATE}" >&2
  exit 1
fi
LOCKED=$(jq -r '.locked' <<<"${JAR_STATE}")
PAUSED=$(jq -r '.paused' <<<"${JAR_STATE}")
echo "  state: locked=$([ "${LOCKED}" = "true" ] && echo "yes" || echo "no"), paused=$([ "${PAUSED}" = "true" ] && echo "yes" || echo "no")"

if [ -n "${AMOUNT:-}" ]; then
  if ! PREVIEW=$(invoke preview_split --jar_id "${SLUG}" --amount "${AMOUNT}" 2>&1); then
    if grep -q 'Error(Contract, #13)' <<<"${PREVIEW}"; then
      echo "error: a tip of ${AMOUNT} is below ${SLUG}'s minimum — nothing to preview" >&2
    elif grep -q 'Error(Contract, #5)' <<<"${PREVIEW}"; then
      echo "error: preview_split rejected the amount ${AMOUNT} (SplitAmountTooSmall or AmountOverflow: too small to split, or overflow)" >&2
    else
      echo "error: previewing the tip failed:" >&2
      echo "${PREVIEW}" >&2
    fi
    exit 1
  fi
  echo "  a tip of ${AMOUNT} stroops would pay:"
  jq -r --argjson preview "${PREVIEW}" '
    .splits as $splits
    | range(0; ($splits | length)) as $i
    | "    - \($splits[$i].to)  ->  \($preview[$i]) stroops"' <<<"${JAR}"
fi
