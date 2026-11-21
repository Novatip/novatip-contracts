#!/usr/bin/env bash
set -euo pipefail

# Manage recurring-tip schedules: create, show, cancel and collect.
#
# A schedule stores a supporter, jar, amount, period and message. Funds move
# only through collect_recurring, which pulls one due period from the token
# allowance the supporter gave this contract. SOURCE signs every subcommand:
# create needs the supporter's own signature, cancel accepts the supporter or
# the jar owner, and collect may be signed by any keeper.
#
# Required env (see .env.example):
#   NETWORK         testnet | mainnet | local
#   SOURCE          Stellar CLI identity signing the call
#   CONTRACT_ID     optional; defaults to .contract-id.<NETWORK>
#   create:  SLUG, AMOUNT, PERIOD_LEDGERS, and optionally MESSAGE
#   show/cancel/collect: ID
#
# Optional env:
#   SUPPORTER         identity whose address is the supporter (default: SOURCE)
#   APPROVE_AMOUNT    allowance in the printed approve command (default: AMOUNT)
#   EXPIRATION_LEDGER approve expiration ledger (default: current ledger + 100000)
#
# Usage:  ./scripts/recurring.sh create
#         ID=1 ./scripts/recurring.sh show
#         ID=1 ./scripts/recurring.sh cancel
#         ID=1 ./scripts/recurring.sh collect

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"
. "${NOVATIP_ROOT}/scripts/lib/cli.sh"

require_env NETWORK SOURCE
require_contract_id
require_stellar_cli
command -v jq >/dev/null 2>&1 || {
  echo "error: jq is required to read recurring schedules" >&2
  exit 1
}

ACTION="${1:-}"
case "${ACTION}" in
  create | show | cancel | collect) ;;
  *)
    echo "usage: $0 {create|show|cancel|collect}" >&2
    exit 1
    ;;
esac
if [ "$#" -gt 1 ]; then
  echo "error: unexpected argument '$2'" >&2
  exit 1
fi

invoke() {
  stellar contract invoke \
    --id "${CONTRACT_ID}" \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    -- "$@"
}

require_positive_int() {
  local name="$1" value="$2" max="${3:-}" canon
  if [[ ! "${value}" =~ ^[0-9]+$ ]]; then
    echo "error: ${name} must be a positive integer (got '${value}')" >&2
    exit 1
  fi
  canon="${value#"${value%%[!0]*}"}"
  [ -n "${canon}" ] || canon="0"
  if [ "${canon}" = "0" ]; then
    echo "error: ${name} must be a positive integer (got '${value}')" >&2
    exit 1
  fi
  if [ -n "${max}" ] && [ "${canon}" -gt "${max}" ]; then
    echo "error: ${name} must be at most ${max} (got '${value}')" >&2
    exit 1
  fi
}

validate_slug() {
  local name="$1" value="$2"
  if [[ ! "${value}" =~ ^@[a-z0-9_-]{3,32}$ ]]; then
    echo "error: ${name} must be a canonical @-prefixed slug (3-32 lowercase letters, digits, '_' or '-')" >&2
    exit 1
  fi
}

latest_ledger() {
  local out
  out=$(stellar rpc call --network "${NETWORK}" getLatestLedger 2>/dev/null) || return 1
  printf '%s' "${out}" | jq -r '.sequence'
}

