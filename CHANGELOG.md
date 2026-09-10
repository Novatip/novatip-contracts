# Changelog

All notable changes to `novatip-contracts` are documented here.

## [Unreleased]

### Fixed
- `extend_ttl` was called with two arguments where the SDK takes three
  (`key`, `threshold`, `extend_to`), so the crate did not compile. A
  `JAR_TTL_THRESHOLD` constant now supplies the missing bound, and the TTL is
  only bumped once the remaining lifetime drops below it rather than on every
  access
- The `JAR_TTL_LEDGERS` doc comment claimed 1 000 000 ledgers was "roughly 78
  years"; at the 5-second close rate it is roughly 58 days
- Event assertions in `test.rs` compared `Val` with `==`, which it does not
  implement. The payloads are now decoded to their concrete types, which also
  asserts the events decode to the shape consumers expect
- Split-validation tests still expected the removed catch-all `InvalidSplits`
  after error codes were split into `SplitsEmpty`, `SplitOutOfRange` and
  `SplitSumNot100Pct`
- The workspace used `resolver = "2"`, under which the `rust-version` pin is
  ignored when selecting dependency versions. Cargo resolved a `getrandom`
  requiring `edition2024`, which the pinned 1.84 toolchain cannot parse, so no
  build succeeded. Switching to `resolver = "3"` makes the MSRV pin bind, and
  `Cargo.lock` is committed with the resolved set
- `jar_id.len() == 0` replaced with `is_empty()` (clippy, `-D warnings`)

### Changed
- `rust-toolchain.toml` now lists `wasm32v1-none` alongside
  `wasm32-unknown-unknown`, so `rustup show` installs the target
  `stellar contract build` needs without a separate step
- `docs/CONTRACT.md` documents error codes 10, 11 and 12, records that code 4
  is retained but no longer raised, and notes that codes are appended rather
  than renumbered
- Added `CONTRIBUTING.md` and `.gitattributes`


### Deployed
- Testnet deployment at
  [`CCY2WPXROVUMPYAK3YJHZ57I35JKAUM5GDJKLUF5Y72KGENBQNSYAJIW`](https://stellar.expert/explorer/testnet/contract/CCY2WPXROVUMPYAK3YJHZ57I35JKAUM5GDJKLUF5Y72KGENBQNSYAJIW),
  settling in the testnet USDC SAC, with a `@demo` jar splitting 70/30

### Fixed
- `validate_splits` counted each entry's `bps` twice, so any valid split vector
  was rejected as `InvalidSplits` — a merge kept the accumulate line from two
  separate changes
- `test.rs` was missing the `MockAuth`, `MockAuthInvoke` and `IntoVal` imports
  the auth tests use, leaving the crate uncompilable
- `scripts/deploy.sh` looked for the wasm under `wasm32-unknown-unknown`; the
  Stellar CLI now builds to `wasm32v1-none`, so the path is discovered instead

### Added
- `jar_crtd` event published on every successful `create_jar`, carrying the jar
  slug and owner, so indexers can discover jars from the event log
- `DuplicateRecipient` (code 7) error variant, raised when a split vector names the same address twice
- Edge case tests for single recipient tip and max recipients rejection
- Tests covering zero-bps rejection on `create_jar` (leading, trailing, and sole entry) and on `update_splits`
- Tests for duplicate recipients (adjacent and non-adjacent) on `create_jar` and `update_splits`, plus valid-case coverage for distinct recipients and a full 20-recipient jar
- Test that `tip` fails when the sender's balance is insufficient
- Tests covering a share above 100% and a `bps` set whose sum overflows `u32`,
  on both `create_jar` and `update_splits`
- `jar_exists(jar_id) -> bool` view function, so a slug-availability check no
  longer has to call `get_jar` and catch the `JarNotFound` panic. Reads one
  persistent key, matches slugs exactly, and is cheap enough for the debounced
  onboarding check. Not yet exposed from `@novatip/sdk`.
- Tests covering `jar_exists` before and after registration, against a rejected
  jar, and across `update_splits`
- `MAX_MESSAGE_LEN` (280 bytes) bound on the `tip` message, with a new
  `MessageTooLong` error (code 8, appended so existing codes are unchanged)
- Tests covering a message one byte over the limit and one at exactly the limit
- `MAX_JAR_ID_LEN` (64 bytes) bound on `jar_id` in `create_jar`, with a new
  `InvalidJarId` error (code 9, appended so existing codes are unchanged).
  `jar_id` is used as a storage key, an event topic, and a public URL slug, so
  an empty or unbounded id costs unnecessary rent and can produce jars no
  frontend can address
- Tests covering an empty `jar_id`, one byte over the limit, and one at
  exactly the limit
- `scripts/create-jar.sh` now accepts a `SPLITS` env var carrying a full
  splits JSON array, so it can create multi-recipient jars instead of only
  the single-recipient case. `RECIPIENT` still works unchanged as shorthand
  for a 100% split. `SPLITS` is validated locally (valid JSON array, 1-20
  recipients, `bps` summing to exactly 10000) before invoking the contract
- `make optimize` target, and a matching optimize step in `scripts/deploy.sh`
  between build and deploy. The deployed wasm was previously the unoptimized
  build output, which permanently inflates both the upload cost and the
  contract code entry's ongoing state rent; the before/after byte size is now
  printed during deploy

### Changed
- Jar discovery moved from an on-chain list to the event log. The `get_jar_ids()`
  view and its `JarIds` instance-storage vector — both added and removed within
  this unreleased cycle, so never shipped in a release — are gone: the vector
  grew without bound and made each `create_jar` cost more than the last.
  Indexers reconstruct the jar list by scanning `jar_crtd` events; see
  [`docs/CONTRACT.md`](docs/CONTRACT.md) for the backfill procedure.
- `validate_splits()` now rejects any split with `bps == 0` (`InvalidSplits`).
  Such a recipient could never be paid but still consumed one of the 20
  recipient slots and appeared in clients as a collaborator. This applies to
  both `create_jar` and `update_splits`; jars written before this change are
  unaffected on read but must drop zero-bps entries before their next
  `update_splits` call.
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

### Fixed
- `validate_splits()` added each entry's `bps` to the running total twice, so
  splits summing to the required `10_000` were rejected as `InvalidSplits` and
  no valid jar could be created or updated. The zero-bps and duplicate-recipient
  branches were merged with both sides of the accumulator line left in place.

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
