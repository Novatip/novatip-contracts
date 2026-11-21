#!/usr/bin/env bash
set -euo pipefail

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"

# List events published by a deployed tip_splitter contract, with the topic
# filters computed correctly instead of hand-encoded.
#
# Jar events have two topics: the event-name symbol and the jar id string.
# Admin events have only the name topic. `stellar events` wants each topic
# segment as base64 XDR of an ScVal, so this script encodes them with `stellar xdr encode
# --type ScVal` rather than hardcoding base64. A wrongly encoded segment fails
# with "xdr value invalid"; a wrong segment *count* silently returns nothing,
# which is why the encoding is derived here in one place.
#
# Required env:
#   NETWORK      testnet | mainnet | local
#   CONTRACT_ID  optional; defaults to .contract-id.<NETWORK>
#
# Optional env:
#   EVENT          one of jar_crtd | splits | jar_xfer | min_tip | tip |
#                  tk_allow | tk_dis | tip_hook | reg_set | fee_set | pause |
#                  resume | adm_xfer | upgrade | all (default: all)
#   SLUG           jar id to filter on, e.g. "@demo". Ignored for admin events,
#                  which have no jar id topic. Unset lists every jar.
#   START_LEDGER   first ledger to scan. Default: WINDOW ledgers before the
#                  network's current ledger.
#   END_LEDGER     last ledger to scan. Default: the current ledger.
#   WINDOW         how far back the default start reaches (default: 10000,
#                  roughly 14 hours at the 5-second close rate).
#
# Usage:  set -a; source .env; set +a
#         ./scripts/events.sh                          # every event, all jars
#         EVENT=tip SLUG=@demo ./scripts/events.sh     # tips to one jar
#
# Sample session (testnet, contract from .contract-id.<NETWORK>, jar "@demo"):
#
#   $ EVENT=tip SLUG=@demo ./scripts/events.sh
#   ==> ledger 4 512 104  tip  @demo  from=GBSG...ZTTQEVFL amount=2 500 000
#       message="great set"  breakdown=[["GA7T...3YQYMA",1 750 000],
#                                        ["GDVE...4AQH3ZLLFLA5",750 000]]
#   ==> ledger 4 512 219  tip  @demo  from=GA33...O5QZSL amount=1 000 000
#       message="thanks!"  breakdown=[["GA7T...3YQYMA",700 000],
#                                     ["GDVE...4AQH3ZLLFLA5",300 000]]
#   2 event(s) from ledger 4 502 104 to 4 512 104.
#
# Note the RPC keeps only a recent window of ledgers (each public RPC's
# --event-retention; testnet's is about 7 days). A START_LEDGER older than
# that fails, and this script explains the limit rather than passing the raw
# error through.

: "${NETWORK:?set NETWORK (testnet | mainnet | local)}"
require_contract_id
EVENT="${EVENT:-all}"
WINDOW="${WINDOW:-10000}"

command -v stellar >/dev/null 2>&1 || {
  echo "error: the stellar CLI is required (https://developers.stellar.org/docs/run-cli/get-started)" >&2
  exit 1
}
command -v jq >/dev/null 2>&1 || {
  echo "error: jq is required to parse the event output" >&2
  exit 1
}

# Events published with the jar id as their second topic. Admin events carry
# only the event-name topic, so SLUG must not be appended for them.
JAR_EVENTS=(jar_crtd splits jar_xfer min_tip tip)
ADMIN_EVENTS=(tk_allow tk_dis tip_hook reg_set fee_set pause resume adm_xfer upgrade)
VALID_EVENTS=("${JAR_EVENTS[@]}" "${ADMIN_EVENTS[@]}")

is_jar_event() {
  local e
  for e in "${JAR_EVENTS[@]}"; do
    [ "${e}" = "$1" ] && return 0
  done
  return 1
}

if [ "${EVENT}" != "all" ]; then
  ok=0
  for e in "${VALID_EVENTS[@]}"; do
    [ "${e}" = "${EVENT}" ] && ok=1
  done
  if [ "${ok}" -eq 0 ]; then
    echo "error: EVENT must be one of ${VALID_EVENTS[*]} or all (got '${EVENT}')" >&2
    exit 1
  fi
fi

# Encode an ScVal as base64 for use as a topic segment. Newer CLIs print a
# JSON envelope with the base64 under .raw; older ones print the bare value.
encode_scval() {
  local out
  out=$(stellar xdr encode --type ScVal "$1")
  case "${out}" in
    '{'*) printf '%s' "${out}" | jq -r '.raw // .bytes // empty' ;;
    *)    printf '%s' "${out}" | tr -d '[:space:]' ;;
  esac
}

