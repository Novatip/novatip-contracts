# Changelog

All notable changes to `novatip-contracts` are documented here.

## [Unreleased]

### Added
- `get_jar_ids()` view function for indexer discovery of all registered jar slugs
- Edge case tests for single recipient tip, max recipients rejection, and jar ID tracking
- `JarIds` storage key to track registered slugs at the instance level
- `DuplicateRecipient` (code 7) error variant, raised when a split vector names the same address twice
- Tests for duplicate recipients (adjacent and non-adjacent) on `create_jar` and `update_splits`, plus valid-case coverage for distinct recipients and a full 20-recipient jar

### Changed
- `validate_splits()` now rejects any split vector containing the same recipient
  address more than once, on both `create_jar` and `update_splits`. Duplicates
  were not a loss-of-funds bug, but they made `tip` issue several transfers to
  one destination in a single call and forced per-collaborator accounting to
  de-duplicate after the fact. Clients that allow entering a collaborator twice
  must sum the shares before submitting. Jars written before this change are
  unaffected on read, but must drop duplicates before their next `update_splits`
  call.
- `create_jar_rejects_bad_bps_sum` now uses two distinct recipients, so it still
  exercises the sum check rather than tripping the new duplicate check first

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
- `TipReceived` event emission on every successful tip
- Full test coverage for all contract functions
- Deploy scripts for testnet and mainnet (`scripts/deploy.sh`, `scripts/create-jar.sh`)
- GitHub Actions CI pipeline (fmt, clippy, test)
