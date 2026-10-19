# Changelog

All notable changes to `novatip-contracts` are documented here.

## [Unreleased]

### Added
- `version() -> u32` returns `INTERFACE_VERSION`, a compile-time constant bumped whenever the public interface changes shape (#169)
- `get_jar_stats(jar_id) -> JarStats { tip_count, total }` and `get_supporter_stats(jar_id, supporter) -> SupporterStats` report tip totals kept on-chain (#167, #168)
- `allow_token(token)` / `disallow_token(token)`, `is_token_allowed(token)`, and `tip_in_token(from, jar_id, token, amount, message)` for multi-asset tips (#170)
- `tip` returns the tip's sequence number; `get_tip_count() -> u64` exposes the running total
- `tip_with_ref(from, jar_id, amount, message, reference)` adds an optional opaque 32-byte client reference echoed in the `tip` event
- `transfer_jar_ownership(jar_id, new_owner)` and `jar_xfer` event
- `update_splits` emits a `splits` event
- `get_jar_owner(jar_id)`, `get_split_count(jar_id)`, `preview_split(jar_id, amount)`
- `get_admin()`, `set_min_tip_amount(jar_id, min_amount)`, `get_min_tip_amount(jar_id)`
- `min_tip` event and `BelowMinTipAmount` error (code 13)
- `scripts/transfer-jar.sh`, `scripts/set-min-tip.sh`, `scripts/update-splits.sh`
- Error codes 14 `TokenNotAllowed`, 15 `CannotDisallowDefault`, 16 `StatsOverflow`
- `get_min_accepted_tip(jar_id) -> i128` reports the smallest tip that pays every recipient a non-zero share, raised to the jar's minimum when that is larger
- `preview_split_details(jar_id, amount) -> Vec<PreviewSplit>` returns the preview paired with each recipient's address, so a client needs one call instead of `preview_split` plus `get_jar`
- Error codes 25 `AmountZero`, 26 `AmountNegative`, 27 `SplitAmountTooSmall`, 28 `AmountOverflow` and 29 `InvalidMinimumAmount`; `InvalidAmount` (5) is retired and no longer raised
- `transfer_admin(new_admin)`, `pause()`, `resume()`, `is_paused()` and `upgrade(new_wasm_hash)`; error codes 30 `AdminExists`, 31 `ContractPaused`
- `rename_jar(jar_id, new_jar_id)` moves a jar, its stats and its minimum to a new slug; error code 32 `JarIdInUse`
- Error codes 33 `NoFundsMoved` (a tip where every share was skipped) and 34 `InvalidRecipient` (contract or token used as recipient/owner)
- `create_jar` extends the contract instance TTL, and jar-scoped calls extend the jar and minimum in lockstep (#91, #92)
- `contracts/tip-splitter/interface.json` is now a populated public-interface snapshot instead of a placeholder

### Changed (breaking: event payload)
- Every event struct carries a leading `version: u32` field, currently `1`
- The `tip` event's data is the named `TipEvent` struct (`version`, `from`, `amount`, `message`, `breakdown`, `token`, `seq`, `reference`) on both payout paths; `collect_recurring` previously still published the positional tuple
- Invalid owners and recipients (the contract itself or its token) are rejected with `InvalidRecipient` (#94)
- `update_splits` is a no-op when the new splits equal the current ones (#100)
- The `tip` event's data tuple gained `token: Address` (5th element) and `tip_count: u64` (6th element)
- `tip` withholds a recipient's share when that recipient is also the tipper

### Fixed
- Restored the test suite that a truncated paste destroyed in the #195/#196 merge train: 22 test functions (including the proptest strategies) and every call site that decoded the `tip` event
- Repaired `compute_shares`, whose two competing bodies had been spliced together without a separator, so the crate did not compile
- `examples/tip-forwarder` builds for wasm again (`#![cfg_attr(not(test), no_std)]`)
- Zero-share splits rejected with `SplitOutOfRange` (was `InvalidSplits`)
- Split validation reports specific codes: `SplitsEmpty` (10), `SplitOutOfRange` (11), `SplitSumNot100Pct` (12)
- u32 overflow in split sum handled by `checked_add` instead of relying on `overflow-checks`

### Removed
- `InvalidSplits` (code 4) is no longer raised; variant retained for code stability

### Changed
- `0.1.0` release date corrected to match git tag date

## [0.1.0] - 2025-07-01

### Added
- `tip_splitter` Soroban contract (Rust) deployed on Stellar testnet
- `create_jar(owner, jar_id, splits)` - register a tip jar with basis-point splits
- `tip(from, jar_id, amount, message)` - atomic USDC split across all recipients
- `update_splits(jar_id, splits)` - replace a jar's collaborator splits
- `get_jar(jar_id)` - read a jar's on-chain configuration
- `get_token()` - return the configured USDC Stellar Asset Contract address
- Atomic payment routing with rounding dust sent to the last recipient
- Typed contract errors (`NotInitialized`, `JarExists`, `JarNotFound`, `InvalidSplits`, `InvalidAmount`, `TooManyRecipients`)
- `tip` event emission on every successful tip, with topics `(symbol "tip", jar_id)`
- Full test coverage for all contract functions
- Deploy scripts for testnet and mainnet (`scripts/deploy.sh`, `scripts/create-jar.sh`)
- GitHub Actions CI pipeline (fmt, clippy, test)
