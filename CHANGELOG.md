# Changelog

All notable changes to `novatip-contracts` are documented here.

## [Unreleased]

### Added
- `tip_many(from, tips) -> Vec<u64>` tips several jars in one transaction from a single authorisation, each entry carrying its own amount and message and publishing its own `tip` event (#84). Bounded by `MAX_BATCH_TIPS` (10); the whole call is all-or-nothing. New `TipRequest { jar_id, amount, message }` type, new errors `BatchEmpty` (37) and `BatchTooLarge` (38), and `get_limits().max_batch_tips`
- Protocol fee (#79): `set_fee_bps(fee_bps)` (admin only, default `0`, capped at 500 bps / 5%) and `get_fee_bps() -> u32`. The fee is taken from each tip before the split and paid to the admin; recipients' total plus the fee equals the tip exactly. New `fee_set` event, new error `FeeTooHigh` (35), and `scripts/set-fee.sh` (defaults to the intended 100 bps)
- Tests for authority moving with jar ownership (#75), multi-byte messages at the 280-byte limit (#74), and a tipper who holds a share in the distribution property test (#78)
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
- `get_limits() -> Limits` reports `max_jar_id_len`, the longest accepted jar id (`MAX_JAR_SLUG_LEN + 1`, i.e. `@` plus a 32-character body), so onboarding can validate a slug against the contract's real bound instead of hardcoding 64 (#98). The returned map gains a field; a client decoding `Limits` into a fixed three-field type must add it. With the canonical charset from #95 the effective maximum is 33 bytes, not the 64-byte `MAX_JAR_ID_LEN` storage cap

### Changed (breaking: jar ids)
- `create_jar` and `rename_jar` accept only canonical slugs — `@` followed by 3–32 characters from `[a-z0-9_-]`, matching the backend's `SLUG_REGEX` (#95). Upper case, whitespace, control bytes and lookalikes are rejected with `InvalidJarId`; `jar_exists` returns `false` for them.

### Changed (breaking: event payload)
- `version()` is now `4`: the `tip_many` entry point, the `TipRequest` type and the `BatchEmpty` (37) / `BatchTooLarge` (38) errors were added to the public interface (#84). No existing function, error number or event shape changed
- The `tip` event (`TipEvent`) gained a `fee: i128` field: the protocol fee the tip moved to the admin (#79). `version()` is now `3`
- `collect_recurring` publishes the same `TipEvent` struct as `tip`; it was still publishing the old positional tuple
- A tipper who is also a recipient now keeps their own share; it used to roll into the last recipient, so they paid it out to someone else (#78). `preview_tip` reports the new amounts
- The `tip` event's data tuple gained `token: Address` (5th element) and `tip_count: u64` (6th element)
- `tip` withholds a recipient's share when that recipient is also the tipper

### Fixed
- Workspace wasm builds and `cargo deny check` work again: `examples/tip-forwarder` declares `no_std` outside tests (the wasm builds failed with `E0152: duplicate lang item panic_impl`), both crates declare the workspace MIT license, and the path dependency carries a version instead of a wildcard
- `preview_split` checks the amount's sign before the jar's minimum, so a zero or negative amount reports `AmountZero`/`AmountNegative` exactly as `tip` does (#97)
- `tip` and `create_recurring` reject messages that are not valid UTF-8 or that contain control characters other than newline, before any transfer, with the appended error 36 `InvalidMessage` (#96)
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
