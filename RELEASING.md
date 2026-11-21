# Releasing `tip-splitter`

There is no upgrade path for a deployed Soroban contract: a new version
means a new contract id, and every consumer (SDK, backend, web app) must
move to it deliberately. This document defines what counts as breaking,
how it maps to the version number, and the steps for a release.

## What is breaking

Any of the following is a breaking change:

- **Function signatures** — adding, removing or renaming a public function,
  or changing its parameters, auth requirements or return type
  (e.g. adding `preview_split`, removing `get_jar_ids`).
- **Error code values** — renumbering an existing variant or reusing a
  retired number for a different meaning. New variants are always
  **appended**; existing values never move (code 4 stays reserved even
  though nothing raises it anymore).
- **Event topics and payloads** — changing a topic symbol or the shape of
  the data (e.g. the `tip` tuple gaining `breakdown`, `jar_xfer` going
  from a bare address to `(prev_owner, new_owner)`).
- **Storage layout** — changing `DataKey` variants, the shape of a stored
  `Jar`, or TTL constants in a way that alters what is on ledger.

Docs-only and comment-only changes are not breaking. Pure gas or code-size
changes that keep signatures, codes, events and layout identical are not
breaking either — but they still get a release if they are deployed, so
there is a hash to pin against.

## Version numbers (pre-1.0)

The crate version lives in `contracts/tip-splitter/Cargo.toml` (currently
`0.1.0`); the tag is `v` plus that version. While the major version is `0`:

- **Breaking change** → bump the minor: `0.1.0` → `0.2.0`.
- **Backwards-compatible change** (new view, new error code appended, bug
  fix that keeps the interface above identical) → bump the patch:
  `0.1.0` → `0.1.1`.

`1.0.0` is reserved for the audited mainnet contract.

## Release steps

1. **Bump the crate version** in `contracts/tip-splitter/Cargo.toml`
   per the rule above.
2. **Bump `INTERFACE_VERSION` and regenerate `interface.json`** if any public
   function, error code, event, or storage type changed:
   - Increment `const INTERFACE_VERSION: u32` in
     `contracts/tip-splitter/src/lib.rs`.
   - Regenerate `contracts/tip-splitter/interface.json` and verify it matches
     the compiled contract:
     ```bash
     make interface
     ./scripts/check-interface.sh
     ```
3. **Cut the changelog section**: rename the `## [Unreleased]` heading in
   `CHANGELOG.md` to `## [X.Y.Z] - YYYY-MM-DD` and start a fresh empty
   `## [Unreleased]` above it.
4. **Run `make ci`** and commit the bump, `interface.json`, the changelog and
   any code.
5. **Tag and push**: `git tag vX.Y.Z && git push origin main --tags`.
   The tag must equal the crate version — the release workflow fails
   otherwise.
6. **Let the release workflow build the artifact.** Pushing the `v*` tag
   triggers `.github/workflows/release.yml`, which checks the tag against
   the crate version, builds with the pinned toolchain (**Rust 1.84.0**,
   per `rust-toolchain.toml`) and the pinned Stellar CLI
   (**27.1.0**, `STELLAR_CLI_VERSION` in the workflow) exactly as
   `scripts/deploy.sh` does (`stellar contract build --optimize`), and creates
   a GitHub release holding:
   - the optimized wasm (`tip_splitter.optimized.wasm`),
   - its SHA-256 (`tip_splitter.optimized.wasm.sha256`),
   - the contract interface spec (`interface.json`),
   - the changelog section for that version as the release notes,
   plus a build provenance attestation. Building the same tag twice
   produces the same hash (pinned toolchain + CLI + `Cargo.lock`).
7. **Record the hash**: the released SHA-256 lives in the GitHub release
   — both as the `.sha256` asset and in the release notes. Do not paste
   hashes into the README; the release page is the source of truth.
8. **Deploy to testnet from the published artifact** — never from a local
   build:
   ```bash
   gh release download vX.Y.Z --pattern 'tip_splitter.optimized.wasm*'
   sha256sum -c tip_splitter.optimized.wasm.sha256
   stellar contract deploy --wasm tip_splitter.optimized.wasm \
     --source "$SOURCE" --network testnet -- --admin "$ADMIN" --token "$USDC_TOKEN"
   ```
9. **Update the `Network Deployments` table in `README.md`** with the new
   testnet contract id, source commit/tag, and status, verify the live
   deployment against the release, and open the consumer updates that need the
   new id (`novatip-sdk`, `novatip-backend`, `novatip-web`):
   ```bash
   ./scripts/verify-deployment.sh
   stellar contract info interface --id <CONTRACT_ID> --network testnet
   ```

## Checking a deployment against the release

```bash
gh release download vX.Y.Z --pattern 'tip_splitter.optimized.wasm.sha256'
stellar contract fetch --id <CONTRACT_ID> --network testnet --out-file onchain.wasm
sha256sum onchain.wasm
# compare with the released hash; identical output means identical code
```
