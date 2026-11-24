# Changelog

All notable changes to `novatip-contracts` are documented here.

## [Unreleased]

### Fixed

- Supporter totals are keyed by an internal jar uid instead of the reusable slug: after `delete_jar` plus `create_jar` the new jar starts with no supporter history, and `rename_jar` moves the totals with the jar. Jars created before this change keep their slug-keyed totals (#225).
- `rename_jar` requires the registrar's co-signature over `(new_jar_id, owner)` while a registrar is set, closing the squatting gap `create_jar` already guarded (#224).
- `delete_jar` on a locked jar needs authorization from every current recipient, like `unlock_splits`, so a lock cannot be bypassed by deleting and re-creating the jar (#223).
- `collect_recurring` fails with `ContractPaused` while the contract is paused and `JarPaused` while the jar is paused, before any state change, so `next_due_ledger` is unchanged and the same call works after the pause is lifted (#220).
- A protocol fee the payee cannot receive is escrowed for them instead of failing the tip or recurring collection; the payee collects it with `withdraw_held` and a `fee_held` event records it. `version()` is now `14` (#226).
- Recurring schedules are bound to the jar's internal uid: after `delete_jar` plus `create_jar` on the same slug, or a `rename_jar` followed by someone claiming the old slug, `collect_recurring` fails with the new `JarChanged` (48) and moves no funds. `version()` is now `13` (#222).
- `create_recurring` and `collect_recurring` reject a plan that moves no funds with `NoFundsMoved`, so a supporter who is a jar's only recipient can no longer inflate totals, sequence numbers and `tip` events (#221).
- Held balances are keyed by `(recipient, token)`: `get_held` and `withdraw_held` take the token, `withdraw_held` pays only that token's balance, and the `withdraw` event is now version 2 with a `token` field. `version()` is now `12`. Balances held under the previous single-token key are not migrated (#219).

- Tippers who are also recipients are charged only for funds that leave their account (#229).
- `transfer_admin` now requires the proposed administrator's authorization before changing control (#230).
- `preview_split_details` applies the same amount, minimum, maximum and paused-jar validation as tipping previews (#227, #228).

### Fixed

- `NotAuthorized` is now error code 44. It had been added at 40, colliding with `TooManyAllowedTokens`; as the last appended variant it takes the next free number, so no existing code is renumbered. The docs table, `interface.json` and the pinned-code test were updated to match.
- Repaired test modules that did not compile after prior merges (unclosed `MigratedEvent` and test functions, an interleaved escrow test and a stale `tip_many` call), and refreshed assertions that had drifted from the contract's behavior.
- Renew jar minimum-tip storage together with the jar, reject non-SAC addresses in the token allowlist, pass the paid token to post-tip hooks, and report constructor token misconfiguration with a typed error (#231, #232, #233, #234).

### Documentation

- `InvalidAmount` (5) is documented as retired in code and in the error and `preview_tip` tables, naming codes 25–29 as its replacements (#240).
- Rewrote "Detecting the interface version" as a table with one row per interface version from 1 to 10 (#271).
- Added Events sections for `pause`, `resume`, `upgrade`, `tk_allow`, `tk_dis` and `fee_set`, example payloads for `adm_xfer` and `jar_rnm`, and `jar_rnm` support in `scripts/events.sh` (#272).
- Added a table of which state-changing entry points the contract pause and the jar pause block; `pause` now names `tip_many` (#273).
- Added "When a share is held", covering the cause, check, fix, withdrawal and single-token limit of held shares (#274).
- Added a multi-recipient jar creation and read-back example for the deployed testnet contract (#54).
- Documented all storage keys, their readers, writers, TTL renewal, and archive behavior (#283).
- Documented `tip_many` preflight and failure-isolation guidance (#282).
- Added a `cargo doc` examples module with a representative call for every public contract function, including building the split vector for `create_jar` (#59).
- Documented the split rounding rule with a worked example: the last recipient absorbs the remainder, so entry order matters (#60).

### Added
- `version()` is now `11`: a locked jar can recover from one unreachable recipient with `request_recipient_removal`, `cancel_recipient_removal` and `execute_recipient_removal` (owner announces, `rm_req` event, 120 960-ledger delay during which the recipient can veto, then the share moves to the last recipient and the lock stays on), with new errors `RemovalNotRequested` (46) and `RemovalNotReady` (47) (#239).
- `transfer_jar_ownership` to the jar's current owner now fails with the new `SameOwner` (45) instead of `InvalidRecipient`, which is kept for the contract and token addresses (#241).
- `version()` is now `10`: `change_recipient(jar_id, old, new)` lets a recipient replace their own address in a jar without the owner, keeping bps, position and any lock, and publishes a `splits` event (#310); `withdraw_held_to(recipient, to)` pays a held balance to another address with a `wd_to` event carrying both (#311); `set_fee_recipient(Option<Address>)` (admin only) and `get_fee_recipient()` send protocol fees to a configurable address, defaulting to the admin, with a `fee_rcpt` event, and the fee self-transfer rule now compares the tipper with the fee recipient (#312). No new error codes
- `scripts/upgrade.sh` uploads a wasm, checks its hash, confirms on mainnet, calls `upgrade`, verifies the deployed wasm and prints `version()` before and after (#294)
- `scripts/recurring.sh` creates, shows, cancels and collects recurring tips, printing the token `approve` command and explaining `NotDue` with the next due ledger (#290)
- `scripts/delete-jar.sh` prints a jar and its totals, requires typing the slug to confirm, deletes it and checks `jar_exists` (#288)
- `scripts/rename-jar.sh` validates the new canonical slug, rejects a taken one and reads the jar back under its new id (#287)
- Added admin-authorized `admin_recover_jar` for lost owner keys and the distinct `jar_recov` audit event. This can reassign any jar without old/new-owner consent; splits and jar flags are preserved. `version()` is now `9` (#52).
- `tip_many(from, tips, token)` takes an optional `token` for the whole batch, checked against the allowlist like `tip_in_token`; the jar minimum applies only to the default token (#322). `TipRequest` gains `reference: Option<BytesN<32>>`, echoed in each entry's `tip` event (#318)
- `get_allowed_tokens() -> Vec<Address>` lists the allowlist, default first, capped at `MAX_ALLOWED_TOKENS` (10) with the appended error `TooManyAllowedTokens` (40) (#321)
- `set_max_tip_amount(jar_id, max_amount)` and `get_max_tip_amount(jar_id)`: an optional per-jar maximum, owner only, at least the minimum, enforced by default-token tips and previews, with a `max_tip` event and appended errors `AboveMaxTipAmount` (41) and `InvalidMaximumAmount` (42) (#323)
- `version()` is now `8`: `tip_many` gained a `token` argument and `TipRequest` a `reference` field, so existing callers must pass `None` for both (#318, #322)
- `migrate()` (admin only) moves stored state to the installed build's schema after an `upgrade`, once per `SCHEMA_VERSION`, and emits a `migrate` event with `MigratedEvent { version, admin, from_version, to_version }` (#325)
- `update_recurring(id, amount, message)` lets the supporter change a recurring tip without cancelling, keeping the id and `next_due_ledger`; validated like `create_recurring` and published as `recr_upd` (#316)
- `get_jar_info(jar_id) -> JarInfo` returns owner, splits, locked, paused, minimum tip, minimum accepted tip, totals and the current fee in one call (#319)
- `get_limits()` reports `min_jar_id_len` (4) and `max_fee_bps` (500) (#320)
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
- `version()` is now `8`: `cancel_recurring(caller, id)` takes the caller, which may be the supporter or the jar owner (#324); anyone else gets the new error `NotAuthorized` (40). `recr_del` data is now `(id, cancelled_by)`
- `version()` is now `8`: `recr_add` publishes `RecurringAddedEvent { version, id, jar_id, amount, period_ledgers, next_due_ledger }` instead of a bare id, and its topics are `(recr_add, supporter, jar_id)` so a creator can filter by jar (#317)
- `version()` is now `7`: every admin event (`tk_allow`, `tk_dis`, `tip_hook`, `reg_set`, `fee_set`, `pause`, `resume`, `adm_xfer`, `upgrade`) has only the event-name topic, and its data is one versioned struct whose first fields are `version` and `admin`, followed by the changed value (#333). `UpgradedEvent` also gained `version` and `previous_hash`; the first upgrade of a pre-existing deployment reports `None` because soroban-sdk 22 has no getter for the running code hash (#334)
- `version()` is now `7`: `lock`, `jar_pause`, `withdraw`, `adm_xfer` and `jar_rnm` publish named, versioned structs (`JarLockEvent`, `JarPausedEvent`, `HeldWithdrawnEvent`, `AdminTransferredEvent`, `JarRenamedEvent`) instead of bare values or positional tuples. `jar_del` topics are now `(jar_del, jar_id)` and `jar_rnm` topics are `(jar_rnm, old_jar_id)`, so a per-jar subscription sees renames and deletions. Consumers that decoded the old bare payloads or tuples must read the named fields.
- `version()` is now `6`: `leave_jar` publishes its `splits` event as `SplitsUpdatedEvent { version, count, splits }`, the same payload `update_splits` publishes. It was still publishing a bare `u32` count, so an indexer decoding every `splits` event with one type failed on it
- `version()` is now `5`: `set_jar_paused(jar_id, paused)`, `is_jar_paused(jar_id)` and `delete_jar(jar_id)` were added, with the appended error `JarPaused` (39). `contracts/tip-splitter/interface.json` is regenerated to match
- `version()` is now `4`: the `tip_many` entry point, the `TipRequest` type and the `BatchEmpty` (37) / `BatchTooLarge` (38) errors were added to the public interface (#84). No existing function, error number or event shape changed
- The `tip` event (`TipEvent`) gained a `fee: i128` field: the protocol fee the tip moved to the admin (#79). `version()` is now `3`
- `collect_recurring` publishes the same `TipEvent` struct as `tip`; it was still publishing the old positional tuple
- A tipper who is also a recipient now keeps their own share; it used to roll into the last recipient, so they paid it out to someone else (#78). `preview_tip` reports the new amounts
- The `tip` event's data tuple gained `token: Address` (5th element) and `tip_count: u64` (6th element)
- `tip` withholds a recipient's share when that recipient is also the tipper

### Fixed
- `NotAuthorized` was numbered 40, colliding with `TooManyAllowedTokens`, and `MigratedEvent` was missing its closing brace, so the crate did not compile; `NotAuthorized` is now 44 (#239, #241).
- Removed the unreachable `MAX_JAR_ID_LEN` (64) check from `is_valid_jar_id`; the canonical 4–33 byte rule is checked once (#242).
- The E2E job can run: the local Stellar network is started by a step with `--local` (a `services:` container cannot pass arguments to an image entrypoint, so quickstart defaulted to a public network and never passed its health check), the token is now deployed with `stellar contract asset deploy` because no bundled `stellar_asset.wasm` ships with the CLI any more, the CLI version is read from `scripts/lib/cli.sh` instead of pinned below the scripts' own floor, recipients are funded and given trustlines so their shares are paid rather than held, jar ids use the canonical `@`-prefixed form, and CLI output is read with `-q` so the assertions parse JSON instead of progress lines
- `scripts/tip.sh` calls `tip` with its `from` and `message` arguments, which the contract has always required; the script was written against an older signature and every invocation failed with `Missing required argument 'from'`
- Workspace wasm builds and `cargo deny check` work again: `examples/tip-forwarder` declares `no_std` outside tests (the wasm builds failed with `E0152: duplicate lang item panic_impl`), both crates declare the workspace MIT license, and the path dependency carries a version instead of a wildcard
- `preview_split` checks the amount's sign before the jar's minimum, so a zero or negative amount reports `AmountZero`/`AmountNegative` exactly as `tip` does (#97)
- `tip` and `create_recurring` reject messages that are not valid UTF-8 or that contain control characters other than newline, before any transfer, with the appended error 36 `InvalidMessage` (#96)
- Zero-share splits rejected with `SplitOutOfRange` (was `InvalidSplits`)
- Split validation reports specific codes: `SplitsEmpty` (10), `SplitOutOfRange` (11), `SplitSumNot100Pct` (12)
- u32 overflow in split sum handled by `checked_add` instead of relying on `overflow-checks`

### Removed
- `InvalidSplits` (code 4) is no longer raised; variant retained for code stability
- `JarRecord` and `CompactSplit` were removed from the exported interface; neither was reachable from a function argument or return type

### Changed
- The shipped wasm is ~34 KiB, down from ~65 KiB: Rust doc comments no longer ride along in `contractspecv0`, and the two unused exported types are gone (#329). `MAX_WASM_BYTES` is lowered from 128 KiB to 42 KiB
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
