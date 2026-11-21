#!/usr/bin/env bash
set -euo pipefail

# Upload a wasm and upgrade the deployed tip_splitter contract in place.
#
# `upgrade(new_wasm_hash)` only accepts a hash whose wasm is already installed
# on the network. This script builds the current checkout (or takes an existing
# WASM), uploads it, checks the returned hash against the local SHA-256, asks
# for an explicit confirmation on mainnet, calls upgrade, and verifies that the
# deployed code is now the wasm that was uploaded. version() is printed before
# and after so an interface bump is visible in a single run.
#
# Required env (see .env.example):
#   NETWORK      testnet | mainnet | local
#   SOURCE       Stellar CLI identity holding the contract admin's key
#   WASM         optional path to a prebuilt wasm; unset builds this checkout
#   CONTRACT_ID  optional; defaults to .contract-id.<NETWORK>
#
# Safety override:
#   CONFIRM_MAINNET=1  skip the typed mainnet confirmation (CI only)
#
# Usage:  ./scripts/upgrade.sh
#         WASM=target/wasm32v1-none/release/tip_splitter.wasm ./scripts/upgrade.sh

. "$(dirname "${BASH_SOURCE[0]}")/lib/env.sh"
. "${NOVATIP_ROOT}/scripts/lib/cli.sh"

require_env NETWORK SOURCE
require_contract_id
require_stellar_cli

invoke() {
  stellar contract invoke \
    --id "${CONTRACT_ID}" \
    --source "${SOURCE}" \
    --network "${NETWORK}" \
    -- "$@"
}

# Build or accept a wasm, then hash it locally. The Soroban wasm hash is the
# SHA-256 of the bytes, so this value is both the upload receipt and the hash
# passed to upgrade; computing it locally lets us reject a mismatched upload
# before sending the privileged call.
if [ -z "${WASM:-}" ]; then
  echo "==> Building optimized wasm"
  stellar contract build --optimize
  WASM="$(./scripts/find-wasm.sh || true)"
  if [ -z "${WASM}" ] || [ ! -f "${WASM}" ]; then
    echo "error: no built tip_splitter.wasm found under target/." >&2
    echo "       If the build printed a missing-target error, run:" >&2
    echo "       rustup target add wasm32v1-none" >&2
    exit 1
  fi
elif [ ! -f "${WASM}" ]; then
  echo "error: WASM='${WASM}' does not exist" >&2
  exit 1
fi

WASM_SIZE=$(wc -c < "${WASM}")
LOCAL_HASH=$(sha256sum "${WASM}" | cut -d' ' -f1)
echo "==> Wasm: ${WASM} (${WASM_SIZE} bytes)"
echo "    SHA-256: ${LOCAL_HASH}"

echo "==> version() before upgrade"
invoke version

# Upload first: a hash cannot be used by upgrade until the code is installed.
echo "==> Uploading wasm to ${NETWORK}"
UPLOAD_OUTPUT=$(stellar contract upload \
  --wasm "${WASM}" \
  --source "${SOURCE}" \
  --network "${NETWORK}" 2>&1)
printf '%s\n' "${UPLOAD_OUTPUT}"

UPLOAD_HASH=$(printf '%s\n' "${UPLOAD_OUTPUT}" | grep -oiE '[0-9a-f]{64}' | tail -n 1 || true)
UPLOAD_HASH=$(printf '%s' "${UPLOAD_HASH}" | tr '[:upper:]' '[:lower:]')
if [ -n "${UPLOAD_HASH}" ] && [ "${UPLOAD_HASH}" != "${LOCAL_HASH}" ]; then
  echo "error: uploaded hash ${UPLOAD_HASH} does not match local SHA-256 ${LOCAL_HASH}." >&2
  echo "       No upgrade call was sent." >&2
  exit 1
fi

# An upgrade keeps the address and all state but replaces the executed code,
# so mainnet needs a typed confirmation showing exactly what will be installed.
if [ "${NETWORK}" = "mainnet" ] && [ -z "${CONFIRM_MAINNET:-}" ]; then
  echo ""
  echo "=== MAINNET UPGRADE CONFIRMATION ==="
  echo "  Network:  ${NETWORK}"
  echo "  Contract: ${CONTRACT_ID}"
  echo "  Source:   ${SOURCE}"
  echo "  Wasm:     ${WASM}"
  echo "  New hash: ${LOCAL_HASH}"
  echo ""
  printf "Type 'yes' to confirm mainnet upgrade: "
  read -r confirmation
  if [ "${confirmation}" != "yes" ]; then
    echo "Upgrade cancelled." >&2
    exit 1
  fi
fi

echo "==> Upgrading ${CONTRACT_ID}"
invoke upgrade --new_wasm_hash "${LOCAL_HASH}"

# Same check as verify-deployment.sh: fetch the code the contract now runs and
# compare its hash with the wasm we just uploaded.
echo "==> Verifying the deployed wasm matches the uploaded hash"
DEPLOYED_WASM=$(mktemp /tmp/novatip-upgraded-XXXXXX.wasm)
trap 'rm -f "${DEPLOYED_WASM}"' EXIT
stellar contract fetch \
  --id "${CONTRACT_ID}" \
  --network "${NETWORK}" \
  --out-file "${DEPLOYED_WASM}"
DEPLOYED_HASH=$(sha256sum "${DEPLOYED_WASM}" | cut -d' ' -f1)
echo "    Deployed SHA-256: ${DEPLOYED_HASH}"
if [ "${DEPLOYED_HASH}" != "${LOCAL_HASH}" ]; then
  echo "error: deployed wasm hash does not match the uploaded wasm." >&2
  exit 1
fi

echo "==> version() after upgrade"
invoke version

echo "==> Done. ${CONTRACT_ID} now runs wasm ${LOCAL_HASH}."
