# novatip-contracts

Soroban smart contracts for **Novatip** — tap-to-tip any creator in seconds, with
tips split across collaborators on-chain and settled in USDC on Stellar.

This repository holds the on-chain core. The backend (`novatip-backend`), web app
(`novatip-web`), shared SDK (`novatip-sdk`), and docs (`novatip-docs`) live in
separate repositories under the same organization.

## Contracts

| Contract | Status | Purpose |
|----------|--------|---------|
| `tip-splitter` | ✅ live on testnet (see [Network Deployments](#network-deployments)) | Receives a tip and splits it across recipients by basis points |
| `supporter-badge` | planned | Non-transferable badge minted to supporters |

See [`docs/CONTRACT.md`](./docs/CONTRACT.md) for the full `tip-splitter` interface.

## Quick start

```bash
# Requires Stellar CLI 27.1.0 or newer (`stellar version`)
# One-time: the Stellar CLI builds contracts for this target
rustup target add wasm32v1-none

# Build all contracts to wasm
make build

# Run the test suite
make test
```

> Without the target installed, `make build` fails with `can't find crate for
> core` — the CLI moved from `wasm32-unknown-unknown` to `wasm32v1-none`.
> `make test` runs natively and works either way.

## Scripts

Everything under [`scripts/`](./scripts) reads the same environment variables
(see [`.env.example`](./.env.example)) and signs with a configured Stellar CLI
identity. `CONTRACT_ID` is optional for every script that needs it: it falls
back to the `.contract-id.<NETWORK>` file `deploy.sh` writes.

Load your environment once per shell, then run whichever script you need:

```bash
set -a; source .env; set +a

./scripts/deploy.sh          # build (optimized), deploy; writes .contract-id.<NETWORK>
./scripts/create-jar.sh      # register a jar (RECIPIENT or SPLITS)
./scripts/tip.sh             # send AMOUNT of the jar's token to every recipient
./scripts/transfer-jar.sh    # hand a jar to NEW_OWNER
./scripts/set-min-tip.sh     # set a jar's minimum tip, or clear it
./scripts/set-fee.sh         # set the protocol fee (default 100 bps = 1%)
./scripts/pause-jar.sh       # pause or resume tipping (PAUSED=true|false)
./scripts/lock-splits.sh     # lock splits, or print the multisigner unlock flow
./scripts/leave-jar.sh       # remove SOURCE from a jar's recipient splits
./scripts/withdraw-held.sh   # release a recipient's escrowed share
./scripts/allowlist.sh       # allow, disallow or check a token (ACTION=...)
./scripts/pause-contract.sh  # pause, resume or inspect contract tipping
./scripts/transfer-admin.sh  # hand admin control to a verified account
```

| Script | Needs | Notes |
|--------|-------|-------|
| `deploy.sh` | `NETWORK`, `SOURCE`, `ADMIN`, `USDC_TOKEN` | Prints and saves the deployed contract id to `.contract-id.<NETWORK>` |
| `create-jar.sh` | `NETWORK`, `SOURCE`, `OWNER`, `SLUG`, and `RECIPIENT` or `SPLITS` | `SPLITS` is validated locally (1–20 recipients, bps summing to 10000) before any call |
| `tip.sh` | `NETWORK`, `SOURCE`, `JAR_ID`, `AMOUNT`, and optionally `FROM` and `MESSAGE` | `tip` takes the tipper and the message text as arguments; `FROM` defaults to `SOURCE` (only the submitting identity can sign) and `MESSAGE` to a placeholder, since the CLI rejects an empty message |
| `transfer-jar.sh` | `NETWORK`, `SOURCE`, `SLUG`, `NEW_OWNER` | Reads and prints the jar's current owner first, so a wrong `SOURCE` or contract is visible before the hand-over |
| `set-min-tip.sh` | `NETWORK`, `SOURCE`, `SLUG`, and optionally `MIN_TIP` | `MIN_TIP` is a positive integer in the token's smallest unit. Unset or `none` **clears** the minimum — which means invoking without the `--min_amount` flag at all, the only way to send `Option::None`. Zero, negative and non-numeric values are rejected locally |
| `set-fee.sh` | `NETWORK`, `SOURCE` (the contract admin), and optionally `FEE_BPS` | Sets the protocol fee taken from every tip before the split. `FEE_BPS` defaults to `100` (1%); `0` turns the fee off; above `500` (5%) is rejected locally and by the contract. Reads the value back |
| `pause-jar.sh` | `NETWORK`, `SOURCE` (the jar owner), `SLUG`, `PAUSED=true|false` | Pauses or resumes tipping and reads the resulting state back |
| `lock-splits.sh` | `NETWORK`, `SOURCE` (the jar owner), `SLUG`, `ACTION=lock|unlock` | Locks splits and verifies the state. Unlock requires the owner and every current recipient to sign; `ACTION=unlock` prints build, signing, and submission instructions without submitting an incomplete transaction |
| `leave-jar.sh` | `NETWORK`, `SOURCE` (the recipient), `SLUG`, and optionally `RECIPIENT` | Defaults `RECIPIENT` to the source identity address, checks membership, and prints splits before and after leaving |
| `withdraw-held.sh` | `NETWORK`, `SOURCE` (the recipient), `RECIPIENT`, `USDC_TOKEN` | Prints `get_held`, exits cleanly when it is zero, reuses `trustline.sh --check` to warn on a missing trustline, and reads the balance back after withdrawing |
| `allowlist.sh` | `NETWORK`, `SOURCE` (the contract admin), `TOKEN`, `ACTION=allow|disallow|check` | Refuses to disallow the default token locally (`CannotDisallowDefault`), and reads `is_token_allowed` back after allow/disallow |
| `pause-contract.sh` | `NETWORK`, `SOURCE` (the contract admin), `ACTION=pause|resume|status` | Prints `is_paused` before and after the call and reminds the operator which tip entry points are blocked |
| `transfer-admin.sh` | `NETWORK`, `SOURCE` (the current admin), `NEW_ADMIN` | Resolves `NEW_ADMIN` via `stellar keys` and Horizon, requires typing the address unless `YES=1`, and reads `get_admin` back after the transfer |

`ACTION` is shared by `lock-splits.sh`, `allowlist.sh` and `pause-contract.sh`;
set it per invocation rather than relying on one value in `.env`.

`set-min-tip.sh` reads the value back afterwards, since the CLI prints nothing
on success and clearing a jar that already had no floor is a silent no-op.

## Tech

- [Rust](https://www.rust-lang.org/) + [Soroban SDK](https://developers.stellar.org/docs/build/smart-contracts)
- [Stellar CLI](https://developers.stellar.org/docs/tools/cli) 27.1.0+ for build & deploy
- Tips settle in **USDC** (Stellar Asset Contract)

## Network Deployments

| Network | Contract ID | Source Commit | Status |
|---------|-------------|---------------|--------|
| Testnet | [`CCKPD2MPAYQYCL7QJKUMJHTEJWNHSIP7BZYIZBRO3C4DM4VHZKL6VBRL`](https://stellar.expert/explorer/testnet/contract/CCKPD2MPAYQYCL7QJKUMJHTEJWNHSIP7BZYIZBRO3C4DM4VHZKL6VBRL) | pre-`main` (older interface) | live, behind `main` |
| Mainnet | coming soon | — | pending audit |

> **Testnet deployment is behind `main`.** The deployed contract exposes only
> 9 of the 16 functions documented in [`docs/CONTRACT.md`](./docs/CONTRACT.md):
> `__constructor`, `create_jar`, `update_splits`, `transfer_jar_ownership`,
> `tip`, `get_jar`, `jar_exists`, `get_admin`, `get_token`.
>
> **Missing from the current testnet deployment:** `get_jar_owner`,
> `get_split_count`, `preview_split`, `is_recipient`, `get_limits`,
> `set_min_tip_amount`, `get_min_tip_amount`. Its `tip` and `jar_xfer` events
> also predate the payload changes listed as breaking in the changelog.
>
> To check the live interface yourself:
> ```bash
> stellar contract info interface --id CCKPD2MPAYQYCL7QJKUMJHTEJWNHSIP7BZYIZBRO3C4DM4VHZKL6VBRL --network testnet
> stellar contract info meta --id CCKPD2MPAYQYCL7QJKUMJHTEJWNHSIP7BZYIZBRO3C4DM4VHZKL6VBRL --network testnet
> ```
>
> To verify a deployment matches a local build, run
> `./scripts/verify-deployment.sh` (see issue #128).

The testnet deployment settles in the testnet USDC SAC below. The `@demo` jar
mentioned in earlier revisions may no longer be present — verify with
`get_jar` before relying on it.

USDC Stellar Asset Contract addresses:

| Network | USDC SAC |
|---------|----------|
| Testnet | `CBIELTK6YBZJU5UP2WWQEUCYKLPU6AUNZ2BQ4WWFEIE3USCIHMXQDAMA` |
| Mainnet | `CCW67TSZV3SSS2HXMBQ5JFGCKJNXKZM7UQUWUZPUTHXSTZLEO7SJMI75` |

## Changelog

See [CHANGELOG.md](./CHANGELOG.md) for a full history of changes.

## License

MIT — see [LICENSE](./LICENSE).
