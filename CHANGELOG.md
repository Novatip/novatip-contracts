# Changelog

All notable changes to `novatip-contracts` are documented here.

## [Unreleased]

### Added
- `get_jar_ids()` view function for indexer discovery of all registered jar slugs
- Edge case tests for single recipient tip, max recipients rejection, and jar ID tracking
- `JarIds` storage key to track registered slugs at the instance level
- Tests covering zero-bps rejection on `create_jar` (leading, trailing, and sole entry) and on `update_splits`

### Changed
- `validate_splits()` now rejects any split with `bps == 0` (`InvalidSplits`).
  Such a recipient could never be paid but still consumed one of the 20
  recipient slots and appeared in clients as a collaborator. This applies to
  both `create_jar` and `update_splits`; jars written before this change are
  unaffected on read but must drop zero-bps entries before their next
  `update_splits` call.

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