# Decode a base64 ScVal topic/value to a short human-readable form.
decode_scval() {
  local out
  out=$(stellar xdr decode --type ScVal "$1" --output json 2>/dev/null) || out=""
  if [ -n "${out}" ]; then
    printf '%s' "${out}" | jq -c . 2>/dev/null || printf '%s' "${out}"
  else
    printf '%s' "$1"
  fi
}

# Reduce a decoded ScVal JSON to its single string payload — the symbol name
# or the jar id — so one output line reads: ledger  name  @slug  payload.
scval_word() {
  printf '%s' "$1" | jq -r '[.. | strings] | if length > 0 then .[0] else "." end' 2>/dev/null \
    || printf '%s' "$1"
}

# Current ledger, used to default the scan window and to explain retention errors.
latest_ledger() {
  local out
  out=$(stellar rpc call --network "${NETWORK}" getLatestLedger 2>/dev/null) || return 1
  printf '%s' "${out}" | jq -r '.sequence'
}

LEDGER_NOW="$(latest_ledger || true)"
if [ -z "${LEDGER_NOW}" ] || [ "${LEDGER_NOW}" = "null" ]; then
  echo "error: could not read the current ledger from the ${NETWORK} RPC." >&2
  echo "       Check NETWORK and RPC_URL, or pass START_LEDGER/END_LEDGER explicitly." >&2
  exit 1
fi
START_LEDGER="${START_LEDGER:-$((LEDGER_NOW > WINDOW ? LEDGER_NOW - WINDOW : 0))}"
END_LEDGER="${END_LEDGER:-${LEDGER_NOW}}"

if [ "${START_LEDGER}" -ge "${END_LEDGER}" ]; then
  echo "error: START_LEDGER (${START_LEDGER}) must be below END_LEDGER (${END_LEDGER})." >&2
  exit 1
fi

# Topics are built per event inside list_events: the event-name symbol, plus
# the jar id as a second segment when SLUG is set. With no SLUG the
# single-symbol segment acts as a prefix filter that matches every jar.
TOTAL=0
list_events() {
  local name="$1"
  local -a args=(events --id "${CONTRACT_ID}" --network "${NETWORK}"
                 --start-ledger "${START_LEDGER}" --end-ledger "${END_LEDGER}"
                 --output json)
  args+=(--topic "$(encode_scval "{\"symbol\":\"${name}\"}")")
  if [ -n "${SLUG:-}" ] && is_jar_event "${name}"; then
    args+=(--topic "$(encode_scval "{\"string\":\"${SLUG}\"}")")
  fi

  local raw err_file
  err_file=$(mktemp)
  if ! raw=$(stellar "${args[@]}" 2>"${err_file}"); then
    if grep -qiE 'startLedger|ledger.*not.*(found|available)|outside.*window|retention|old' "${err_file}"; then
      echo "error: the ${NETWORK} RPC only keeps a recent window of ledgers (about 7 days" >&2
      echo "       on the public testnet RPC), and ledger ${START_LEDGER} is outside it." >&2
      echo "       Raise START_LEDGER or lower WINDOW; older events need a full-history" >&2
      echo "       archive or your own RPC with a longer --event-retention-window." >&2
    else
      echo "error: stellar events failed for '${name}':" >&2
      sed 's/^/    /' "${err_file}" >&2
    fi
    rm -f "${err_file}"
    return 1
  fi
  rm -f "${err_file}"

  # Each event: id is "<ledger>-<order>", topic[0] is the contract itself,
  # topic[1] the event symbol, topic[2] the jar id, and value the payload.
  local count
  count=$(printf '%s' "${raw}" | jq -r 'length')
  [ "${count}" = "0" ] && return 0
  printf '%s' "${raw}" | jq -c '.[]' | while IFS= read -r event; do
    ledger=$(printf '%s' "${event}" | jq -r '.id' | cut -d- -f1)
    symbol_b64=$(printf '%s' "${event}" | jq -r '.topic[1] // empty')
    jar_b64=$(printf '%s' "${event}" | jq -r '.topic[2] // empty')
    value=$(printf '%s' "${event}" | jq -r '.value // .topic // "?"')
    symbol=$(scval_word "$(decode_scval "${symbol_b64}")")
    jar=$(scval_word "$(decode_scval "${jar_b64}")")
    value=$(decode_scval "${value}")
    printf '==> ledger %-14s %-9s %-12s %s\n' "${ledger:-?}" "${symbol}" "${jar:-?}" "${value}"
  done
  TOTAL=$((TOTAL + count))
}

if [ "${EVENT}" = "all" ]; then
  for name in "${VALID_EVENTS[@]}"; do
    list_events "${name}" || true
  done
else
  list_events "${EVENT}" || true
fi

echo "${TOTAL:-0} event(s) from ledger ${START_LEDGER} to ${END_LEDGER}."