create_schedule() {
  require_env SLUG AMOUNT PERIOD_LEDGERS
  validate_slug SLUG "${SLUG}"
  require_positive_int AMOUNT "${AMOUNT}"
  require_positive_int PERIOD_LEDGERS "${PERIOD_LEDGERS}" 4294967295

  MESSAGE="${MESSAGE:-recurring tip}"
  local msg_bytes
  msg_bytes=$(printf '%s' "${MESSAGE}" | wc -c | tr -d '[:space:]')
  if [ "${msg_bytes}" -gt 280 ]; then
    echo "error: MESSAGE is ${msg_bytes} bytes; the contract accepts at most 280" >&2
    exit 1
  fi

  local supporter_address source_address signer
  signer="${SUPPORTER:-${SOURCE}}"
  if ! supporter_address=$(stellar keys address "${signer}"); then
    echo "error: could not resolve supporter identity '${signer}' to a Stellar address" >&2
    exit 1
  fi
  if ! source_address=$(stellar keys address "${SOURCE}"); then
    echo "error: could not resolve SOURCE identity '${SOURCE}' to a Stellar address" >&2
    exit 1
  fi
  if [ "${source_address}" != "${supporter_address}" ]; then
    echo "error: create_recurring requires the supporter's signature; SOURCE resolves to ${source_address}, SUPPORTER is ${supporter_address}" >&2
    exit 1
  fi

  local token out id schedule
  token=$(invoke get_token)
  echo "==> Creating a ${AMOUNT}-per-${PERIOD_LEDGERS}-ledger schedule to ${SLUG} for ${supporter_address}"
  out=$(invoke create_recurring \
    --supporter "${supporter_address}" \
    --jar_id "${SLUG}" \
    --amount "${AMOUNT}" \
    --period_ledgers "${PERIOD_LEDGERS}" \
    --message "${MESSAGE}")
  id=$(printf '%s\n' "${out}" | grep -oE '[0-9]+' | tail -n 1 || true)
  if [ -z "${id}" ]; then
    echo "error: create_recurring succeeded but returned no schedule id" >&2
    exit 1
  fi
  echo "    schedule id: ${id}"

  echo "==> Reading back the new schedule"
  schedule=$(invoke get_recurring --id "${id}")
  printf '%s\n' "${schedule}" | jq .

  local approve_amount expiration ledger_now
  approve_amount="${APPROVE_AMOUNT:-${AMOUNT}}"
  require_positive_int APPROVE_AMOUNT "${approve_amount}"
  if [ -n "${EXPIRATION_LEDGER:-}" ]; then
    expiration="${EXPIRATION_LEDGER}"
    require_positive_int EXPIRATION_LEDGER "${expiration}"
  else
    ledger_now=$(latest_ledger || true)
    if [ -z "${ledger_now}" ] || [ "${ledger_now}" = "null" ]; then
      echo "error: could not read the current ledger to default EXPIRATION_LEDGER; set it explicitly." >&2
      exit 1
    fi
    expiration=$((ledger_now + 100000))
  fi

  echo ""
  echo "==> Before the first collect, approve this contract as the token spender:"
  echo "stellar contract invoke --id ${token} --source '${signer}' --network ${NETWORK} -- approve --from ${supporter_address} --spender ${CONTRACT_ID} --amount ${approve_amount} --expiration_ledger ${expiration}"
}

show_schedule() {
  require_env ID
  require_positive_int ID "${ID}"
  echo "==> Reading recurring schedule ${ID}"
  invoke get_recurring --id "${ID}" | jq .
}

cancel_schedule() {
  require_env ID
  require_positive_int ID "${ID}"

  local caller out
  if ! caller=$(stellar keys address "${SOURCE}"); then
    echo "error: could not resolve SOURCE identity '${SOURCE}' to a Stellar address" >&2
    exit 1
  fi
  echo "==> Schedule ${ID} before cancellation"
  invoke get_recurring --id "${ID}" | jq .
  echo "==> Cancelling ${ID} as ${caller}"
  invoke cancel_recurring --caller "${caller}" --id "${ID}"

  echo "==> Confirming ${ID} is gone"
  if out=$(invoke get_recurring --id "${ID}" 2>&1); then
    echo "error: get_recurring still succeeded after cancel; the schedule may remain:" >&2
    printf '%s\n' "${out}" >&2
    exit 1
  fi
  echo "    get_recurring now fails with RecurringNotFound, as expected."
  echo "==> Done. Schedule ${ID} was cancelled."
}

collect_schedule() {
  require_env ID
  require_positive_int ID "${ID}"

  local schedule next_due period out after new_due ledger_now
  schedule=$(invoke get_recurring --id "${ID}")
  next_due=$(printf '%s\n' "${schedule}" | jq -r '.next_due_ledger')
  period=$(printf '%s\n' "${schedule}" | jq -r '.period_ledgers')
  echo "==> Schedule ${ID}: next_due_ledger=${next_due}, period_ledgers=${period}"

  if ! out=$(invoke collect_recurring --id "${ID}" 2>&1); then
    printf '%s\n' "${out}" >&2
    if printf '%s\n' "${out}" | grep -q 'NotDue'; then
      ledger_now=$(latest_ledger || true)
      if [ -n "${ledger_now}" ] && [ "${ledger_now}" != "null" ]; then
        echo "==> NotDue: the current ledger is ${ledger_now}; this schedule is not payable until ledger ${next_due} (period: ${period} ledgers)." >&2
      else
        echo "==> NotDue: wait until next_due_ledger=${next_due} before collecting." >&2
      fi
    fi
    exit 1
  fi

  echo "==> Collected one period of schedule ${ID}"
  echo "==> Reading back the advanced schedule"
  after=$(invoke get_recurring --id "${ID}")
  printf '%s\n' "${after}" | jq .
  new_due=$(printf '%s\n' "${after}" | jq -r '.next_due_ledger')
  echo "==> Done. next_due_ledger is now ${new_due}."
}

case "${ACTION}" in
  create) create_schedule ;;
  show) show_schedule ;;
  cancel) cancel_schedule ;;
  collect) collect_schedule ;;
esac
