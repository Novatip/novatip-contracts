#!/usr/bin/env bash
set -euo pipefail

# Deploy the tip_splitter contract to a Stellar network.
#
# Required env (see .env.example):
#   NETWORK     testnet | mainnet | local
#   SOURCE      Stellar CLI identity used to sign
#   ADMIN       admin address stored in the contract
#   USDC_TOKEN  USDC Stellar Asset Contract id (C...)
#
# Usage:  set -a; source .env; set +a; ./scripts/deploy.sh

: "${NETWORK:?set NETWORK}"
: "${SOURCE:?set SOURCE}"
: "${ADMIN:?set ADMIN}"
: "${USDC_TOKEN:?set USDC_TOKEN}"

WASM="target/wasm32-unknown-unknown/release/tip_splitter.wasm"

echo "==> Building contracts"
stellar contract build

echo "==> Deploying tip_splitter to ${NETWORK}"
CONTRACT_ID=$(stellar contract deploy \
  --wasm "${WASM}" \
  --source "${SOURCE}" \
  --network "${NETWORK}" \
  -- \
  --admin "${ADMIN}" \
  --token "${USDC_TOKEN}")

echo "==> Deployed contract id: ${CONTRACT_ID}"
echo "${CONTRACT_ID}" > .contract-id
echo "    (saved to .contract-id)"
