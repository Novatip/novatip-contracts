# `tip_splitter` — contract interface

Receives a single USDC tip and splits it across one or more recipients by
basis-point shares, in one transaction. A recipient who cannot receive the token
no longer blocks the others: their share is escrowed by the contract for later
withdrawal.

## Glossary

- <a id="glossary-jar"></a>**jar** — a creator's tip target, registered under a unique slug with an `owner` and a list of recipient splits.
- <a id="glossary-slug"></a>**slug** — a jar's canonical public identifier (`jar_id`), formatted as `@` followed by 3–32 lowercase ASCII letters, digits, `_`, or `-`.
- <a id="glossary-split"></a>**split** — a `(to, bps)` entry mapping a recipient `Address` to its percentage share of each tip.
- <a id="glossary-bps"></a>**bps** — basis points (`1/100` of a percent, so `10_000 bps = 100%`).
- <a id="glossary-share"></a>**share** — the portion of a net tip allocated to one recipient according to their `bps`.
- <a id="glossary-held"></a>**held** — funds owed to a recipient that could not be transferred during a tip and are stored by the contract for later withdrawal.
- <a id="glossary-escrow"></a>**escrow** — the contract's internal holding mechanism (`DataKey::Held`) that retains undeliverable recipient shares until `withdraw_held` succeeds.
- <a id="glossary-keeper"></a>**keeper** — any off-chain caller or automation service that triggers permissionless `collect_recurring` executions when a schedule is due.
- <a id="glossary-registrar"></a>**registrar** — an optional address configured by the admin whose co-signature is required to register new jars via `create_jar`.
- <a id="glossary-hook"></a>**hook** — an optional post-tip contract (`set_tip_hook`) invoked after a tip settles, without blocking or reverting the tip.
- <a id="glossary-net"></a>**net** — the tip amount remaining after subtracting the protocol fee, distributed across recipients.
- <a id="glossary-gross"></a>**gross** — the total tip amount paid by the supporter before deducting the protocol fee.
- <a id="glossary-dust"></a>**dust** — the integer-division remainder left over from splitting `net` by `bps`, always assigned to the last recipient.
- <a id="glossary-sequence-number"></a>**sequence number** — a monotonically increasing contract-wide `u64` assigned to each successful tip and emitted in the `tip` event.

## Concepts

- **Jar** — a creator's tip target ([jar](#glossary-jar)), identified by a public [slug](#glossary-slug) (e.g. `@alice`).
  Holds an `owner` and a list of `Split`s ([split](#glossary-split)). A slug (`jar_id`) is canonical: a
  leading `@` followed by 3–32 lower-case ASCII letters, digits, `_` or `-`
  — exactly the backend's `SLUG_REGEX`. Anything else (upper case, whitespace,
  control bytes, a missing `@`, or a length outside that range) is rejected with
  `InvalidJarId`, so lookalikes cannot exist next to a real creator's slug.
  The storage bound is also `MAX_JAR_ID_LEN` (64 bytes) — it doubles as a
  storage key, an event topic, and a public URL slug.
- **Split** — a recipient `Address` and its [share](#glossary-share) in basis points ([`bps`](#glossary-bps)).
  Every split must have `bps >= 1`, all splits in a jar must sum to exactly
  `10_000` (= 100%), and no address may appear more than once.
- **USDC token** — the Stellar Asset Contract id is fixed at deploy time; every
  tip settles in that asset.
- **Message** — the free-text note a supporter attaches to a tip. Capped at
  `280` bytes (`MAX_MESSAGE_LEN`), must be valid UTF-8, and may not contain
  control characters other than newline; echoed into the `tip` event.
- **Sequence number** — a contract-wide `u64` ([sequence number](#glossary-sequence-number)), incremented once per successful
  tip, returned by `tip` and published in the `tip` event. Gives each tip an
  identifier of its own where the transaction hash does not.
- **Reference** — an optional 32-byte opaque id a tipper attaches with
  `tip_with_ref`, echoed in the `tip` event so the calling integration can
  match the on-chain event back to whatever it created off-chain. Never stored
  and never checked by the contract.

## Storage keys and lifetimes

`DataKey` is the complete set of contract storage keys. Persistent entries have
their own TTL: jar-related paths extend an entry to `JAR_TTL_LEDGERS` (1,000,000
ledgers) when it falls below `JAR_TTL_THRESHOLD` (500,000). Instance entries
share one contract-instance TTL; `create_jar` and a successful tip extend the
instance to the same target. An archived entry must be restored before it can
be read or written again. Instance entries archive together, while persistent
entries archive independently.

| `DataKey` variant | Storage | Writers | Readers | TTL extension | After archival |
|------------------|---------|---------|---------|---------------|-----------------|
| `Admin` | Instance | `__constructor`, `transfer_admin` | `get_admin`, `require_admin` and every admin-only function | With the shared instance TTL | Restore the contract instance before admin-gated calls can read it. |
| `Token` | Instance | `__constructor` | `get_token`, default-token tip and recurring paths | With the shared instance TTL | Restore the contract instance; there is no token setter. |
| `Jar(jar_id)` | Persistent | `create_jar`, `store_jar` used by owner, split, pause, lock and recovery changes | `load_jar`, jar reads, tips and recurring operations | `store_jar` and `load_jar` extend the jar | Restore to make the jar available again; event history remains available for reconstruction. |
| `MinTip(jar_id)` | Persistent | `set_min_tip_amount`, clear, and `rename_jar` | Tip validation, previews and minimum-tip reads | Set/clear, minimum reads, and `load_jar` renew it alongside the jar | Restore it to recover the configured floor; a never-set floor is `None`. |
| `Held(recipient)` | Persistent | `pay_out` when a recipient cannot receive a share | `get_held`, `withdraw_held` | A new held credit and positive `get_held` reads extend it | Restore to withdraw the balance; the event log alone cannot recreate a spendable held balance. |
| `Recurring(id)` | Persistent | `create_recurring`, `update_recurring`, `collect_recurring` | `get_recurring`, `cancel_recurring`, `update_recurring`, `collect_recurring` | Creation, update, read and collection extend it | Restore the schedule; otherwise collection reports `RecurringNotFound`. |
| `RecurringCount` | Persistent | `create_recurring` | `create_recurring` | Extended when a schedule is created | Restore the counter to preserve the next id; an absent counter is read as zero. |
| `Stats(jar_id)` | Persistent | Successful tips through `bump_totals`; `rename_jar` moves an existing entry | `get_jar_stats`, combined jar info | A successful tip and a positive stats read extend it | Restore to recover totals; if no stats entry has ever been written, the getter returns zero totals. |
| `Supporter(jar_id, supporter)` | Persistent | Successful tips through `bump_totals` | `get_supporter_stats` | A successful tip and a positive stats read extend it | Restore to recover totals; a never-written entry reads as zero totals. |
| `AllowedToken(token)` | Instance | `__constructor`, `allow_token`, `disallow_token` | `is_token_allowed`, `tip_in_token` | With the shared instance TTL | Restoring the contract instance restores the allowlist; the default token is initially allowed. |
| `TipCount` | Instance | Successful tip settlement | Sequence assignment and `get_tip_count` | With the shared instance TTL | Restore the instance to preserve the sequence counter. |
| `TipHook` | Instance | `set_tip_hook` | `get_tip_hook` and successful tip settlement | With the shared instance TTL | Restore the instance; an absent hook means no hook is configured. |
| `Registrar` | Instance | `set_registrar` | `get_registrar` and `create_jar` | With the shared instance TTL | Restore the instance; an absent registrar means no co-signature is required. |
| `Paused` | Instance | `pause`, removed by `resume` | `is_paused` and tip entry points | With the shared instance TTL | Restore the instance; absent pause state means the contract is not paused. |
| `FeeBps` | Instance | `set_fee_bps` | `get_fee_bps` and tip settlement | With the shared instance TTL | Restore the instance; absent fee state defaults to zero. |
| `WasmHash` | Instance | Successful `upgrade` | The next `upgrade` event | With the shared instance TTL | Restore the instance; absent history is reported as `previous_hash: None`. |
| `MaxTip(jar_id)` | Persistent | `set_max_tip_amount`, clear, and `rename_jar` | Tip validation, previews and maximum-tip reads | Set/read and tip paths extend the entry | Restore to recover the cap; a never-set maximum is `None`. |
| `AllowedTokens` | Instance | `__constructor`, `allow_token`, `disallow_token` | `get_allowed_tokens` | With the shared instance TTL | Restore the instance; older deployments without the list fall back to the default token. |
| `SchemaVersion` | Instance | `migrate` | `migrate` | With the shared instance TTL | Restore the instance; an absent version is treated as zero so migration can run. |

Deleting a jar explicitly removes its `Jar`, `Stats`, `MinTip` and `MaxTip`
entries. `Supporter`, `Held`, recurring schedules, and contract-wide counters
have independent keys and are not removed by `delete_jar`.

## Limits

Quick reference for bounds enforced by the contract:

| Limit | Value | Constant | Error when exceeded |
|---|---|---|---|
| Basis point denominator | `10_000` | `BPS_DENOM` | `SplitOutOfRange` (11), `SplitSumNot100Pct` (12) |
| Maximum recipient count | `20` | `MAX_RECIPIENTS` | `TooManyRecipients` (6) |
| Maximum message length | `280` bytes | `MAX_MESSAGE_LEN` | `MessageTooLong` (8) |
| Maximum batch tips | `10` | `MAX_BATCH_TIPS` | `BatchTooLarge` (38) |
| Maximum jar id length | `33` bytes (`@` + 32) | `MAX_JAR_SLUG_LEN + 1` | `InvalidJarId` (9) |
| Minimum jar id length | `4` bytes (`@` + 3) | `MIN_JAR_SLUG_LEN + 1` | `InvalidJarId` (9) |
| Maximum protocol fee | `500` bps (5%) | `MAX_FEE_BPS` | `FeeTooHigh` (35) |

## Types

```rust
struct Split    { to: Address, bps: u32 }
struct Jar      { owner: Address, splits: Vec<Split>, locked: bool, paused: bool }
struct Limits   {
    bps_denom: u32, max_recipients: u32, max_message_len: u32, max_batch_tips: u32,
    max_jar_id_len: u32, min_jar_id_len: u32, max_fee_bps: u32,
}
struct JarInfo {
    owner: Address, splits: Vec<Split>, locked: bool, paused: bool,
    min_tip_amount: Option<i128>, min_accepted_tip: i128,
    tip_count: u64, total: i128, fee_bps: u32,
}
struct Recurring {
    supporter: Address, jar_id: String, amount: i128,
    period_ledgers: u32, message: String, next_due_ledger: u32,
}
struct Split          { to: Address, bps: u32 }
struct Jar            { owner: Address, splits: Vec<Split> }
struct Limits         { bps_denom: u32, max_recipients: u32, max_message_len: u32, max_batch_tips: u32 }
struct TipRequest     { jar_id: String, amount: i128, message: String, reference: Option<BytesN<32>> }
struct JarStats       { tip_count: u64, total: i128 }
struct SupporterStats { tip_count: u64, total: i128 }
```

`TipRequest` is one entry of a [`tip_many`](#tipping-many-jars-at-once-84) batch: the
slug to tip, the amount, and the message that rides along with that jar's `tip`
event, plus an optional 32-byte `reference` echoed in that entry's `tip`
event exactly as `tip_with_ref` echoes it (#318).

`JarStats` and `SupporterStats` are the on-chain totals behind
[reading tip totals](#reading-tip-totals). They share a shape but not a key:
one belongs to a jar, the other to a `(jar, supporter)` pair.

## Functions

| Function | Auth | Description |
|----------|------|-------------|
| `__constructor(admin, token)` | — | Deploy-time init. Stores the admin and USDC token address, and puts that token on the tip allowlist as its first entry. |
| `create_jar(owner, jar_id, splits)` | `owner`, plus the [`registrar`](#glossary-registrar)'s co-signature over `(jar_id, owner)` while one is registered | Register a new jar. Fails if the slug is not a canonical `@`-prefixed `[a-z0-9_-]{3,32}` slug, already exists, or splits are invalid. Emits a `jar_crtd` event. See [the registrar gate](#the-jar-registrar-and-its-trust-model-178). |
| `update_splits(jar_id, splits)` | jar `owner`, plus every current recipient whose share **decreases or is removed** when the jar is locked | Replace a jar's splits. Subject to the same validation as `create_jar`. On a locked jar the owner alone cannot cut a collaborator — see [`lock_splits`](#locking-a-jars-splits). Emits a `splits` event. |
| `change_recipient(jar_id, old, new)` | `old` | Replace `old` with `new` in the jar's splits, keeping the same bps and position (#310). The owner is not needed, and it works on a locked jar (the lock is kept). Rejects a `new` already in the jar (`DuplicateRecipient`), the contract or token address (`InvalidRecipient`), and an `old` that is not a recipient (`NotRecipient`). Emits a `splits` event. |
| `lock_splits(jar_id)` | jar `owner` | Lock a jar so `update_splits` needs each reduced/removed recipient's consent. Emits a `lock` event with `JarLockEvent { version, locked: true }` data. |
| `unlock_splits(jar_id)` | jar `owner` **and every current recipient** | Remove the lock. Consent mirrors a reduction: all recipients must sign, or the owner could disarm the lock and then cut shares. Emits a `lock` event with `JarLockEvent { version, locked: false }` data. |
| `is_splits_locked(jar_id) -> bool` | — | Whether the jar's splits are locked. Panics with `JarNotFound` if the slug is free. |
| `set_jar_paused(jar_id, paused)` | jar `owner` | Pause (`true`) or resume (`false`) tipping for one jar (#45). While paused, every tip entry point rejects the jar with `JarPaused`; reads stay available. The flag survives `transfer_jar_ownership`. Emits a `jar_pause` event with `JarPausedEvent { version, paused }` data. See [pausing a jar](#pausing-a-jar-45). |
| `is_jar_paused(jar_id) -> bool` | — | Whether tipping is currently paused for the jar. Panics with `JarNotFound` if the slug is free. |
| `leave_jar(jar_id, recipient)` | `recipient` | Remove `recipient` from the jar's splits. Their share is added to the last remaining entry so the splits still sum to 10 000. Rejected with `SoleRecipient` (only entry) or `NotRecipient` (not in the jar). Emits a `splits` event. |
| `set_min_tip_amount(jar_id, min_amount)` | jar `owner` | Set or clear an optional minimum tip amount for a jar. `min_amount` must be strictly positive (`> 0`) or `None` to clear. Emits a `min_tip` event. |
| `set_max_tip_amount(jar_id, max_amount)` | jar `owner` | Set or clear an optional maximum tip amount for a jar (#323). `max_amount` must be positive and no lower than the jar's minimum (`InvalidMaximumAmount`), or `None` to clear. Tips in the default token and previews above it are rejected with `AboveMaxTipAmount`. Emits a `max_tip` event with `MaxTipEvent { version, amount }` data. |
| `get_max_tip_amount(jar_id) -> Option<i128>` | — | Read a jar's optional maximum tip amount. Panics with `JarNotFound` if the slug is free. |
| `transfer_jar_ownership(jar_id, new_owner)` | current jar `owner` and `new_owner` | Hand control of a jar to `new_owner`. The exact `(jar_id, new_owner)` pair must be signed by both parties; a transfer to the current owner is rejected. Splits are unchanged. Emits a `jar_xfer` event carrying both owners. |
| `admin_recover_jar(jar_id, new_owner)` | contract admin | Reassign a jar whose owner key is lost. No owner signature is required; shares and jar flags are preserved. Emits `jar_recov`. |
| `delete_jar(jar_id)` | current jar `owner` | Delete a registered jar and free its slug so it can be recreated. Removes the jar, its stats, and any minimum-tip value for that slug. Emits a `jar_del` event with the jar id as a topic. |
| `rename_jar(jar_id, new_jar_id)` | jar `owner` | Move a jar, its stats and its minimum to a new slug. The old slug stops resolving immediately; `new_jar_id` must be free and pass the same length checks as `create_jar`. Emits a `jar_rnm` event with the old jar id as a topic and `JarRenamedEvent { version, jar_id, new_jar_id }` data. |
| `tip(from, jar_id, amount, message)` | `from` | Move `amount` USDC out of `from` and route it across the jar's recipients. A recipient who cannot receive the token has their share **escrowed in the contract** (see `get_held`/`withdraw_held`) instead of reverting the tip. Emits a `tip` event carrying the per-recipient breakdown. Rejects amounts below the jar's minimum with `BelowMinTipAmount`. |
| `get_held(recipient) -> i128` | — | How much the contract is holding ([held](#glossary-held)) for `recipient` because a past tip could not deliver their share. `0` when nothing is held. |
| `withdraw_held(recipient)` | `recipient` | Pay a recipient their held balance once they can receive again. Rejected with `NothingHeld` when nothing is owed. Emits a `withdraw` event with `HeldWithdrawnEvent { version, amount }` data. |
| `withdraw_held_to(recipient, to)` | `recipient` | Pay `recipient`'s held balance to `to` instead (#311), for an account that can no longer receive the token. Rejects `to` equal to the contract or the token (`InvalidRecipient`) and `NothingHeld` when nothing is owed. Emits a `wd_to` event with topics `(wd_to, recipient)` and `HeldWithdrawnToEvent { version, amount, to }` data. |
| `create_recurring(supporter, jar_id, amount, period_ledgers, message) -> u128` | `supporter` | Register a recurring-tip schedule. Validates the jar exists and the amount/message pass the same checks `tip` applies; `period_ledgers` must be non-zero. Assigns a fresh id, stores the entry with its first `next_due_ledger`, and emits a `recr_add` event. Returns the id. The schedule moves funds through `collect_recurring`: the supporter must have approved this contract as a token spender for at least `amount` per period. |
| `cancel_recurring(caller, id)` | `caller`: the entry's `supporter` or the jar's `owner` | Remove a recurring-tip record. Either party may cancel (#324); anyone else gets `NotAuthorized`. Emits a `recr_del` event naming who cancelled. |
| `cancel_recurring(id)` | the entry's `supporter` | Remove a recurring-tip record. Emits a `recr_del` event. |
| `update_recurring(id, amount, message)` | the entry's `supporter` | Change a recurring tip's amount and message in place (#316). Keeps the id and `next_due_ledger`. Validated like `create_recurring` (`AmountZero` / `AmountNegative` → `MessageTooLong` / `InvalidMessage` → `JarNotFound` → `BelowMinTipAmount` → split arithmetic); `RecurringNotFound` for an unknown id. Emits a `recr_upd` event. |
| `get_recurring(id) -> Recurring` | — | Read a stored recurring-tip entry. Panics with `RecurringNotFound` for an unknown id. |
| `collect_recurring(id)` | — (any caller, typically a [keeper](#glossary-keeper)) | Collect **one due period** of a recurring tip (#175). Pays the schedule's `amount` from the supporter's approved allowance with `transfer_from` (this contract is the spender), split by the same arithmetic as `tip`, and publishes the identical `tip` event. `next_due_ledger` advances by exactly `period_ledgers`. Too early — `NotDue`; allowance below `amount` (exhausted or expired) — `InsufficientAllowance`; supporter balance below `amount` — `InsufficientBalance`; all raised before any transfer, with zero state change. See [collecting recurring tips](#collecting-a-due-recurring-tip-175). |
| `tip(from, jar_id, amount, message)` | `from` | Transfer `amount` of the **default token** from `from`, split across the jar's recipients. Emits a `tip` event carrying the per-recipient breakdown and the token paid in. Rejects amounts below the jar's minimum with `BelowMinTipAmount`. |
| `tip_in_token(from, jar_id, token, amount, message)` | `from` | Same split, paid in `token`, which must be on the allowlist — otherwise `TokenNotAllowed`, raised before any transfer. The jar minimum applies only when `token` is the default token. Emits the same `tip` event. |
| `allow_token(token)` | contract `admin` | Add `token` to the allowlist. Idempotent. Emits a `tk_allow` event in the shared admin shape, carrying the admin and the allowed token. |
| `disallow_token(token)` | contract `admin` | Remove `token` from the allowlist. Refuses the default token with `CannotDisallowDefault`. Emits a `tk_dis` event in the shared admin shape, carrying the admin and the token. |
| `set_tip_hook(hook)` | contract `admin` | Register or clear (`None`) the post-tip [hook](#glossary-hook) contract (#176). Emits a `tip_hook` event carrying the admin who made the change. See [the post-tip hook](#the-post-tip-hook-176). |
| `get_tip_hook() -> Option<Address>` | — | The registered hook, or `None` when tips settle without one. |
| `set_registrar(registrar)` | contract `admin` | Register or clear (`None`) the registrar whose co-signature `create_jar` requires (#178). Emits a `reg_set` event carrying the admin. See [the registrar gate](#the-jar-registrar-and-its-trust-model-178). |
| `get_registrar() -> Option<Address>` | — | The registrar `create_jar` currently requires, or `None` when jar creation is permissionless. |
| `set_fee_bps(fee_bps)` | contract `admin` | Set the protocol fee taken from every tip before the split, in basis points (#79). Defaults to `0`; above `500` (5%) is rejected with `FeeTooHigh`. The fee is paid to the fee recipient (the admin unless `set_fee_recipient` set one). Emits a `fee_set` event. See [the protocol fee](#the-protocol-fee-79). |
| `get_fee_bps() -> u32` | — | The protocol fee currently taken from every tip, in basis points. `0` until the admin sets one. |
| `set_fee_recipient(recipient)` | contract `admin` | Send protocol fees to `recipient` instead of the admin, or `None` to go back to the admin (#312). Rejects the contract or token address (`InvalidRecipient`). A tip from the fee recipient moves no fee (the self-transfer rule). Emits a `fee_rcpt` event in the shared admin shape. |
| `get_fee_recipient() -> Address` | — | Where protocol fees are paid: the configured recipient, or the admin when none is set. |
| `tip(from, jar_id, amount, message) -> u64` | `from` | Transfer `amount` USDC from `from`, split across the jar's recipients. Emits a `tip` event carrying the per-recipient breakdown and this tip's sequence number, and returns that number. Rejects amounts below the jar's minimum with `BelowMinTipAmount`. |
| `tip_with_ref(from, jar_id, amount, message, reference) -> u64` | `from` | Identical to `tip`, plus an optional 32-byte opaque `reference` echoed in the `tip` event. Same transfers, same validation, same counter — both entry points call one internal implementation. |
| `tip_many(from, tips, token) -> Vec<u64>` | `from` | Tip several jars in one transaction, each with its own amount, message and optional reference. `token` is `None` for the default token, or one allowed token for the whole batch (#322), checked like `tip_in_token` before any transfer (`TokenNotAllowed`); the jar minimum applies only to the default token. `from` authorises once for the whole batch, and every entry settles exactly as a single `tip` would — same validation, same minimum-tip check, same protocol fee, same self-transfer skip — and publishes its own `tip` event. The batch is bounded by `MAX_BATCH_TIPS` (`BatchTooLarge`); an empty batch is `BatchEmpty`. **All-or-nothing**: a failure in any jar reverts the whole call, leaving the earlier jars untouched. Returns one sequence number per entry, in order. See [tipping many jars at once](#tipping-many-jars-at-once-84). |
| `get_tip_count() -> u64` | — | How many tips the contract has taken, i.e. the number the next tip will receive. `0` on a fresh deploy. |
| `get_jar(jar_id) -> Jar` | — | Read a jar's configuration. Panics with `JarNotFound` if the slug is free. |
| `get_jar_owner(jar_id) -> Address` | — | Read just a jar's owner. Panics with `JarNotFound` if the slug is free. |
| `get_split_count(jar_id) -> u32` | — | How many recipients a jar pays. Panics with `JarNotFound` if the slug is free. |
| `preview_split(jar_id, amount) -> Vec<i128>` | — | What each recipient would receive from a tip of `amount`, in split order. Rejects amounts below the jar's minimum with `BelowMinTipAmount` as well as the amounts `tip` rejects; panics with `JarNotFound` if the slug is free. |
| `preview_split_details(jar_id, amount) -> Vec<PreviewSplit>` | — | The same preview paired with each recipient's address, so a client that renders the breakdown needs one call instead of `preview_split` plus `get_jar`. Validates exactly as `preview_split` does. |
| `get_min_accepted_tip(jar_id) -> i128` | — | The smallest positive amount that pays every configured recipient a non-zero share, raised to the jar's minimum when that minimum is larger. Panics with `JarNotFound` if the slug is free. |
| `preview_tip(jar_id, from, amount) -> Vec<i128>` | — | What each recipient would **actually** receive from a `tip` of `amount` sent by `from` (#177), in split order: the sender-aware sibling of `preview_split`. A recipient who is the sender is reported as `0`, exactly as the `tip` event's breakdown records them, because their share is never transferred and stays with them. Computed by the same private helper the tip payout loop uses, so preview and payout cannot drift. Rejects what `tip` rejects. |
| `jar_exists(jar_id) -> bool` | — | Whether the slug is already registered. |
| `is_recipient(jar_id, address) -> bool` | — | Whether `address` appears in the jar's splits. Panics with `JarNotFound` if the slug is free. |
| `get_min_tip_amount(jar_id) -> Option<i128>` | — | Read a jar's optional minimum tip amount. Returns `None` if no minimum is set. Panics with `JarNotFound` if the slug is free. |
| `get_min_accepted_tip(jar_id) -> i128` | — | The smallest tip this jar accepts: the larger of the amount needed to give every recipient a non-zero share and the jar's configured minimum. See [the smallest amount a jar accepts](#the-smallest-amount-a-jar-accepts-86). Panics with `JarNotFound` if the slug is free. |
| `get_token() -> Address` | — | The USDC token address tips settle in. |
| `get_admin() -> Address` | — | The contract admin recorded at deploy time. |
| `transfer_admin(new_admin)` | contract `admin` | Hand admin control to `new_admin`. Rejected with `AdminExists` when it is already the current admin. Emits an `adm_xfer` event with `AdminTransferredEvent { version, admin, new_admin }` data. |
| `pause()` | contract `admin` | Pause tipping: `tip`, `tip_with_ref`, `tip_in_token` and `tip_many` reject with `ContractPaused`; see [what each pause blocks](#what-each-pause-blocks). Emits a `pause` event in the shared admin shape with `paused: true`. |
| `resume()` | contract `admin` | Lift a pause. Emits a `resume` event in the shared admin shape with `paused: false`. |
| `is_paused() -> bool` | — | Whether tipping is currently paused. Reads instance storage. |
| `upgrade(new_wasm_hash)` | contract `admin` | Replace the running contract Wasm while keeping the address and all stored state. Emits an `upgrade` event carrying the previous and new hashes. |
| `migrate()` | contract `admin` | Bring stored state up to this build's schema after an `upgrade` (#325). Runs once per schema version and emits a `migrate` event; a call with nothing to do changes nothing. |
| `get_limits() -> Limits` | — | The bounds this contract enforces: `bps_denom`, `max_recipients`, `max_message_len`, `max_batch_tips`. Reads no storage. |
| `get_limits() -> Limits` | — | The bounds this contract enforces: `bps_denom`, `max_recipients`, `max_message_len`, `max_jar_id_len`. Reads no storage. |
| `get_limits() -> Limits` | — | The bounds this contract enforces: `bps_denom`, `max_recipients`, `max_message_len`, `max_batch_tips`, `max_jar_id_len`, `min_jar_id_len`, `max_fee_bps`. Reads no storage. |
| `get_limits() -> Limits` | — | The bounds this contract enforces: `bps_denom`, `max_recipients`, `max_message_len`, `max_batch_tips`, `max_jar_id_len`, `min_jar_id_len`, `max_fee_bps`. Reads no storage. |
| `version() -> u32` | — | Which interface shape this deployment speaks. Reads no storage. See [detecting the interface version](#detecting-the-interface-version). |
| `get_jar_stats(jar_id) -> JarStats` | — | A jar's on-chain tip count and total. Zeros for a jar nobody has tipped; `JarNotFound` if the slug is free. |
| `get_jar_info(jar_id) -> JarInfo` | — | Everything a tip page needs in one call (#319): `owner`, `splits`, `locked`, `paused`, `min_tip_amount`, `min_accepted_tip`, `tip_count`, `total` and the current `fee_bps`. The same values as `get_jar`, `get_min_tip_amount`, `get_min_accepted_tip`, `get_jar_stats` and `get_fee_bps`; `JarNotFound` if the slug is free. |
| `get_supporter_stats(jar_id, supporter) -> SupporterStats` | — | One address's tip count and total against one jar. Zeros for an address that has never tipped it; `JarNotFound` if the slug is free. |
| `is_token_allowed(token) -> bool` | — | Whether the admin has put `token` on the tip allowlist. The default token is always allowed. |
| `get_allowed_tokens() -> Vec<Address>` | — | Every allowed token, default first (#321). The list holds at most 10 tokens (`MAX_ALLOWED_TOKENS`); `allow_token` beyond that is rejected with `TooManyAllowedTokens`. |
The **Errors** column lists the `tip_splitter` error codes a call can return,
**in the order the contract checks them** — only the first failure a call hits

### Admin powers and blast radius

Every privileged entry point requires the contract `admin` signature (`transfer_admin` hands that role to a new address):

| Admin function | What it can affect | What it can never do |
| -------------- | ------------------ | -------------------- |
| `pause()` / `resume()` | Halts or resumes `tip`, `tip_with_ref`, `tip_in_token` and `tip_many`. `collect_recurring` is not halted ([#220](https://github.com/Novatip/novatip-contracts/issues/220)) | Cannot move funds out of any jar or alter existing splits |
| `set_fee_bps(fee_bps)` | Sets the protocol fee taken off the top of tips (capped at 5% / `500` bps) | Cannot exceed `500` bps or take funds retroactively |
| `allow_token(token)` / `disallow_token(token)` | Controls which tokens (besides the default USDC token) can be tipped | Cannot disallow the default token or seize held balances |
| `set_tip_hook(hook)` | Registers or clears a post-tip hook contract called after tip transfers | Cannot intercept or revert funds already sent to recipients before the hook runs (hook failure reverts the whole tx) |
| `set_registrar(registrar)` | Registers or clears a co-signer required for new jar creation | Cannot modify, pause, or delete already-registered jars |
| `transfer_admin(new_admin)` | Transfers the `admin` role to `new_admin` | Cannot bypass any per-function bounds by itself |
| `migrate()` | Runs once per schema version after an upgrade to migrate stored state | Cannot run arbitrarily without a preceding `upgrade` |
| `upgrade(new_wasm_hash)` | **Replaces the contract Wasm code** while keeping the address and all stored state | **Overrides every other guarantee above** — new code is not bound by the old contract's caps or invariants |
is reported, so the order tells a client what it will actually see. An error is
written as raised only when every earlier check has passed.

| Function | Auth | Errors (in check order) | Description |
|----------|------|-------------------------|-------------|
| `__constructor(admin, token)` | — | — | Deploy-time init. Stores the admin and USDC token address. |
| `create_jar(owner, jar_id, splits)` | `owner` (+ registrar co-auth when set) | `InvalidJarId` → `JarExists` → `SplitsEmpty` → `TooManyRecipients` → `SplitOutOfRange` / `DuplicateRecipient` (per entry) → `SplitSumNot100Pct` → Soroban auth error (missing registrar co-signature, #178) | Register a new jar. Fails if the slug is not a canonical `@`-prefixed `[a-z0-9_-]{3,32}` slug, already exists, or splits are invalid. The registrar's co-signature is demanded only after validation passes, so a malformed request never burns one. Emits a `jar_crtd` event. |
| `update_splits(jar_id, splits)` | jar `owner` | `JarNotFound` → `SplitsEmpty` → `TooManyRecipients` → `SplitOutOfRange` / `DuplicateRecipient` (per entry) → `SplitSumNot100Pct` | Replace a jar's splits. Subject to the same validation as `create_jar`. Emits a `splits` event. |
| `set_min_tip_amount(jar_id, min_amount)` | jar `owner` | `JarNotFound` → `InvalidMinimumAmount` | Set or clear an optional minimum tip amount for a jar. `min_amount` must be strictly positive (`> 0`) or `None` to clear. Emits a `min_tip` event. |
| `transfer_jar_ownership(jar_id, new_owner)` | current jar `owner` and `new_owner` | `JarNotFound` → `InvalidRecipient` (new owner is the current owner, the contract itself, or the token) | Hand control of a jar to `new_owner`. The exact `(jar_id, new_owner)` pair is signed by both owners. Splits are unchanged. Emits a `jar_xfer` event carrying both owners. |
| `admin_recover_jar(jar_id, new_owner)` | contract admin | `JarNotFound` → `InvalidRecipient` (new owner is current owner, contract, or token) | Reassign a jar after owner-key loss. Preserves splits and flags; emits `jar_recov`. |
| `delete_jar(jar_id)` | current jar `owner` | `JarNotFound` | Delete a registered jar and free its slug for reuse. Removes the jar, its stats, and the associated minimum-tip value. Emits a `jar_del` event with the jar id as a topic. |
| `rename_jar(jar_id, new_jar_id)` | jar `owner` | `InvalidJarId` (new id not a canonical `@`-prefixed `[a-z0-9_-]{3,32}` slug) → `JarNotFound` → `JarIdInUse` | Move a jar, its stats and its minimum to `new_jar_id`. The old id stops resolving once the move is complete. Emits a `jar_rnm` event with the old jar id as a topic and `JarRenamedEvent { version, jar_id, new_jar_id }` data. |
| `tip(from, jar_id, amount, message)` | `from` | `ContractPaused` → `NotInitialized` → `AmountZero` (amount = 0) / `AmountNegative` (amount < 0) → `MessageTooLong` → `JarNotFound` → `JarPaused` → `BelowMinTipAmount` → `InsufficientBalance` → `SplitAmountTooSmall` (a share truncates to zero) / `AmountOverflow` (`amount * bps` overflows `i128`) | Transfer `amount` USDC from `from`, split across the jar's recipients. Emits a `tip` event carrying the per-recipient breakdown. Rejects amounts below the jar's minimum with `BelowMinTipAmount` and amounts that cannot pay every recipient with `SplitAmountTooSmall`; see [the smallest amount a jar accepts](#the-smallest-amount-a-jar-accepts-86). A failing token `transfer` reverts the whole call with the **token contract's own error**, not one of this contract's codes (e.g. an insufficient balance). |
| `get_jar(jar_id) -> Jar` | — | `JarNotFound` | Read a jar's configuration. Panics with `JarNotFound` if the slug is free. |
| `get_jar_owner(jar_id) -> Address` | — | `JarNotFound` | Read just a jar's owner. Panics with `JarNotFound` if the slug is free. |
| `get_split_count(jar_id) -> u32` | — | `JarNotFound` | How many recipients a jar pays. Panics with `JarNotFound` if the slug is free. |
| `preview_split(jar_id, amount) -> Vec<i128>` | — | `AmountZero` / `AmountNegative` → `JarNotFound` → `BelowMinTipAmount` → `SplitAmountTooSmall` / `AmountOverflow` | What each recipient would receive from a tip of `amount`, in split order. Rejects amounts below the jar's minimum with `BelowMinTipAmount` as well as the amounts `tip` rejects, in the same order; panics with `JarNotFound` if the slug is free. |
| `preview_tip(jar_id, from, amount) -> Vec<i128>` | — | `AmountZero` (amount = 0) / `AmountNegative` (amount < 0) → `JarNotFound` → `BelowMinTipAmount` → `SplitAmountTooSmall` / `AmountOverflow` | The sender-aware preview (#177): exactly the amounts `tip` will transfer for `from`, helper-shared with the payout. |
| `preview_tip(jar_id, from, amount) -> Vec<i128>` | — | `InvalidAmount` (amount ≤ 0) → `JarNotFound` → `BelowMinTipAmount` → `InvalidAmount` (a share truncates to zero, or `amount * bps` overflows) | The sender-aware preview (#177): exactly the amounts `tip` will transfer for `from`, helper-shared with the payout. |
| `tip_many(from, tips, token) -> Vec<u64>` | `from` | Soroban auth error (missing `from` signature) → `ContractPaused` → `BatchEmpty` → `BatchTooLarge` → `NotInitialized` → `TokenNotAllowed`, then per entry the same sequence as `tip`: `AmountZero` / `AmountNegative` → `MessageTooLong` → `JarNotFound` → `BelowMinTipAmount` → `AboveMaxTipAmount` → `SplitAmountTooSmall` / `AmountOverflow` → `InsufficientBalance` → the token contract's own error | Tip several jars in one transaction (#84). The batch-size and pause checks run first, before any jar is looked up; each entry is then settled exactly as `tip` would settle it (including the per-jar `JarPaused` check), and a failure in any entry reverts the whole call. |
| `collect_recurring(id)` | — | `RecurringNotFound` → `NotDue` → `JarNotFound` → `NotInitialized` → `InsufficientAllowance` → `InsufficientBalance` | Collect one due period of a recurring schedule through the supporter's token allowance (#175). Publishes the same `tip` event and fires the post-tip hook. |
| `jar_exists(jar_id) -> bool` | — | — | Whether the slug is already registered. Reads one key and returns a plain `bool`; never panics on a free slug. |
| `is_recipient(jar_id, address) -> bool` | — | `JarNotFound` | Whether `address` appears in the jar's splits. Panics with `JarNotFound` if the slug is free. |
| `get_min_tip_amount(jar_id) -> Option<i128>` | — | `JarNotFound` | Read a jar's optional minimum tip amount. Returns `None` if no minimum is set. Panics with `JarNotFound` if the slug is free. |
| `get_min_accepted_tip(jar_id) -> i128` | — | `JarNotFound` | The smallest tip this jar accepts: the larger of the truncation floor and the configured minimum. See [the smallest amount a jar accepts](#the-smallest-amount-a-jar-accepts-86). |
| `get_token() -> Address` | — | `NotInitialized` | The USDC token address tips settle in. Reads instance storage; `NotInitialized` if the deploy-time token is absent. |
| `get_admin() -> Address` | — | `NotInitialized` | The contract admin recorded at deploy time. Reads instance storage; `NotInitialized` if the deploy-time admin is absent. |
| `get_limits() -> Limits` | — | — | The bounds this contract enforces: `bps_denom`, `max_recipients`, `max_message_len`, `max_batch_tips`, `max_jar_id_len`, `min_jar_id_len`, `max_fee_bps`. Reads no storage. |
| `get_limits() -> Limits` | — | — | The bounds this contract enforces: `bps_denom`, `max_recipients`, `max_message_len`, `max_batch_tips`, `max_jar_id_len`, `min_jar_id_len`, `max_fee_bps`. Reads no storage. |

Two ordering notes that are easy to get wrong:

- **Auth is not a contract error code.** A call that lacks the required
  signature fails with Soroban's authorization error, not a `tip_splitter`
  variant. The owner-only setters (`update_splits`, `set_min_tip_amount`,
  `transfer_jar_ownership`) look the jar up *before* requiring the owner's
  signature, so an unauthenticated caller using a free slug sees `JarNotFound`,
  not an auth failure. `create_jar` requires the declared owner's signature
  first.
- **`tip` validates the message before it looks up the jar**, so an over-long
  message to an unregistered slug reports `MessageTooLong`, not `JarNotFound`,
  and a non-UTF-8 or control-byte message reports `InvalidMessage` at the same
  point. Likewise `validate_splits` checks each entry's `bps` range
  before scanning for that entry's duplicates, so
  `[{to: a, bps: 0}, {to: a, bps: 10_000}]` reports `SplitOutOfRange`.

### Confirming you are a recipient

A collaborator added to someone else's jar has no cheap way to confirm they are
actually in it. The alternative is fetching the whole jar with `get_jar` and
scanning the split vector for your own address, which is awkward from a wallet
or a one-line script.

`is_recipient(jar_id, address)` answers it directly and returns a plain
`bool`. It reads the same single persistent key `get_jar` does, so it is
cheap enough for a lightweight client.

**Membership is not ownership.** The answer tracks the split vector only: a jar
owner who takes no share of a tip gets `false`, and an address listed in the
splits gets `true` whether or not it owns the jar. Use
[`get_jar_owner`](#reading-just-the-owner) to ask who controls a jar.

**A missing jar is an error, not a `false`.** An unregistered slug panics with
`JarNotFound`, exactly as `get_jar` does. Returning `false` would make a typo'd
jar id indistinguishable from a genuine non-membership. Use `jar_exists` when
the question is whether the slug is registered at all.

Membership follows `update_splits`: a collaborator removed by a splits update
immediately reads as `false`, and one added reads as `true`. There is no
historical view — the answer describes the jar's current splits, so it is not a
record of who was paid by past tips. For that, read the per-recipient
breakdown in the [`tip` event](#tip--published-on-every-successful-tip). For
what a *future* tip would pay each of them, use
[`preview_split`](#previewing-a-tip).

### Reading the contract's limits

`BPS_DENOM`, `MAX_RECIPIENTS`, `MAX_MESSAGE_LEN` and `MAX_BATCH_TIPS` are
private constants, so a client that wants to validate input before paying for a
transaction has no way to read them. The result was that every client carried
its own copy — the tip form capped messages at its own number, the splits editor
hardcoded the twenty recipient limit — and changing a bound here silently
desynchronised the stack.
`BPS_DENOM`, `MAX_RECIPIENTS`, `MAX_MESSAGE_LEN`, `MAX_JAR_SLUG_LEN` and
`MAX_JAR_ID_LEN` are private constants, so a client that wants to validate
input before paying for a transaction has no way to read them. The result was
that every client carried its own copy — the tip form capped messages at its
own number, the splits editor hardcoded the twenty recipient limit, the
onboarding form hardcoded 64 for the jar id — and changing a bound here
silently desynchronised the stack.

`get_limits` returns all seven:

```rust
Limits {
    bps_denom: 10_000,       // BPS_DENOM — the denominator every `bps` is a share of
    max_recipients: 20,      // MAX_RECIPIENTS — most recipients in one jar
    max_message_len: 280,    // MAX_MESSAGE_LEN — longest tip message, in UTF-8 bytes
    max_batch_tips: 10,      // MAX_BATCH_TIPS — most jars one `tip_many` call may tip
    max_jar_id_len: 33,      // MAX_JAR_SLUG_LEN + 1 — longest canonical jar id ("@" + 32)
    min_jar_id_len: 4,       // MIN_JAR_SLUG_LEN + 1 — shortest canonical jar id ("@" + 3)
    max_fee_bps: 500,        // MAX_FEE_BPS — highest fee `set_fee_bps` accepts
}
```

All of them are compile-time constants, so the view reads no storage and costs
the same regardless of contract state. A client can fetch it once at startup
and cache it for the session.

`max_jar_id_len` is the **effective** input bound, not the 64-byte
`MAX_JAR_ID_LEN` storage cap. The canonical rule in
[`jar_id` validation](#jar_id-validation) already caps an id at a leading `@`
plus 32 characters, so a longer value is rejected with `InvalidJarId` long
before the storage cap matters; reporting 64 would let an onboarding form
submit a slug the contract refuses.

**Consumer-facing change:** `max_jar_id_len` is a new field in the map
`get_limits` returns (#98). A decoder typed against the previous three-field
`Limits` must add the field, or ignore map keys it does not know, before
pointing at a deployment that reports it; `version()` stays at 3 because no
existing function, error code or event changed shape.

`max_message_len` is a **byte** count, not a character count — see
[Message](#concepts) above. A client showing a remaining-characters indicator
must count UTF-8 bytes against this number, not `message.length`.

A test asserts the returned values equal the constants (`get_limits_matches_
contract_constants` in `src/test/views.rs`), so the view and the enforcement cannot drift
apart. The current values are also asserted literally, which means changing a
bound fails the suite until the constants are updated with it. The document
you are reading is checked too: `scripts/check-docs.sh` (`make check-docs`,
and a CI step) builds the contract, reads its public interface out of the
built wasm, and fails when the Functions table above or the Errors table
below no longer matches the contract — a function or error variant missing
from either table, listed there although the contract does not have it, or
documented with a different error number.

### Tipping many jars at once (84)

A tipper who supports several creators pays for it per creator: one transaction,
one wallet approval and one network fee each. For micro-tips, where the point is
speed and the amounts are small, that overhead is a large fraction of the whole
interaction.

`tip_many(from, tips)` settles a whole batch under **one** authorisation. Each
entry in `tips` is a `TipRequest` — the slug, the amount, and the message — and
each is settled exactly as a standalone [`tip`](#tip--published-on-every-successful-tip)
would settle it:

```rust
TipRequest { jar_id: String, amount: i128, message: String }
```

"Exactly" is literal. Every entry goes through the same private `settle_tip` the
single-jar entry points share, so a batched jar gets the same validation, the
same minimum-tip check, the same protocol fee, the same self-transfer skip, the
same held-funds fallback, and **its own `tip` event** carrying its own sequence
number. Each entry's amount and message are its own; the single authorisation is
the only thing the entries share.

So a batched tip is indistinguishable, on chain, from the same tips sent one at
a time — an indexer that already decodes a `tip` event needs no change
whatsoever to follow a batch. `tip_many` returns one sequence number per entry,
in the order the entries were given.

**The batch is all-or-nothing.** Soroban rolls the whole transaction back if any
entry panics, so a batch whose last jar is rejected leaves the earlier jars'
balances and totals untouched, and no sequence number is consumed by the entries
that would have settled. A client that wants partial success tips the jars that
would fail separately, or filters them out first.

The batch is bounded by `MAX_BATCH_TIPS` (see
[reading the contract's limits](#reading-the-contracts-limits)) and that bound is
a typed error, not a budget abort: each entry can fan out to `MAX_RECIPIENTS`
transfers, so an unbounded batch would eventually exhaust the transaction's
budget and fail with an opaque wasm trap. `MAX_BATCH_TIPS * MAX_RECIPIENTS`
keeps the worst case at 200 transfers, the same order of magnitude as a single
maximum-size jar. Exceeding the bound is `BatchTooLarge`; an empty batch is
`BatchEmpty`.

### Preparing and debugging a batch

Before asking the supporter to sign a batch:

1. Read `get_limits().max_batch_tips`; submit between 1 and that number of entries (currently at most 10).
2. Check `is_paused()` and, when a non-default `token` is supplied, check `is_token_allowed(token)`. One token applies to the whole batch.
3. For each entry, check `jar_exists(jar_id)`, `is_jar_paused(jar_id)`, the amount, and the message length against `get_limits().max_message_len` (280 bytes).
4. Call `preview_tip(jar_id, from, amount)` for every entry to catch per-jar minimum, split, and arithmetic failures. Each entry is settled like a standalone tip and incurs its own protocol fee.
5. Simulate the exact complete `tip_many` call with the intended sender, token, amounts, and messages before requesting authorization. Per-entry previews do not validate the combined call's authorization or total available balance.

If a batch simulation fails, the contract error does not identify which entry failed. Simulate entries individually, or divide the batch into smaller subsets, using the same sender and token; remove or correct the rejected entry, then simulate the complete batch again. Because execution is all-or-nothing, no entry succeeds until the full batch succeeds.

### Reading just the owner

`get_jar_owner` answers "who controls this jar?" without moving the recipient
list. The alternative — `get_jar(jar_id).owner` — deserializes the whole
`splits` vector to read one address, which for a jar near the 20 recipient cap
is a lot of data to render a "you own this jar" badge or to decide whether the
connected wallet may call `update_splits`.

It reads the same stored jar `get_jar` does, so the two never disagree, and it
tracks `transfer_jar_ownership` immediately. A free slug is a `JarNotFound`
panic rather than a placeholder address — a zero address in a return value
would read to a client as a jar somebody owns.

### Counting recipients

`get_split_count` returns the number of entries in a jar's `splits`. A tip page
rendering a "3 collaborators" badge needs that one integer, and
`get_jar(jar_id).splits.len()` makes it pay for the whole recipient vector —
up to 20 addresses and shares — to compute it. It also gives a client a cheap
way to decide whether fetching the full list is worth it at all.

The count is always between `1` and `20` (`MAX_RECIPIENTS`) for a stored jar,
since validation rejects an empty splits list. An unregistered slug therefore
panics with `JarNotFound` rather than returning `0`, which no real jar can
have.

### Previewing a tip

`preview_split` answers "what does each collaborator actually get?" before the
supporter signs. It returns one `i128` per recipient, in the same order as the
jar's `splits`, computed by the same code path `tip` pays out with — the two
share one internal helper, so the quoted split and the paid split cannot drift.

Clients should call it instead of recomputing the split themselves. Doing the
arithmetic client-side puts the rounding rule in two places, and the copy
inevitably falls behind.

```
splits:  alice 70%, bob 30%
preview_split(jar_id, 101) -> [70, 31]
```

Alice's share truncates from 70.7 to 70 and Bob absorbs the leftover 1 on top
of his 30 — the [dust](#glossary-dust) goes to the **last** recipient, so a UI that rounds evenly
would show Bob the wrong number.

The shares always sum to exactly `amount` minus the
[protocol fee](#the-protocol-fee-79) — `amount` itself while no fee is set. That holds for awkward amounts too:
a 3333 / 3333 / 3334 jar previewing `100` returns `[33, 33, 34]`, not `[33, 33,
33]` with a unit lost.

`preview_split` rejects exactly what `tip` rejects, so a preview that returns
at all describes a tip that can go through:

- `amount == 0` — `AmountZero`, checked before the jar lookup and the minimum.
- `amount < 0` — `AmountNegative`, at the same point.
- an `amount` small enough that some recipient's share would truncate to zero
  (`amount * bps < 10_000`) — `SplitAmountTooSmall`. See
  [the smallest amount a jar accepts](#the-smallest-amount-a-jar-accepts-86)
  for the floor this implies.
- an `amount` so large that `amount * bps` overflows `i128` — `AmountOverflow`.
- an unregistered slug — `JarNotFound`, rather than an empty vector, which
  would read as a jar that pays nobody.
- an `amount` below the jar's configured minimum — `BelowMinTipAmount`.

The sign checks come first, exactly as in `tip`, so the same amount gets the
same code from both calls even on a jar with a minimum (#97).

One caveat: `tip` withholds a recipient's share when that recipient is also the
tipper — rather than making them pay themselves — and the share simply stays
with them. `preview_split` does not take a `from` address, so
it reports every recipient's full share and disagrees with the actual payout
for a tipper who is on the jar. Do not redo that arithmetic client-side: call
[`preview_tip`](#previewing-a-tip-for-a-known-sender) instead, which takes the
sender and matches the payout exactly.

### Previewing a tip for a known sender

`preview_tip(jar_id, from, amount)` answers the same question as
`preview_split` but *for a specific tipper* (#177). It returns what `tip` will
actually transfer to each recipient, in split order: a recipient who is the
sender is reported as `0` — the same number the `tip` event's breakdown records
for them — and their share stays with them; every other recipient gets exactly
their `preview_split` entry (#78).

```
splits:  alice 50%, bob 30%, carol 20%
preview_split(jar_id, 1000)          -> [500, 300, 200]
preview_tip(jar_id, alice, 1000)     -> [0, 300, 200]   # alice keeps her 500
preview_tip(jar_id, stranger, 1000)  -> [500, 300, 200]
```

The numbers come from the **same private helper** (`payout_amounts`) that
`tip`'s transfer loop consumes, so the view and the payout are one
implementation and cannot drift apart. Earlier builds rolled the tipper's
withheld share into the last recipient, so a tipper on the jar paid their own
share to someone else; #78 fixed that, and both the view and the payout
changed together. Validation mirrors
`tip` (same errors, same precedence); like `preview_split` it reads no
balance, so it never says whether `from` can pay, and a share that ends up
escrowed as `held` at transfer time (#171) is still reported, because that
amount leaves the tipper either way.

### Setting a minimum tip amount

A creator can opt out of micro-dust tips by setting an optional floor on their jar.
By default, jars have no minimum tip amount (`None`), accepting any positive amount.

`set_min_tip_amount(jar_id, min_amount)` lets the jar owner configure an optional
minimum tip amount (`Some(amount)` where `amount > 0`) or clear it (`None`). Only the
jar owner may authorize this call. Passing a non-positive amount (`<= 0`) panics with
`InvalidMinimumAmount` (error code 29).

When a minimum is configured, any tip with `amount < min_amount` is rejected with
`BelowMinTipAmount` (error code 13) before any tokens move or events emit. `preview_split`
similarly rejects amounts below the minimum with `BelowMinTipAmount`.

`get_min_tip_amount(jar_id)` reads the configured floor:
- Returns `None` if no minimum is set (the default for all newly created jars).
- Returns `Some(amount)` if a minimum floor has been configured.
- Panics with `JarNotFound` (error code 3) if the slug is not registered.

### The smallest amount a jar accepts (86)

The configured minimum is only one of two floors. Independently of it, `tip`
rejects any amount that cannot give **every** recipient a non-zero share. A
jar's effective minimum is the larger of the two, and
`get_min_accepted_tip(jar_id)` returns it.

**1. The truncation floor.** A recipient on `bps` receives
`amount * bps / 10_000` with integer division, so their share is zero unless
`net * bps >= 10_000`, where [`net`](#glossary-net) is what is left after the
[protocol fee](#the-protocol-fee-79). The smallest net that pays everyone is

```
net_min = max over every recipient of ceil(10_000 / bps)
```

Every entry is checked, including the last one — even though the last entry
absorbs the rounding remainder, the guard runs before any share is computed.

**2. The fee gross-up.** The fee comes off the top, so the signed amount must
be large enough that `net_min` survives it. Since `net = amount -
floor(amount * fee_bps / 10_000)`, the first [gross](#glossary-gross) amount that leaves a net of
at least `net_min` is

```
gross_min = floor((net_min - 1) * 10_000 / (10_000 - fee_bps)) + 1
```

With no fee (`fee_bps = 0`) this is simply `net_min`.

**3. The configured minimum.** The owner's optional floor is applied last:

```
accepted_min = max(gross_min, configured_minimum)
```

(no configured minimum counts as `0`).

`get_min_accepted_tip` runs exactly this calculation, so a client can show the
floor — and disable a submit button below it — without reimplementing the
arithmetic. It is a view: it reads no balance, so it says nothing about whether
the tipper can actually pay. The configured component is a default-token
amount; `tip_in_token` only enforces it when the tip is paid in the default
token, while the truncation floor is pure basis-point arithmetic and applies to
any allowed token.

**Worked example — a jar with a 1 bps share.** Splits of `1` bps for a
micro-collaborator and `9 999` bps for everyone else. `ceil(10_000 / 1)` is
`10 000`, so with no fee:

| Tip | Result |
|-----|--------|
| 9 999 | rejected with `SplitAmountTooSmall` (27); nothing moves |
| 10 000 | accepted: `1` to the 1 bps recipient, `9 999` to the other |

**Worked example — a fee raises the floor.** Keep the 1 bps share but set a 5%
protocol fee (`fee_bps = 500`). A gross of `104` leaves a net of
`104 - floor(104 * 500 / 10_000) = 99`, which truncates the 1 bps share to
zero; `105` leaves `100`, the first net that pays it. `get_min_accepted_tip`
reports `105`, a tip of `104` fails with `SplitAmountTooSmall`, and `105`
succeeds.

**Which error a too-small amount returns.** The floors are enforced by
different checks, so the code tells the client which one it missed:

- below the **configured** minimum — `BelowMinTipAmount` (13);
- below the **truncation** floor — `SplitAmountTooSmall` (27), even when the
  configured minimum is unset or smaller.

An amount below `get_min_accepted_tip` is therefore always one of those two.
`InvalidAmount` (5) is **not** returned; the variant is retired and kept only
so existing codes keep their values. A client that handles `InvalidAmount` as
the minimum-tip error will never see it, and should handle 13 and 27 instead.

### Checking slug availability

`jar_exists` is the intended way to test whether a slug is taken. The
alternative — calling `get_jar` and catching the `JarNotFound` panic — is
awkward from the SDK, since a missing jar is an ordinary answer here rather
than an error. `jar_exists` reads one persistent key and returns a plain
`bool`, so the onboarding form can call it on every (debounced) keystroke.

Matching is exact: `jar_exists("@ali")` is `false` while `"@alice"` is taken.
A jar rejected by validation is never stored, so its slug stays free.

Note that availability is not a reservation. Between the check and the
`create_jar` call, another transaction can claim the slug — `create_jar` still
panics with `JarExists`, and clients must handle that rather than treating a
`false` from `jar_exists` as a guarantee.

### `jar_id` validation

A `jar_id` is a canonical slug: a leading `@` followed by **3–32** characters
from `[a-z0-9_-]`. `create_jar` and `rename_jar` reject anything else with
`InvalidJarId` — upper case, whitespace, control bytes, invalid UTF-8, a
missing `@`, or a body outside the length range. The rule matches
`novatip-backend`'s `SLUG_REGEX`, so every jar a tip page can create is
accepted, and no lookalike (`@Alice`, `@alice `) can be registered next to a
real creator's slug.

The id is also used as a storage key, an event topic, and a public URL slug.
The storage bound is `MAX_JAR_ID_LEN` (64 bytes); the canonical rule is
stricter, so any id that passes it is well inside that bound. `jar_exists`
returns `false` for ids `create_jar` would reject rather than panicking on
them.

### Validation rules

`create_jar` and `update_splits` share one validator, so both enforce all of:

- The list must be non-empty — `SplitsEmpty`.
- At most 20 entries (`MAX_RECIPIENTS`) — `TooManyRecipients`.
- **No entry may have `bps == 0`** — `SplitOutOfRange`.
- **No address may appear twice** — `DuplicateRecipient`.
- **No entry may have `bps > 10_000`** — `SplitOutOfRange`.
- The `bps` values must sum to exactly `10_000` — `SplitSumNot100Pct`.

A `bps == 0` entry is rejected rather than accepted-and-ignored. Such a
recipient could never be paid (`tip` skips zero shares), but it would still
consume one of the 20 recipient slots and surface in clients as a collaborator
who never receives funds. Rejecting it at write time keeps a stored jar's
recipient list an accurate record of who actually gets paid.

Note that this is a validation-time rule about `bps`, not a guarantee about
transferred amounts: a recipient with a valid non-zero `bps` can still receive
`0` on a small tip, because `amount * bps / 10_000` truncates (e.g. `bps: 100`
on a tip of `50` yields `0`).

Duplicates are likewise rejected rather than merged. A repeated address is not a
loss-of-funds bug — the shares still total 100% — but it makes `tip` issue
several separate transfers to one destination in a single call, wasting fees,
and leaves an on-chain record that per-collaborator accounting has to
de-duplicate after the fact. Clients that want to let a user enter the same
collaborator twice should sum the shares before submitting.

The check is a pairwise comparison over the vector, so position doesn't matter:
`[a, b, a]` is rejected just as `[a, a, b]` is.
A `bps > 10_000` entry claims more than the whole tip, so it could never belong
to a set summing to 100% — the sum check would reject it anyway. It is rejected
per entry because that also bounds the running total: with at most 20 entries
of at most `10_000` each, the accumulator can never exceed `200_000`, far below
`u32::MAX`. The sum is additionally accumulated with `checked_add`, which fails
with `SplitSumNot100Pct` rather than trapping.

This matters because `bps` is caller-supplied and unbounded in the wire type.
Before, a set of shares whose true sum exceeded `u32::MAX` relied on
`overflow-checks = true` in the release profile to trap — which reverted the
transaction, so the 100% invariant did hold, but clients saw an opaque wasm
error instead of error code 4, and the guarantee lived in `Cargo.toml` rather
than in the validator. Both the per-entry bound and `checked_add` now put it in
the code, so flipping that profile setting cannot turn it into a bypass.

### Splitting rules

- Each non-final recipient receives `amount * bps / 10_000` (integer division).
- The **last** recipient receives `amount - (sum of prior shares)`, so rounding
  dust is never lost and the full amount is always distributed.
- A tip is **not** all-or-nothing. Each recipient is delivered with
  `try_transfer`; a recipient who cannot receive the token (a dropped trustline
  or a deauthorised account) has their share moved from the tipper into the
  contract and credited under `DataKey::Held(recipient)`. Everyone else is paid
  normally and the tip still succeeds. `withdraw_held` releases the [escrow](#glossary-escrow) once
  the recipient can receive again.
- `preview_split(jar_id, amount)` runs this same calculation as a view, so a
  client can show the exact per-recipient amounts before the supporter signs.
- The amounts actually transferred are published in the `tip` event's
  `breakdown`, so no consumer has to reproduce this arithmetic after the fact
  either.

**Worked example: an amount that does not divide evenly.** A jar splits a tip
three ways, in this order, with no protocol fee set (with one, the same
arithmetic runs on the amount left after the fee):

```
splits:  alice 3_333,  bob 3_333,  carol 3_334      (bps; sums to 10_000)
tip:     1_001
```

| Recipient | Rule | Calculation | Share |
|-----------|------|-------------|------:|
| alice | `amount * bps / 10_000` | `1_001 * 3_333 / 10_000 = 3_336_333 / 10_000 = 333.63…`, truncated | 333 |
| bob | `amount * bps / 10_000` | same as alice | 333 |
| carol (last) | `amount - (sum of prior shares)` | `1_001 - (333 + 333) = 1_001 - 666` | 335 |

The shares always sum to the full amount: `333 + 333 + 335 = 1_001`. Nothing is
lost to rounding and nothing is created. Carol's nominal 33.34% of `1_001` is
`333.73…`, yet she receives `335`: the last entry takes the whole remainder, so
it can end up slightly above its percentage (by less than one unit for each
earlier recipient, so at most `n - 1` units for `n` recipients). That is the
rule working as designed, not a bug.

**Order matters.** The same three entries listed as `carol 3_334, alice 3_333,
bob 3_333` give carol `1_001 * 3_334 / 10_000 = 333`, alice `333`, and bob, now
last, `1_001 - 666 = 335`. The total is unchanged, but the extra unit has moved
from carol to bob. `create_jar` and `update_splits` keep the vector in the
order given, so put the recipient who should absorb rounding dust last.
`preview_split` returns `[333, 333, 335]` for the first order, in split order.

> **Trust implication (escrow).** Holding an undeliverable share means the
> contract temporarily custodies tokens it did not before — the previous
> all-or-nothing design let the contract hold no balance. The contract account
> must therefore be able to hold the jar token (a trustline / authorized account
> on the asset), and a held share is only ever claimable by the recipient it is
> credited to. The tipper is debited the full amount either way.

### When the tipper is also a recipient (85)

If the address sending a tip appears in the jar's splits, `tip` does not move
their share. The split is computed as for any other recipient, and then the
tipper's entry is skipped: no transfer to themselves, and none to the contract.
Every other recipient is paid exactly as usual. The one zero-share case a plan
can hold is therefore the tipper — a share that would truncate to zero is
rejected before any transfer with `SplitAmountTooSmall`
([the minimum tip](#the-minimum-tip-and-the-decision-recorded-here)), so a `0` in
the plan can only mean "this recipient is the sender".

**What the tipper actually pays.** The tipper is debited the other recipients'
shares plus the protocol fee, not the `amount` they passed in. In a jar that
pays the tipper a share `s`, the amount leaving their balance is `amount - s`.
This is the only configuration in which the amount that leaves the tipper
differs from `amount`, and it is why the `tip` event reports the tipper's
`breakdown` entry as `0`: the event is a record of the transfers the call made,
and the call made no transfer for them.

**Worked example.** A jar split 50/50 between the supporter and `alice`, with the
supporter listed first:

```rust
splits = [{ to: supporter, bps: 5_000 }, { to: alice, bps: 5_000 }]
```

The supporter tips `100`, with no protocol fee configured. The split runs over
the whole amount — the supporter would receive `100 * 5_000 / 10_000 = 50`, and
`alice`, the final entry, receives the remainder `50` — and then the supporter's
entry is skipped:

| Account | Before | After | Change |
|---------|-------:|------:|--------|
| supporter (the tipper) | 100 | 50 | −50 — only `alice`'s share leaves them; their own 50 stays |
| alice | 0 | 50 | +50 |
| admin | 0 | 0 | — the fee is `0` |

The `tip` event's `breakdown` is `[(supporter, 0), (alice, 50)]`, and the
identity holds: `fee (0) + moved (50) + the tipper's own share (50) = amount
(100)`. The supporter's balance confirms it — they were debited `50`, not `100`.

**With a protocol fee** the rule applies to what is left after the fee comes off
the top. At 100 bps (1%), a tip of `100` moves `1` to the admin and splits the
remaining `99`: the supporter's `49` stays with them, and `alice` receives `50`.
The supporter is debited `51` (`amount - 49`), and
`fee (1) + moved (50) + own share (49) = 100`.

**Being the tipper decides the skip, not the position.** Where the entry sits in
the list does not change the rule, and the other recipients are paid by the same
arithmetic either way. Position does decide which entry absorbs the rounding
dust, because the split gives the remainder to the **last** entry: if the tipper
is last, the dust-inclusive remainder is the share they keep and the others
receive their truncated shares; if the tipper is elsewhere, the last *other*
recipient absorbs that dust instead. The difference is bounded by the dust — at
most `n - 1` stroops — and is a property of the split arithmetic, not of the
self-tip rule.

**A tipper who is the jar's only recipient moves nothing**, and the call is
rejected with `NoFundsMoved` (30) rather than publishing a `tip` event that
claims a tip happened at no cost.

To see these amounts before signing, use
[`preview_tip(jar_id, from, amount)`](#previewing-a-tip-for-a-known-sender): it
reports the tipper as `0` and every other recipient's actual payout, computed by
the same helper `tip` pays with.

## Authorization

Every function that names an address in its signature requires that address's
signature, and nothing else does. What a signature actually *commits to* is the
part clients get asked about, because it decides when a previously signed
transaction stops being valid.

### `tip`

A tipper authorizes the `tip` call itself, and the same authorization tree
includes each token `transfer(from, recipient, share)` that `tip` performs. The
transfers are nested under the contract invocation, carrying the exact recipient
address and the exact amount.

Consequences a client has to design for:

- **The wallet shows one transfer per recipient, not one payment to the
  contract.** A jar splitting 70/30 between two collaborators prompts the
  supporter to approve two token transfers. Support teams get asked why the
  tipper is never asked to send anything to the jar's contract address: the
  contract never holds the funds, it moves them from the tipper straight to
  each recipient.
- **A tip signed against one set of splits cannot pay a different set.** If the
  jar's splits (or its minimum tip amount) change between simulation and
  submission, the signed transaction no longer matches what the contract will
  actually do, and Stellar rejects it with `Error(Context, InvalidAction)`
  before any funds move. This is the answer to the obvious question about a jar
  owner "redirecting" a tip that is already in flight: they can call
  `update_splits`, and the tip in transit simply fails. The old signature cannot
  pay the new recipients.
- **The client's recovery is to re-simulate and ask for a fresh signature.**
  Without this documented the rejection looks random, because nothing in the
  tip request changed — the jar did. On `InvalidAction`, re-simulate
  (`preview_split` gives the same per-recipient amounts the new tree will
  commit to), re-sign, and submit; a wallet that only retries the same
  envelope will fail identically forever.
- A tipper who is also a recipient is not paid, so no transfer appears for them
  in the tree, and their share stays with them. With a
  [protocol fee](#the-protocol-fee-79) set, the tree also holds one transfer of
  the fee to the admin. See
  [when the tipper is also a recipient](#when-the-tipper-is-also-a-recipient-85)
  for what the tipper is actually charged, and
  [`breakdown`](#tip--published-on-every-successful-tip).

### Owner-only functions

| Function | Authorizes | Tree contains |
|----------|-----------|---------------|
| `create_jar(owner, jar_id, splits)` | `owner` | The contract call only. No token moves. |
| `update_splits(jar_id, splits)` | the jar's current `owner` | The contract call only. |
| `transfer_jar_ownership(jar_id, new_owner)` | the jar's current `owner` and `new_owner` | The exact `(jar_id, new_owner)` transfer pair, signed by both parties. |
| `admin_recover_jar(jar_id, new_owner)` | the contract admin | The admin authorizes reassignment; neither old nor new owner signs. Splits and flags are preserved. |
| `delete_jar(jar_id)` | the jar's current `owner` | The contract call only. |
| `set_min_tip_amount(jar_id, min_amount)` | the jar's current `owner` | The contract call only. |
| `set_jar_paused(jar_id, paused)` | the jar's current `owner` | The contract call only. No token moves. |

Each of these is a single contract-level invocation with no nested token
transfers, so the signer approves one action, and the amount is not part of what
is signed. Reading the jar (`get_jar`, `get_split_count`, `preview_split`, …)
requires no authorization at all.

The same staleness rule applies to a different degree: these calls do not commit
to a jar's future state, so an `update_splits` signed against the current owner
still succeeds after that owner changes — it just targets whichever jar the
slug names at submission time.

> A test that pins the `tip` authorization behaviour is tracked separately from
> this document; link it here once it lands.

### Admin-only functions

A single address — recorded at deploy time and readable through `get_admin()` —
authorizes the contract's configuration and explicitly listed recovery entry points. All of them go through the
same `require_admin` helper, which loads the stored address and demands its
signature, so the table below is exhaustive: an entry point that is not in it
does not check the admin.

| Function | What only the admin can change |
|----------|--------------------------------|
| `allow_token(token)` | Add an asset to the tip allowlist. Idempotent. |
| `disallow_token(token)` | Remove one, except the deploy-time token (`CannotDisallowDefault`). |
| `set_tip_hook(hook)` | Point the post-tip callback at a contract, or clear it. |
| `set_registrar(registrar)` | Require a registrar co-signature for new jars, or clear it. |
| `set_fee_bps(fee_bps)` | Set the protocol fee in `0..=500` bps; the fee is paid to the fee recipient (the admin by default). |
| `set_fee_recipient(recipient)` | Send fees to another address, or `None` for the admin. |
| `transfer_admin(new_admin)` | Hand the role to another address. It takes effect immediately; there is no acceptance step. |
| `pause()` / `resume()` | Stop and restart `tip`, `tip_with_ref` and `tip_in_token`. |
| `upgrade(new_wasm_hash)` | Replace the running Wasm, keeping the address and all stored state. |
| `migrate()` | Run the stored-state migration for the installed build, once per schema version. |
| `admin_recover_jar(jar_id, new_owner)` | Reassign ownership of an existing jar after its owner key is lost, preserving its splits, lock, and pause state. |

Recovery is a powerful exception to owner-controlled custody. `admin_recover_jar`
requires only the admin's authorization; neither the previous owner nor the new
owner needs to sign, and there is no delay or co-signature. The admin can
reassign any existing jar, including an active one. This does not move funds or
change the recipients, but the new owner controls the jar after reassignment.
The `jar_recov` event provides an audit record; it does not prevent abuse.
Operators must therefore protect and govern the admin key as a root authority.

The admin **cannot**:

- move a jar's funds. Nothing can: tips go straight from the tipper to the
  recipients in the same invocation. There is no sweep, no withdraw-all, and
  `withdraw_held` pays only the address it names.
- edit a jar's splits, minimum, lock or pause state without its owner. The
  explicit `admin_recover_jar` exception changes only ownership; plus each
  affected recipient is required for a reduction on a locked jar.
- repoint the settlement token. `DataKey::Token` is written exactly once, in
  `__constructor`, and there is no setter, so `get_token()` is immutable after
  deploy. The admin may add other assets to the allowlist, but `tip` always
  pays through the default one and `disallow_token` refuses to remove it.
- charge more than 500 bps (5%). `set_fee_bps` rejects larger values with
  `FeeTooHigh` and leaves the stored fee unchanged.
- change the compiled-in bounds: 20 recipients, a 280-byte message, 10 000 bps
  for the whole split. They ship with the code, so changing one means shipping
  new code.
- stop scheduled collection. `pause` gates the four tip entry points only;
  `collect_recurring` and `withdraw_held` keep working while paused.

`upgrade` is the widest power on the list and it is deliberately in the admin's
hands: new Wasm can change everything the bullets above pin down. Operators
should treat the admin key as the contract's root authority for that reason,
and `transfer_admin` as the rotation mechanism. The stored token address
survives an upgrade because storage is kept.

**The token address is immutable after deploy.** It is fixed by the
`__constructor` argument, exposed read-only through `get_token()`, and used by
`tip`, `collect_recurring` and the fee transfer. Rotating the settlement asset
means deploying a new contract, not reconfiguring this one; the allowlist
exists so supporters can pay in *additional* assets without that migration.

**Maintenance note.** This table is the complete list of admin-gated entry
points. When a function starts or stops calling `require_admin`, update this
section, the `Auth` column of the [function tables](#functions), and the
`DataKey::Admin` comment in `contracts/tip-splitter/src/lib.rs` in the same
change — otherwise the "what can the admin do?" answer drifts from the code
exactly the way issue #87 found it had.

## Errors

| Code | Name | Cause |
|------|------|-------|
| 1 | `NotInitialized` | A deploy-time value is missing from instance storage: the USDC token address (raised by `get_token` and by `tip`) or the admin (raised by `get_admin`). Should never happen post-deploy. |
| 2 | `JarExists` | Slug already registered. |
| 3 | `JarNotFound` | Slug not registered. |
| 4 | `InvalidSplits` | **No longer raised.** Split validation now reports the specific failure as code 10, 11 or 12. The variant is retained so existing codes keep their values. |
| 5 | `InvalidAmount` | **No longer raised.** Amount validation reports the specific failure: `AmountZero` (25), `AmountNegative` (26), `SplitAmountTooSmall` (27), `AmountOverflow` (28), or `InvalidMinimumAmount` (29). The variant is retained so existing codes keep their values. |
| 6 | `TooManyRecipients` | More than 20 recipients. |
| 7 | `DuplicateRecipient` | The same address appears more than once in the splits. |
| 8 | `MessageTooLong` | Tip message exceeds 280 bytes. |
| 9 | `InvalidJarId` | `jar_id` is not a canonical slug: not `@` followed by 3–32 characters from `[a-z0-9_-]` (or over the 64-byte `MAX_JAR_ID_LEN` storage bound). |
| 10 | `SplitsEmpty` | The splits list is empty. |
| 11 | `SplitOutOfRange` | An entry has `bps == 0` or `bps > 10_000`. Checked per entry, before the sum. |
| 12 | `SplitSumNot100Pct` | The shares sum to something other than exactly 10 000 bps, including a sum that would overflow `u32`. |
| 13 | `BelowMinTipAmount` | Tip amount is strictly below the jar's configured minimum tip amount. Raised by `tip`, by `preview_split`, by `preview_tip`, and by `tip_in_token` when it is paid in the default token. |
| 14 | `NotRecipient` | `leave_jar` was called for an address not present in the jar's splits. |
| 15 | `SoleRecipient` | `leave_jar` would empty the jar — the caller is its only recipient. |
| 16 | `RecurringNotFound` | `cancel_recurring` / `get_recurring` / `collect_recurring` named an id that does not exist. |
| 17 | `InvalidPeriod` | A recurring tip was registered with a zero `period_ledgers` (or its due-ledger computation would overflow). |
| 18 | `NothingHeld` | `withdraw_held` was called for a recipient the contract holds nothing for. |
| 19 | `TokenNotAllowed` | `tip_in_token` was given a token the admin has not allowed. Raised before any transfer, so a rejected tip moves nothing. |
| 20 | `CannotDisallowDefault` | `disallow_token` was given the deploy-time token, which `tip` always pays through and which therefore stays non-removable. |
| 21 | `StatsOverflow` | A running tip total would not fit `u64`/`i128`. Effectively unreachable; the counters use checked arithmetic so an exhaustion reports as this code rather than a wasm trap. |
| 22 | `InsufficientBalance` | Tip amount exceeds the sender's token balance. Checked once per `tip` and once per `collect_recurring`, before any transfer. |
| 23 | `NotDue` | `collect_recurring` was called before the schedule's `next_due_ledger`. A typed no-op: the entry is untouched and the same call succeeds once the ledger is reached. |
| 24 | `InsufficientAllowance` | `collect_recurring` found the supporter's allowance for this contract below the scheduled amount. An expired allowance reads as zero and fails the same check. Raised before any transfer, so nothing moves. |
| 25 | `AmountZero` | The tip (or preview, or recurring) amount is zero. |
| 26 | `AmountNegative` | The tip (or preview, or recurring) amount is negative. |
| 27 | `SplitAmountTooSmall` | The amount — after the protocol fee, when one is set — is small enough that some recipient's share would truncate to zero (`net * bps < 10_000`). `get_min_accepted_tip` reports the smallest amount that clears it. |
| 28 | `AmountOverflow` | `amount * bps` (or `amount * fee_bps`) would overflow `i128`. |
| 29 | `InvalidMinimumAmount` | `set_min_tip_amount` was given a minimum that is zero or negative. |
| 30 | `NoFundsMoved` | A tip where every share was skipped (every recipient is the tipper) would move no funds. |
| 31 | `InvalidRecipient` | A split recipient or jar owner is the contract itself or its token. |
| 32 | `AdminExists` | `transfer_admin` was called with the current admin's address. |
| 33 | `ContractPaused` | A tip was attempted while the contract is paused. |
| 34 | `JarIdInUse` | `rename_jar` was given a jar id that is already in use. |
| 35 | `FeeTooHigh` | `set_fee_bps` was given a fee above `500` bps (5%). The stored fee is unchanged. |
| 36 | `InvalidMessage` | The tip message is not valid UTF-8, or contains a control character other than newline. Raised before any transfer, and by `create_recurring` for the message it stores. |
| 37 | `BatchEmpty` | `tip_many` was given a batch with no entries. Raised before any jar is looked up, so an empty batch never moves funds. |
| 38 | `BatchTooLarge` | `tip_many` was given more entries than `MAX_BATCH_TIPS` (10). Raised before any jar is looked up. |
| 39 | `JarPaused` | A tip was attempted to a jar its owner has paused with `set_jar_paused` (#45). Raised after `JarNotFound` and before any transfer, so a rejected tip moves no funds; reads stay available while the jar is paused. |
| 40 | `TooManyAllowedTokens` | `allow_token` was given a new token while the allowlist already holds `MAX_ALLOWED_TOKENS` (10) tokens (#321). |
| 41 | `AboveMaxTipAmount` | A tip in the default token, or a preview, exceeded the jar's maximum set with `set_max_tip_amount` (#323). Raised before any transfer. |
| 42 | `InvalidMaximumAmount` | `set_max_tip_amount` was given a maximum that is zero, negative or below the jar's minimum, or `set_min_tip_amount` a minimum above the jar's maximum (#323). |
| 44 | `NotAuthorized` | `cancel_recurring` was called by an address that is neither the schedule's supporter nor the owner of its jar (#324). |

Error codes are part of the public interface: `@novatip/sdk` and the frontend
both map these numbers to user-facing messages. New variants are **appended**
and existing values are not renumbered, which is why code 4 stays in place
rather than being removed.

> **One renumbering did happen, and it had to.** By the time the allowlist,
> `leave_jar`, and the recurring-tip PRs were merged, `main` carried a three-way
> collision: codes 14, 15, and 16 were each claimed by two variants
> (`NotRecipient`/`TokenNotAllowed`, `SoleRecipient`/`CannotDisallowDefault`,
> `RecurringNotFound`/`StatsOverflow`), and `InsufficientBalance` also sat on 14.
> Duplicate codes are not decidable, so the later-added side of every collision
> moved to a fresh number: `TokenNotAllowed` 14 → 19, `CannotDisallowDefault`
> 15 → 20, `StatsOverflow` 16 → 21, `InsufficientBalance` 14 → 22. Codes 1–18
> as consumed by the *working* deployments (the original ladder) are unchanged;
> anything wired to the colliding 14/15/16 of a `main` build could not have
> worked deterministically anyway.

### Token failures keep the token's own code

`tip` moves funds with the USDC Stellar Asset Contract's `transfer`. When that
call fails for a reason other than an empty wallet, the token's error
propagates unchanged — and the SAC numbers its errors from 1 just like this
contract does, so the numbers overlap:

| SAC code | SAC meaning | Collides with this contract's |
|----------|-------------|-------------------------------|
| 10 (`BalanceError`) | balance too low to spend (also: resulting balance too low/high, clawback on a non-clawback trustline) | 10 `SplitsEmpty` |
| 11 (`BalanceDeauthorizedError`) | balance authorization revoked (frozen) by the issuer | 11 `SplitOutOfRange` |
| 12 (`OverflowError`) | spender allowance would overflow | 12 `SplitSumNot100Pct` |
| 13 (`TrustlineMissingError`) | trustline entry missing for the account | 13 `BelowMinTipAmount` |

(The SAC also uses 3, 6, 8 and 9, which likewise coincide with this
contract's codes of the same numbers.)

The one exception is the empty wallet: `tip` (and `collect_recurring`) read
the sender's token `balance` once, before the first transfer, and fail with
the contract's own code 22 `InsufficientBalance` when `balance < amount`. A tip larger than the
sender's balance therefore never reaches the token, so it can never be
mislabelled as "Add at least one collaborator".

Any other token failure — a missing trustline on either end, a frozen
(deauthorized) trustline, an unauthorized or overflowing allowance — still
surfaces with the token's number. **Clients must check which contract raised
the error before mapping the number to a message.** On failure the diagnostic
events name the failing contract (the SAC's contract id vs this contract's
id); only errors raised by `tip_splitter` itself may be looked up in the table
above. A bare `Error(Contract, #10)` with no source is ambiguous between
`SplitsEmpty` and the SAC's `BalanceError`.

## Events

### `jar_crtd` — published on every successful `create_jar`

- **Topics:** `(symbol "jar_crtd", jar_id: String)`
- **Data:** `JarCreatedEvent { owner: Address }`

Indexers must subscribe to this event to build and maintain the full list of
registered jars. There is no on-chain `get_jar_ids` function — event scanning
is the canonical discovery mechanism. This keeps `create_jar` cost constant
(O(1) storage writes) regardless of how many jars have been created.

Enumeration and existence are separate concerns: `jar_exists` answers "is this
one slug taken?" straight from storage, so a client never has to scan the event
log or an indexer's jar list just to validate a name.

### `splits` — published on every successful `update_splits`

- **Topics:** `(symbol "splits", jar_id: String)`
- **Data:** `SplitsUpdatedEvent { count: u32, splits: Vec<Split> }`

Lets an indexer that has cached a jar's splits know they went stale, without
having to re-poll every jar on a schedule. The event carries the updated
split vector itself, so a consumer can refresh immediately and avoid the stale
read window created by a second update racing with a follow-up `get_jar` call.

### `jar_xfer` — published on every successful `transfer_jar_ownership`

- **Topics:** `(symbol "jar_xfer", jar_id: String)`
- **Data:** `JarTransferredEvent { previous_owner: Address, new_owner: Address }`

### `jar_recov` — published on every successful `admin_recover_jar`

- **Topics:** `(symbol "jar_recov", jar_id: String)`
- **Data:** `JarAdminRecoveredEvent { version: u32, admin: Address, previous_owner: Address, new_owner: Address }`

This event is distinct from the mutually authorized `jar_xfer` handoff. It
records the admin-authorized recovery and both owners for monitoring and audit.

Lets an indexer update who controls a jar without re-polling `get_jar` for
every jar on a schedule.

Both ends of the move are published, in that order, so each event is
independently meaningful. With only the new owner, a consumer replaying the log
could see where a jar went but not where it came from, unless it had already
indexed every prior event for that jar and kept the running state — which an
indexer starting from a partial history has not. Carrying `prev_owner` also
means successive transfers chain: each event's `prev_owner` is the previous
event's `new_owner`, so an ownership history can be reconstructed from the
events alone.

`prev_owner` is the address that previously controlled the jar, and the
contract now requires both sides of the handoff to authorize the exact
`(jar_id, new_owner)` pair. A transfer to the current owner is rejected with
`InvalidRecipient`, so the event log never records a no-op handoff. Splits are
unchanged by a transfer, so no `splits` event accompanies this one.

> **Consumer impact.** Event data is a named map. Decoders should read fields
> by name and ignore unknown fields added in future versions.

### `jar_del` — published on every successful `delete_jar`

- **Topics:** `(symbol "jar_del", jar_id: String)`
- **Data:** `JarDeletedEvent { version: u32, jar_id: String }`

Lets an indexer drop a jar from its cached list as soon as the slug is retired,
without polling `jar_exists` or re-reading the jar. A deleted slug is free for
`create_jar` to reuse immediately after the write settles.

The jar id is a topic, like every other jar event, so a per-jar subscription
receives the deletion instead of having to decode every `jar_del` payload. It
remains in the data as well so the event is self-describing.

### `min_tip` — published on every successful `set_min_tip_amount`

- **Topics:** `(symbol "min_tip", jar_id: String)`
- **Data:** `MinTipEvent { amount: Option<i128> }`

Lets indexers track the current minimum tip floor for a jar without polling `get_min_tip_amount`.

### `tip` — published on every successful tip

- **Topics:** `(symbol "tip", jar_id: String)`
- **Data:** `TipEvent { from, amount, fee, message, breakdown, token, seq, reference }`

  Published by `tip`, `tip_with_ref`, `tip_in_token`, and — with the supporter
  as `from` and an explicit `None` reference — `collect_recurring` (#175), all
  through one internal settle path, so one decoder serves every on-chain tip.

The backend indexer subscribes to this event to update balances, leaderboards,
and notifications. `message` is valid UTF-8 with no control characters other
than newline and at most 280 bytes, so the payload size is bounded and a
`varchar(280)` column is enough to store it.

`breakdown` lists one `(recipient, moved, held)` triple per split, **in split
order**, so the event is self-describing: a consumer that wants
per-collaborator earnings never has to fetch the jar and redo the arithmetic.
That matters because a recomputed figure can disagree with the contract's —
the jar's splits may have been changed by `update_splits` between the tip and
the read, and the last recipient's share includes rounding dust that depends
on the exact amount. `amount` is the gross tip and `fee` is the
[protocol fee](#the-protocol-fee-79) this tip moved to the admin (`0` while no
fee is set). `fee` plus the `moved` values equals `amount`, except that a
recipient who is also the sender is reported with `moved = 0` and keeps their
share (see below).

All fields are named in the event map. Every settle path publishes the same
fields — an absent reference is an explicit `None` — so consumers can decode
by field name and ignore fields added by future versions.

The `held` flag distinguishes a **delivered** share (`false`) from one the
contract could not deliver and has escrowed for later withdrawal (`true`, #171).
For a held triple, `moved` is the amount the contract took into custody for that
recipient.

The triples report **what each transfer actually moved**, not the notional
`amount * bps / 10_000` that [`preview_split`](#previewing-a-tip) quotes. The
two differ in one case: `tip` skips paying a recipient who is also the sender,
so that entry reports `0` where the preview would have shown their full share. The recipient is
still listed, which lets a consumer tell "listed but paid nothing this tip"
apart from "not in this jar". The skipped share stays with the tipper; no
other entry grows to absorb it (#78).

`breakdown.len()` equals the jar's split count at the moment of the tip, so it
is at most `MAX_RECIPIENTS` (20) entries — see
[`get_limits`](#reading-the-contracts-limits). The payload stays bounded.

Note that `breakdown` is a snapshot, not the jar's current state. To ask
whether an address is a recipient *now*, use
[`is_recipient`](#confirming-you-are-a-recipient).

> **Consumer impact.** The outer data tuple keeps its four elements
> (`from, amount, message, breakdown`), but each `breakdown` entry is now a
> **triple** `(recipient, moved, held)` rather than a pair — the extra `held`
> flag marks a share the contract escrowed instead of delivering (#171).
> `decodeTipEvent` in `@novatip/sdk` and the backend indexer must read the third
> element of each breakdown entry before consuming events from a contract built
> from this version. A recipient who cannot receive no longer aborts the whole
> tip, so indexers that assumed all-or-nothing must treat `held = true` entries
> as escrowed (claimable via `withdraw_held`) rather than as failures.

#### `seq` — the tip's sequence number

`seq` is a contract-wide counter, incremented once per successful tip, and is
also the return value of `tip` / `tip_with_ref`. Numbers start at `1` and a
reverted tip consumes none, so the sequence has no holes: an indexer that has
stored `1..N` and reads `N` from `get_tip_count` has lost nothing, and a gap
means its own scan is incomplete rather than that a tip never happened.

This exists because consumers previously keyed on the transaction hash. The
backend indexer upserts on `txHash` and the SDK's `TipEvent` tells callers to
deduplicate on it — which holds only while there is one tip per transaction. A
smart wallet batching two tips, or any client submitting tips from one account
in a single transaction, puts several `tip` events in one transaction that all
share that hash, and every tip after the first is dropped as a duplicate.
`seq` is unique per tip by construction.

`seq` is deliberately in the **data**, not the topics. The SDK's
`tipTopicFilter` must have exactly one segment per published topic, so adding a
third topic would make its existing two-segment filter match nothing — silently,
for every consumer. `get_tip_count() -> u64` exposes the running total, `0` on a
fresh deploy.

#### `reference` — an optional client id

`reference` lets the integration that started a tip recognise it when the event
comes back through an indexer or a webhook. An embeddable button, a stream
overlay, and a Discord/Telegram bot each create something off-chain first (a
pending alert, a bot command, a checkout session) and then wait for the matching
`tip` event. Sender, amount and message are none of them unique, and the message
is visible to the creator, so it cannot carry an opaque id.

The value is opaque to the contract and the contract's role stops at echoing it:

- it is **not stored** — no key is written, and reading the jar or the tip count
  is unaffected by whether a tip carried one;
- it is **not checked for uniqueness** — two integrations may send the same
  reference, and two tips in the same jar may carry it. Reconciling a collision
  is the consumer's job; if a consumer needs uniqueness it must use `seq`, which
  the contract does guarantee;
- it **costs nothing when absent** — `tip` passes `None`, and an absent
  reference still occupies position six.

It is exactly 32 bytes (`Option<BytesN<32>>`): enough for a binary UUID, a
`sha256` tag, or a big-endian primary key, and bounded so the payload cannot grow
the way an unbounded string would. The entry point is `tip_with_ref`; `tip` is
unchanged and remains the positional call the SDK and the web tip form use.

> **Consumer impact.** SDK and backend decoders must switch from tuple
> destructuring to named fields. Unknown fields can be ignored safely.
>
> An earlier change in the same section took the tuple from three elements to
> four by appending `breakdown`; that wording describes the `breakdown` addition
> and is kept for the migration note.

### `lock` — published on `lock_splits` and `unlock_splits`

- **Topics:** `(symbol "lock", jar_id: String)`
- **Data:** `JarLockEvent { version: u32, locked: bool }` (`true` when locked, `false` when unlocked)

Lets an indexer cache whether a jar protects its collaborators against a
unilateral owner edit (#173). The flag is named and versioned, so a decoder
reads it by field rather than relying on the payload being a bare `bool`.

### `jar_pause` — published on every `set_jar_paused`

- **Topics:** `(symbol "jar_pause", jar_id: String)`
- **Data:** `JarPausedEvent { version: u32, paused: bool }` (`true` when paused, `false` when resumed)

Lets an indexer cache whether a jar is currently refusing tips (#45), so a tip
page can show a "paused" badge without a separate read. Emitted even when the
flag is set to the value it already held, so the intent is always recorded.

### `withdraw` — published on every successful `withdraw_held`

- **Topics:** `(symbol "withdraw", recipient: Address)`
- **Data:** `HeldWithdrawnEvent { version: u32, amount: i128 }`

Records the moment an escrowed share (#171) is released to its recipient. The
recipient is a topic (not just data) so an indexer can subscribe to one
address's withdrawals. `amount` is named and versioned, so a future field (the
token withdrawn, for example) can be added without changing the payload type.

### `adm_xfer` — published on every successful `transfer_admin`

- **Topics:** `(symbol "adm_xfer")`
- **Data:** `AdminTransferredEvent { version: u32, admin: Address, new_admin: Address }` (`admin` is the outgoing key)

Publishes both ends of the admin handover, so an indexer can track who held the
role without replaying every prior transfer. The data is a named struct rather
than a positional tuple, so fields can be added without breaking decoders.

- **Example (decoded data):** `{ "version": 1, "admin": "GOLD…ADMN", "new_admin": "GNEW…ADMN" }`

### `jar_rnm` — published on every successful `rename_jar`

- **Topics:** `(symbol "jar_rnm", jar_id: String)` — the **old** jar id
- **Data:** `JarRenamedEvent { version: u32, jar_id: String, new_jar_id: String }`

Lets an indexer move a jar in its cache immediately. The old id is a topic so a
subscription for that jar sees the rename, and the data carries both ids so the
event is self-describing. The payload is a named struct rather than a
positional pair.

- **Example:** topics `["jar_rnm", "@old-name"]`, data
  `{ "version": 1, "jar_id": "@old-name", "new_jar_id": "@new-name" }`

### `recr_add` / `recr_del` — recurring-tip registration and cancellation

- **`recr_add` Topics:** `(symbol "recr_add", supporter: Address)`, **Data:** `id: u128`
- **`recr_del` Topics:** `(symbol "recr_del", supporter: Address)`, **Data:** `(id: u128, cancelled_by: Address)`, where `cancelled_by` is the supporter or the jar owner (#324)
- **`recr_add` Topics:** `(symbol "recr_add", supporter: Address, jar_id: String)`, **Data:** `RecurringAddedEvent { version: 1, id: u128, jar_id: String, amount: i128, period_ledgers: u32, next_due_ledger: u32 }` (#317). A creator filters on the third topic to list their recurring supporters without calling `get_recurring` per id. **SDK impact:** decoders that read the old bare `id` must read `.id`, and subscriptions matching the two-element topic must add the jar.
- **`recr_upd` Topics:** `(symbol "recr_upd", supporter: Address)`, **Data:** `RecurringUpdatedEvent { version: 1, id: u128, amount: i128, message: String }` (#316)
- **`recr_del` Topics:** `(symbol "recr_del", supporter: Address)`, **Data:** `id: u128`

Publishes the assigned schedule id (and `recr_del` the id it removed).
A schedule becomes a payment rail through `collect_recurring` (#175): the
supporter approves this contract as a token spender, and any keeper collects
due periods one at a time. Each collection publishes the ordinary `tip` event,
so indexers need no separate decoder for recurring payments.

> The `splits` event is also emitted by `leave_jar` (#172), not only by
> `update_splits`, so an indexer refreshes its cached splits after a recipient
> removes themselves.

### Admin events — one shape

Every admin-gated configuration change publishes the same shape:

- **Topics:** `(symbol "<name>")` — the event name only.
- **Data:** a versioned struct whose first two fields are `version: u32` and
  `admin: Address`, followed by the value that changed.

An indexer therefore reads the admin from the same field of every admin event
instead of special-casing whether it is a topic or the whole payload. The
events are:

| Event | Data struct | Value |
|-------|-------------|-------|
| `tk_allow` | `TokenAllowlistEvent` | `token`, `allowed: true` |
| `tk_dis` | `TokenAllowlistEvent` | `token`, `allowed: false` |
| `tip_hook` | `TipHookEvent` | `hook: Option<Address>` (`None` clears it) |
| `reg_set` | `RegistrarEvent` | `registrar: Option<Address>` (`None` clears it) |
| `fee_set` | `FeeEvent` | `fee_bps: u32` |
| `fee_rcpt` | `FeeRecipientEvent` | `recipient: Option<Address>` (`None` clears it) |
| `pause` | `PauseEvent` | `paused: true` |
| `resume` | `PauseEvent` | `paused: false` |
| `adm_xfer` | `AdminTransferredEvent` | `new_admin: Address` (`admin` is the outgoing key) |
| `upgrade` | `UpgradedEvent` | `previous_hash: Option<BytesN<32>>`, `new_hash: BytesN<32>` |
| `migrate` | `MigratedEvent` | `from_version: u32`, `to_version: u32` |

`tip_hook` records who changed the post-tip hook configuration and to what
(#176). `reg_set` records every installation or removal of the `create_jar`
registrar (#178), including the moment the squatting gate goes up or down.

For `upgrade` (#102, #334), `previous_hash` is the hash recorded by the last
upgrade. soroban-sdk 22 exposes no deployer getter for the code hash the
contract is currently running, so the **first** upgrade of a deployment that
has never upgraded since this field was introduced reports `None`. Every later
upgrade reports the hash installed by the previous one. The new build's
`INTERFACE_VERSION` cannot be read inside the upgrade transaction — the
executing code is still the old build — so clients call `version()` after the
ledger closes if they need it.

The per-event sections below give the topics, data and an example decoded
value for each admin event that has no section of its own elsewhere in this
chapter. Addresses and hashes are shortened with `…`. Decode the data by field
name, as for every other event.

### `pause` — published on every `pause`

- **Topics:** `(symbol "pause")`
- **Data:** `PauseEvent { version: u32, admin: Address, paused: bool }` (`paused` is always `true`)
- **Example (decoded data):** `{ "version": 1, "admin": "GADM…ADMN", "paused": true }`

Published each time the admin calls `pause`, including when the contract is
already paused. From then on the tip entry points reject with `ContractPaused`;
see [what each pause blocks](#what-each-pause-blocks).

### `resume` — published on every `resume`

- **Topics:** `(symbol "resume")`
- **Data:** `PauseEvent { version: u32, admin: Address, paused: bool }` (`paused` is always `false`)
- **Example (decoded data):** `{ "version": 1, "admin": "GADM…ADMN", "paused": false }`

Published each time the admin calls `resume`, including when the contract was
not paused. `pause` and `resume` share one data struct, so an indexer can keep
the current state from the `paused` field of the latest of either event.

### `upgrade` — published on every successful `upgrade`

- **Topics:** `(symbol "upgrade")`
- **Data:** `UpgradedEvent { version: u32, admin: Address, previous_hash: Option<BytesN<32>>, new_hash: BytesN<32> }`
- **Example (decoded data):** `{ "version": 1, "admin": "GADM…ADMN", "previous_hash": "9f2c…41ab", "new_hash": "c07e…5d10" }`

Published after the contract code has been replaced. `previous_hash` is `None`
on the first upgrade of a deployment that has never upgraded since the field was
added (see above). Call `version()` after the ledger closes to learn the new
build's interface version, then `migrate()` if the release notes say the schema
changed ([upgrade, then migrate](#upgrade-then-migrate-325)).

### `tk_allow` — published on every successful `allow_token`

- **Topics:** `(symbol "tk_allow")`
- **Data:** `TokenAllowlistEvent { version: u32, admin: Address, token: Address, allowed: bool }` (`allowed` is always `true`)
- **Example (decoded data):** `{ "version": 1, "admin": "GADM…ADMN", "token": "CDLZ…XLMC", "allowed": true }`

Published whenever `allow_token` succeeds, including for a token that was
already allowed. After it, `tip_in_token` and `tip_many` accept `token`.
`allow_token` fails with `TooManyAllowedTokens` and publishes nothing when the
list is full.

### `tk_dis` — published on every successful `disallow_token`

- **Topics:** `(symbol "tk_dis")`
- **Data:** `TokenAllowlistEvent { version: u32, admin: Address, token: Address, allowed: bool }` (`allowed` is always `false`)
- **Example (decoded data):** `{ "version": 1, "admin": "GADM…ADMN", "token": "CDLZ…XLMC", "allowed": false }`

Published whenever `disallow_token` succeeds, including for a token that was
not on the list. Trying to disallow the default token fails with
`CannotDisallowDefault` and publishes nothing. `tk_allow` and `tk_dis` share
one struct, so the latest event for a token gives its state in `allowed`.

### `fee_set` — published on every successful `set_fee_bps`

- **Topics:** `(symbol "fee_set")`
- **Data:** `FeeEvent { version: u32, admin: Address, fee_bps: u32 }`
- **Example (decoded data):** `{ "version": 1, "admin": "GADM…ADMN", "fee_bps": 100 }`

Published whenever `set_fee_bps` stores a new fee, including `0` and the
current value. A fee above `MAX_FEE_BPS` (500) fails with `FeeTooHigh` and
publishes nothing. The fee applies to tips settled after this event; each
`tip` event reports the fee it actually moved in its `fee` field.

### Upgrade, then migrate (#325)

`upgrade` swaps the code but cannot transform stored state. A build that
changes storage bumps `SCHEMA_VERSION` and adds a step to `migrate`, guarded
by the stored schema version. The procedure is:

1. Install and call `upgrade(new_wasm_hash)`.
2. In a later transaction, once the new code is live, call `migrate()`.

`migrate` is admin-only and idempotent: each step runs once, the new version
is recorded, and a `migrate` event reports `from_version` and `to_version`.
Calling it again does nothing. A deployment that has never migrated reads as
schema version 0.

## Locking a jar's splits

`lock_splits(jar_id)` turns a jar from the owner's alone into an agreement
between collaborators. Once locked, `update_splits` no longer accepts the
owner's signature by itself: **every current recipient whose share decreases or
who is removed must also authorize.** Recipients who are unchanged or increased,
and newly added recipients, do not sign — the lock only guards against being cut
out behind your back.

`unlock_splits(jar_id)` restores unilateral owner control, and because unlocking
re-enables reductions it requires authorization from **all** current recipients,
not just the owner; otherwise an owner could disarm the lock and cut shares a
moment later. `is_splits_locked(jar_id)` reads the flag.

The lock is a property of the jar, not the owner, so it **survives
`transfer_jar_ownership`**: handing control to a new owner does not silently
disarm the protection the collaborators agreed to. Interactions with the
recipient-self-removal (#172) and hold/escrow (#171) paths: `leave_jar` is
authorized by the departing recipient (who is consenting to their own removal),
so it is compatible with a lock; held balances are keyed by recipient and are
unaffected by locks or split edits.

## Pausing a jar (#45)

`set_jar_paused(jar_id, true)` lets a creator stop tips arriving to one jar
without giving up the slug — useful mid-way through changing collaborators or
while a split is in dispute. Every tip entry point (`tip`, `tip_with_ref`,
`tip_in_token`, and each entry of `tip_many`) then rejects that jar with
`JarPaused`, checked after the jar is looked up and before any funds move.
`set_jar_paused(jar_id, false)` reopens it, and `is_jar_paused(jar_id)` reads
the flag (which also rides on the `Jar` that `get_jar` returns).

The pause only closes the tip paths. Every read stays available throughout, so a
tip page can still resolve the jar, show its splits, and preview a tip while
explaining why it is temporarily closed. Like the split lock, the flag is a
property of the jar rather than the owner, so it **survives
`transfer_jar_ownership`**: a handover does not silently reopen a jar the
previous owner closed. It is independent of the contract-wide
[`pause`](#admin-only-functions): either being set is enough to refuse a tip,
and the per-jar switch is owner-gated while the contract-wide one is admin-gated.
`token` is the address the tip was paid in — `get_token()` for a `tip` call,
the caller's argument for a `tip_in_token` call. It has to be in the event: the
other four elements carry a number and nothing else says which asset the number
counts, so an indexer could not tell a tip of 10 USDC from a tip of 10 XLM, and
a jar tipped in two assets would have its per-supporter balances summed across
them as if they were one currency. Reading it from the transaction instead is
not enough, because the contract calls the token itself and the event consumer
sees only the tip's own envelope.

> **Consumer impact.** The data tuple grew from three elements to four, and
> now from four to five. A decoder that reads it positionally —
> `decodeTipEvent` in `@novatip/sdk`, and the backend indexer — must be updated
> to accept the fifth element before it reads events from a contract built
> from this version. The first four are unchanged and keep their positions, so
> a consumer that ignores trailing elements is unaffected; a consumer that
> validates the element count rejects them all, which is why this shape change
> also bumps [`version()`](#detecting-the-interface-version).

### What each pause blocks

Two switches refuse tips: the contract-wide `pause()` (admin) and the per-jar
`set_jar_paused(jar_id, true)` (jar owner). Either one is enough. This table
lists every state-changing entry point and whether each switch blocks it, as
the code behaves today. "Yes" means the call fails with the error shown and
moves nothing.

| Entry point | Blocked by contract pause | Blocked by jar pause |
|-------------|---------------------------|----------------------|
| `tip`, `tip_with_ref` | Yes, `ContractPaused` | Yes, `JarPaused` |
| `tip_in_token` | Yes, `ContractPaused` | Yes, `JarPaused` |
| `tip_many` | Yes, `ContractPaused`, before any entry | Yes, `JarPaused` for a paused jar in the batch; the whole batch reverts |
| `collect_recurring` | No, see [#220](https://github.com/Novatip/novatip-contracts/issues/220) | No, see [#220](https://github.com/Novatip/novatip-contracts/issues/220) |
| `create_recurring`, `update_recurring`, `cancel_recurring` | No | No |
| `withdraw_held`, `withdraw_held_to` | No | No |
| `create_jar`, `rename_jar`, `delete_jar` | No | No |
| `update_splits`, `change_recipient`, `leave_jar` | No | No |
| `lock_splits`, `unlock_splits` | No | No |
| `set_jar_paused` | No | No (it is the switch) |
| `set_min_tip_amount`, `set_max_tip_amount`, `set_jar_tokens_default_only` | No | No |
| `transfer_jar_ownership`, `admin_recover_jar` | No | No |
| `pause`, `resume` | No (it is the switch) | No |
| `allow_token`, `disallow_token`, `set_fee_bps`, `set_fee_recipient`, `set_tip_hook`, `set_registrar` | No | No |
| `transfer_admin`, `upgrade`, `migrate` | No | No |

Reads and previews (`preview_split`, `preview_split_details`, `preview_tip`,
`get_held`, `get_jar` and the other getters) are never blocked, so a tip page
can still show a paused jar and explain why it is closed. Some reads extend
storage TTLs, but none moves funds.

Neither switch holds back recipients or owners: a recipient can still withdraw
a held share, and an owner can still edit, rename, transfer or delete a jar
while either pause is on. `collect_recurring` should honour both switches like
the tip entry points; until #220 is fixed, a keeper can still collect a due
period while the contract or the jar is paused.

## When a share is held (#171)

A tip pays each recipient in its own token transfer. If one recipient cannot
receive the token, the tip does not fail. The contract keeps that recipient's
share for them instead, and they withdraw it later. This section follows one
held share from the tip to the withdrawal.

### Cause

The contract tries each recipient's transfer. When the token refuses it, the
contract moves the share to its own address and credits it to the recipient.
The usual reasons are:

- the recipient's account has no trustline for the token (for classic USDC,
  every `G…` account needs one), or the trustline was removed;
- the token issuer has not authorised the account, or has revoked it.

The tipper pays the full amount either way, and the other recipients are paid
as normal. In the `tip` event, the recipient's `breakdown` entry has
`held: true`, and `moved` is the amount the contract took into custody. This
`held: true` is what a dashboard shows.

### Where the money is

The tokens are in the contract's own token balance. The amount owed is
recorded in persistent storage under `Held(recipient)`. There is one balance per
recipient address, and it adds up every held share from every jar and every
tip. Nobody else can withdraw it. The admin has no sweep, and the jar owner
cannot redirect it.

### Worked example

Jar `@band` has two recipients: Alice at 7000 bps and Bob at 3000 bps. The
protocol fee is 0. Bob has no USDC trustline. A supporter tips 10 USDC
(`100000000` in the token's 7-decimal units):

- Alice receives `70000000` directly.
- Bob's `30000000` goes to the contract and is credited to `Held(Bob)`.
- The `tip` event's breakdown is
  `[[Alice, 70000000, false], [Bob, 30000000, true]]`.
- `get_held(Bob)` now returns `30000000`.

Bob adds the trustline and calls `withdraw_held(Bob)`. He receives
`30000000`, the contract publishes `withdraw` with topics `["withdraw", Bob]`
and data `{ "version": 1, "amount": 30000000 }`, and `get_held(Bob)` returns
`0` again.

### Check

`get_held(recipient)` returns the amount owed in the token's smallest units,
or `0`. Anyone can call it, and no signature is needed:

```bash
stellar contract invoke --id "$CONTRACT_ID" --network testnet --source "$SOURCE" \
  -- get_held --recipient G...RECIPIENT
```

To find out which tips caused the balance, filter the `tip` events for
breakdown entries with `held: true` for that address
(`EVENT=tip SLUG=@band ./scripts/events.sh`).

### Fix

Make the account able to receive the token before withdrawing. Otherwise the
withdrawal fails the same way the original transfer did.

- **Missing trustline:** `IDENTITIES=G...RECIPIENT ./scripts/trustline.sh`
  adds it, and `./scripts/trustline.sh --check` reports it. See
  [USDC trustlines](#usdc-trustlines).
- **Not authorised by the issuer:** ask the issuer to authorise the account.
  The contract cannot do this.

### Withdraw

The recipient signs `withdraw_held(recipient)`, which pays the whole balance in
one transfer and publishes a `withdraw` event:

```bash
stellar contract invoke --id "$CONTRACT_ID" --network testnet --source recipient \
  -- withdraw_held --recipient G...RECIPIENT
```

`scripts/withdraw-held.sh` does the same in one step. It reads `get_held`,
checks the trustline, withdraws, and reads the balance back. It fails with
`NothingHeld` (18) when nothing is owed. If the recipient's own account can
never receive the token (a lost key, or an account the issuer will not
authorise), the recipient can sign `withdraw_held_to(recipient, to)` to send
the balance to another address. That publishes `wd_to` instead.

### If it is never withdrawn

The balance does not expire, and nobody else can claim it. The funds stay in
the contract until the recipient withdraws. The storage entry has a TTL,
though. Each new held share and each `get_held` call with a positive balance
extends it, but if it lapses, the entry is archived. Restore it before
withdrawing; see [Storage model and idle jars](#storage-model-and-idle-jars-56).
The event log alone cannot recreate a spendable balance.

### Limitation: one token

`Held(recipient)` is a single number with no token attached, and
`withdraw_held` always pays the contract's default token (`get_token()`). A
share held from `tip_in_token` in another token is added to the same number
and would be paid out in the default token. Until per-token escrow lands in
[#219](https://github.com/Novatip/novatip-contracts/issues/219), only held
shares from default-token tips are reliably withdrawable as the asset that
was tipped.

## Reading tip totals

Two views report what has actually settled on-chain:

```rust
get_jar_stats(jar_id)                -> JarStats       { tip_count, total }
get_supporter_stats(jar_id, supporter) -> SupporterStats { tip_count, total }
```

Both are bumped at the end of a tip that paid out, and neither is written by a
tip that reverted — a transfer that traps aborts the whole call, counters
included. That is what makes them usable as a check rather than a second
opinion:

- `get_jar_stats` is the figure a creator compares against the backend's. The
  indexer has blind spots it cannot report: `planBatch` gives up on any ledger
  that returns a full page of events, and an event older than the RPC's
  retention window can never be backfilled. A missed tip silently lowers a
  creator's total, and until now nothing on-chain contradicted it. A third-party
  page or embeddable button also gets a total with no dependence on Novatip's
  backend.
- `get_supporter_stats` is the data the roadmap's `supporter_badge` contract
  needs. Milestones are phrased as "10 tips or $10 cumulative", and a contract
  cannot read a Postgres row; before this, a badge could only be minted by a
  trusted off-chain service asserting the milestone.

A jar that exists but has never been tipped reads `tip_count: 0, total: 0`, as
does a supporter who has not tipped a given jar. A **free slug** is
`JarNotFound`, not zeros — a typo'd handle must not look like a jar nobody has
tipped.

**Units.** `total` is the raw sum of amounts in the token's smallest unit. It
does not normalize or convert different tokens. For example, adding 1 USDC and
1 EURC (both with six decimal places) produces `2_000_000`, which is not a
meaningful currency total. Consumers that need per-token totals should index
`tip` events, which include the token, and group amounts by token. The planned
[per-token stats enhancement (#314)](https://github.com/Novatip/novatip-contracts/issues/314)
will add on-chain per-token totals.

**Storage.** `DataKey::Stats(jar_id)` and `DataKey::Supporter(jar_id,
supporter)` are persistent entries under the same TTL policy as the jar itself
(`JAR_TTL_LEDGERS`, bumped at the same threshold), and both views bump on read.
The supporter entry describes one supporter's relationship with one jar, so an
archived entry means they restore it or tip again — the history is still in the
event log, and a badge check that reads zeros asks for a restore rather than
minting on a wrong count.

### What it costs per tip

The totals are two extra persistent writes on a path that already writes.
Measured with `env.cost_estimate()` in `test::tip_writes_the_two_totals_entries`
(one recipient, one supporter tipping a fresh jar twice), against the same tip
with the counters disabled:

| | without totals | with totals | difference |
|---|---|---|---|
| Ledger writes | 4 | 6 | **+2 entries** |
| Bytes written | 884 | 1376 | **+492 bytes** |
| Persistent rent bumps | 2 | 4 | **+2** |

The write columns were re-measured against the merged payout path — the
"without totals" baseline is now 4 entries, not 3, because the tip sequence
counter writes the contract instance each tip and the escrow fallback (#171)
enlarged the per-tip footprint. The rent-bump columns are one lower than
before because `create_jar` now extends the jar's TTL, so a tip to a fresh jar
no longer bumps it. The **differential the #167 PR documented still
holds exactly: two writes, 404 bytes, two rent bumps per tip**. The modelled
CPU delta predates those merges and was not re-measured here.

The difference is flat, not cumulative: a second tip by the same supporter to
the same jar rewrites the same two entries and adds no new ones (the test
asserts this). What the extra rent buys is the ability to disagree with the
indexer — a tip the counters show and the database does not is a bug report,
where previously the two were indistinguishable.

## Tipping in another token

`tip` settles in the deploy-time token, fixed at construction. A supporter
holding EURC had to swap before tipping anything. The admin now manages an
allowlist and there is a second entry point:

```rust
allow_token(token)                     // admin only, idempotent
disallow_token(token)                  // admin only, refuses the default token
is_token_allowed(token) -> bool
tip_in_token(from, jar_id, token, amount, message)
```

`tip_in_token` checks the allowlist before it loads the jar or moves funds, so
an unlisted token fails with `TokenNotAllowed` and pays nobody. Everything else
about it is identical to `tip`: the same `compute_shares`, the same
self-transfer skip, the same breakdown, the same counters, the same event —
which is why the event names the token.

`tip` itself is unchanged and always pays through `get_token()`, which is
allowlisted by the constructor. That keeps every existing integration working,
and means `get_token()` and `is_token_allowed(get_token())` can never disagree.

**Why the default token cannot be disallowed.** `tip` ignores the allowlist by
design — it is the entry point the whole product is built on. Removing its
token would leave a working tip path for an asset the view calls closed, and
`get_token()` would return something `is_token_allowed` rejects. So
`disallow_token` refuses it with `CannotDisallowDefault`; an operator who wants
to retire the default asset deploys a new contract.

**Why the list lives in instance storage.** An allowlist entry is contract
configuration, in the same category as `Admin` and `Token`. A persistent entry
that nobody touches is archived silently, and the failure would look like
"EURC stopped working" with no admin action behind it — the one behaviour an
allowlist must not have. Instance storage is extended with the contract
instance, so an entry cannot expire ahead of the contract that reads it.

### The minimum tip, and the decision recorded here

`set_min_tip_amount` stores a raw `i128` with no unit. Issue #170 asked for a
decision, and there were two defensible ones — apply the floor to the default
token only, or make it per-token:

**Chosen: the minimum applies to the default token only.** A tip in another
allowed asset is not gated by it, even if that asset is the one the minimum was
set in mind for. Making it per-token would mean changing `set_min_tip_amount`'s
signature and its storage key, which breaks every jar that already configured a
minimum — a migration for a feature nobody has asked to price per-asset. The
number is also genuinely unpriceable across assets: 500 is a different amount of
money in a 6-decimal token than in a 7-decimal one, so a per-token floor needs a
unit per entry, not a rename.

The consequence is real and deliberate: **a jar can be tipped below its floor in
another allowed asset.** A creator who cares about a floor should keep the
allowlist to the token they set the minimum in. `preview_split` keeps enforcing
the minimum as it always did, so it describes a default-token tip.

`compute_shares` needed no equivalent decision. It splits by basis points and
never interprets magnitude, so the same arithmetic is correct for any asset —
which is exactly what makes an EURC tip of 100 pay 70/30 just as a USDC tip of
100 does.

## Collecting a due recurring tip (#175)

`create_recurring` stores a schedule; `collect_recurring(id)` moves the money.
The payment rail is the **token allowance** the supporter granted this
contract: an `approve` on the token with `spender = tip_splitter` covering at
least one period's `amount`. With that in place, *anyone* may call
`collect_recurring` — typically the backend keeper or the jar's creator — and
no supporter signature is requested or needed.

A collection:

- checks the schedule exists (`RecurringNotFound`), that the ledger has
  reached `next_due_ledger` (`NotDue`), that the allowance covers `amount`
  (`InsufficientAllowance` — an expired allowance reads as zero and fails the
  same check), and that the supporter's balance covers it
  (`InsufficientBalance`); every one of these is raised **before any funds
  move**, so a rejected call changes nothing, and an early call does not
  cancel the schedule — the same call succeeds once the ledger is reached;
- splits `amount` with the **same** arithmetic `tip` uses (the shared
  `payout_amounts` helper: basis-point shares, dust to the last recipient,
  supporter-is-recipient skipped), and moves each recipient's share with
  `transfer_from` with the contract itself as the spender; a recipient who
  cannot receive is escrowed exactly as in `tip` (#171);
- publishes the **identical `tip` event** — supporter as `from`, the stored
  message, breakdown, token, sequence number, and an explicit absent
  reference — so the indexer, webhooks and analytics see a collected
  installment and a manual tip as the same thing;
- fires the post-tip hook (#176) like any settled tip;
- advances `next_due_ledger` by **exactly one `period_ledgers`**, anchored on
  the old due ledger rather than on "now". At most one period is collected per
  call, so a keeper that was offline for months cannot drain months in one
  call; a backlogged keeper catches up one installment per call, and the
  supporter's allowance is the hard ceiling on the total — revoking it (or
  `cancel_recurring`) stops collections dead.

The per-jar minimum tip is enforced at registration, not re-enforced at
collection: a minimum raised later must not silently strand schedules already
registered under the old floor. A supporter who wants the new floor honoured
cancels and re-registers.

### Running a keeper

Anyone can call `collect_recurring(id)` once a schedule is due; the caller (keeper) pays the Stellar transaction fee, and no supporter signature is required.

- **Discovering due schedules:** Index `recr_add` and `recr_del` contract events to maintain the active set of schedule IDs, and query `get_recurring(id)` to read each schedule's `next_due_ledger` and `period_ledgers`.
- **Polling cadence:** Poll at an interval shorter than `period_ledgers` (for example every few minutes on 5-second ledgers) and invoke `collect_recurring(id)` once `latest_ledger >= next_due_ledger`.

| Error | Meaning | Keeper action |
| ----- | ------- | ------------- |
| `NotDue` | `latest_ledger < next_due_ledger` | **Retryable** — wait until `next_due_ledger` |
| `InsufficientBalance` / `InsufficientAllowance` | Supporter balance or token allowance is currently below `amount` | **Retryable** — back off and retry later |
| `RecurringNotFound` | Schedule was cancelled or never existed | **Dead schedule** — remove `id` from active index |
| `JarNotFound` | Target jar no longer exists | **Dead schedule** — remove `id` from active index |

```bash
# Inspect a recurring schedule
stellar contract invoke --id <CONTRACT_ID> --network testnet -- get_recurring --id 1

# Collect one due period (keeper signs and pays the transaction fee)
stellar contract invoke --id <CONTRACT_ID> --source <KEEPER_ACCOUNT> --network testnet -- collect_recurring --id 1
```

## The post-tip hook (#176)

The admin can register one optional contract — the `supporter_badge` in the
roadmap — that is notified after **every** settled tip:

```rust
set_tip_hook(hook: Option<Address])   // admin-only; None clears
get_tip_hook() -> Option<Address>     // the current hook, if any
```

### The interface a hook must implement

The contract exposes it as a `#[contractclient]` trait (`TipHook`), so a hook
contract can code against it directly:

```rust
#[contractclient(name = "TipHookClient")]
pub trait TipHook {
    fn on_tip(env: Env, jar_id: String, from: Address, amount: i128);
}
```

`on_tip` receives the jar id, the tipper, and the tip amount. It is called
with no authorisation requested and its return value is ignored.

### When and how it is called

Every settle path — `tip`, `tip_with_ref`, `tip_in_token`, and a
`collect_recurring` installment — fires the hook **last**, after all transfers
and the `tip` event, through `env.try_invoke_contract`:

- If the hook **panics or returns an error, the failure is caught and
  discarded**: the tip has already paid out and published, and stays settled.
  A broken badge contract must never stop a creator being paid.
- If the hook burns the transaction's remaining instruction budget, the tip
  fails too. This is inherent to the Soroban budget model — `try_invoke_contract`
  can catch a contract error, but an out-of-gas abort tears down the whole
  transaction and cannot be caught anywhere inside it. **Budget exhaustion in
  a hook is therefore the one way a hook can still revert a tip.**

The combination is why the hook is an opt-in, admin-controlled pointer:
admins should point it at a contract they have audited and budget-checked,
and `get_tip_hook` lets anyone verify what is wired in. Every change emits a
`tip_hook` event carrying the admin who made it. With no hook set, tips behave
exactly as before the feature existed.

## The jar registrar and its trust model (#178)

Novatip derives a jar's on-chain id from the claimed slug (`@alice` for
`/alice`). The backend guarantees slug uniqueness **in its database**, but the
chain does not know about the claim: until Alice finishes onboarding, anyone
can call `create_jar("@alice", …)` first and strand her with an "already
registered on-chain" error that has no on-chain recourse.

`set_registrar(registrar: Option<Address>)` (admin-only) names a contract
whose **co-signature** `create_jar` then requires:

```rust
registrar.require_auth_for_args((jar_id.clone(), owner.clone()).into_val(&env));
```

- The co-signature is over the exact `(jar_id, owner)` pair being registered —
  a signature for a different slug or a different owner authorises nothing.
- The check runs **after** all input validation and the duplicate-slug check,
  so a malformed request never burns a co-signature, and a rejected call
  stores nothing.
- While no registrar is set, `create_jar` behaves **exactly as before**: owner
  auth only. Existing deployments and third-party users of the contract are
  untouched until the admin opts in — which is the reversible half of the
  rollout the issue asks for.
- Every installation or removal emits a `reg_set` event carrying the admin.

**The trust model, stated plainly.** The intended registrar is the Novatip
backend itself: it signs the authorisation entry when a slug claim succeeds,
and the creator's wallet submits the transaction as today. That converts
slug-squatting from *anyone on the public internet* into *only whoever holds
the backend's signing key* — and the backend was designed to hold no keys, so
this mechanism does not decide the trust question, it only makes it concrete
and reversible: an operational decision (key custody, probably a dedicated
low-privilege hot wallet that can sign nothing else) the team can take — or
roll back by clearing the registrar — without redeploying the contract. What
the gate never does is touch existing jars: the registrar can only gate
creations of *free* slugs; owners, splits and funds of registered jars remain
exactly as unreachable to it as they were before.

## The protocol fee (#79)

```rust
set_fee_bps(fee_bps: u32)   // admin only; 0..=500
get_fee_bps() -> u32        // 0 until set
```

Every tip used to go entirely to the jar's recipients, and the contract had no
way to fund the platform running it. A fee taken on-chain is more honest than
one taken off-chain: a tipper can read it before signing.

- **Default 0.** No deployment starts charging on upgrade; the admin opts in.
  The intended configured value is **100 bps (1%)** — Buy Me a Coffee takes 5%
  and Patreon 8–12%, and 1% keeps cross-border micro-tipping viable. Set it
  with `FEE_BPS=100 ./scripts/set-fee.sh`.
- **Capped at 500 bps (5%).** Anything higher is rejected with `FeeTooHigh`, so
  the fee can never be set to swallow a tip.
- **Taken before the split.** `fee = floor(amount * fee_bps / 10_000)` goes to
  the contract admin and `net = amount - fee` is split across the recipients.
  The too-small-to-split guard (`SplitAmountTooSmall`) and the rule that the
  last recipient absorbs rounding dust both apply to `net`, so the recipients'
  total plus the fee equals the tip exactly.

```
fee_bps = 100, splits alice 70%, bob 30%
tip 10 000  -> fee 100, alice 6 930, bob 2 970
tip 10 099  -> fee 100, alice 6 999, bob 3 000   (fee truncates; bob takes the dust)
```

Every preview (`preview_split`, `preview_split_details`, `preview_tip`) reports
post-fee shares, so a client shows the fee as `amount - sum(preview_split)`
without redoing the arithmetic. `get_min_accepted_tip` reports the smallest
*gross* amount whose net still pays every recipient.

The fee applies to `tip`, `tip_with_ref`, `tip_in_token` (in the token tipped),
and `collect_recurring` (drawn from the same allowance, which must still cover
`amount`). Stats and the post-tip hook record the gross `amount`. Two edge
cases follow the self-transfer rule: an admin tipping keeps the fee rather than
paying themselves, and the event then reports `fee = 0` because nothing moved.
A fee transfer the admin cannot receive reverts the tip; the admin can set the
fee to `0` to restore tipping.

`set_fee_bps` emits a `fee_set` event: topic `(symbol "fee_set")`, data
`FeeEvent { version, admin, fee_bps }` — the shared admin event shape.

## Detecting the interface version

```rust
version() -> u32   // reads no storage
```

The shape of `tip`'s event has changed twice and `jar_xfer`'s data once, always
under the same function names, and `decodeTipEvent` in the SDK kept reading
three elements after the contract published four. A client given only a
contract id had no way to find out which build it was talking to before
decoding, so a mismatch surfaced as a parse failure — or as silently ignored
new data.

`version()` lets it ask first. It returns a compile-time constant, so it costs
no storage read and works against a contract with no jars and no callers.

**When the number moves.** Bump `INTERFACE_VERSION` by one when a function is
added or removed, an error code is added or renumbered, or an event's shape
changes. Do not bump it for an internal refactor, a new storage key, or a doc
edit — none of those are visible to a consumer, and a version that changes when
nothing observable did is a version consumers stop trusting.

This build ships `version() == 10`. Every bump since `version()` was added:

| Version | What it added or changed | Issues |
|---------|--------------------------|--------|
| 1 | First numbered interface. Covers the escrow (`held`) breakdown in the `tip` event and `get_held` / `withdraw_held`, the token allowlist (`allow_token`, `disallow_token`, `is_token_allowed`, `tip_in_token`), the totals views (`get_jar_stats`, `get_supporter_stats`), the sequence-numbered `tip` event with `get_tip_count`, and `version()` itself. | #167, #168, #169, #170, #171 |
| 2 | Adds `collect_recurring`, `preview_tip`, `set_tip_hook` / `get_tip_hook`, `set_registrar` / `get_registrar`, and the `tip_hook` and `reg_set` events. Makes the `tip` event data one canonical struct for every settle path. Renumbers the colliding error codes 14/15/16 to 19–22. | #175, #176, #177, #178 |
| 3 | Adds the protocol fee: `set_fee_bps` / `get_fee_bps`, the `fee_set` event, the `FeeTooHigh` error (35) and the `fee` field in the `tip` event. A tipper who is also a recipient now keeps their own share, which `preview_tip` reports. | #78, #79 |
| 4 | Adds `tip_many`, the `TipRequest` type and the `BatchEmpty` (37) / `BatchTooLarge` (38) errors. No existing function, error or event changed. | #84 |
| 5 | Adds `set_jar_paused` / `is_jar_paused`, `delete_jar` and the `JarPaused` error (39). | #45 |
| 6 | The `splits` event from `leave_jar` uses the same `SplitsUpdatedEvent` as `update_splits` instead of a bare count. | follow-up to #172 |
| 7 | Every admin event gets one shape: a name-only topic and a versioned data struct starting with `version` and `admin`. `lock`, `jar_pause`, `withdraw`, `adm_xfer` and `jar_rnm` publish named, versioned structs. The jar id is added to the `jar_del` and `jar_rnm` topics. The `upgrade` event gains `previous_hash`. | #330, #331, #332, #333, #334 |
| 8 | Adds `update_recurring` and the `recr_upd` event, `get_jar_info`, `get_allowed_tokens` with the `TooManyAllowedTokens` error (40), `set_max_tip_amount` / `get_max_tip_amount` with the `max_tip` event and errors 41 and 42, and `migrate` with the `migrate` event. `recr_add` publishes `RecurringAddedEvent` with the jar in its topics. `cancel_recurring` takes the caller, which may be the jar owner, and `recr_del` reports who cancelled. `tip_many` takes a batch `token` and `TipRequest` gains `reference`. `get_limits` reports `min_jar_id_len` and `max_fee_bps`. | #316, #317, #318, #319, #320, #321, #322, #323, #324, #325 |
| 9 | Adds admin-only `admin_recover_jar` and the `jar_recov` event. | #52 |
| 10 | Adds `change_recipient` (publishes `splits`), `withdraw_held_to` with the `wd_to` event, and `set_fee_recipient` / `get_fee_recipient` with the `fee_rcpt` event. No new error codes. | #310, #311, #312 |

`CHANGELOG.md` has the full entry for each change.

**How a client should use it.** Treat an unrecognised number as "newer than I
was built for", not as an error. Every field a lower version documented still
exists at a higher one — changes are appended — so the safe response is to read
what you know and ignore the rest. Refusing to connect on a version you have
not seen would turn a future additive release into an outage.

`version()` is per-contract feature detection. It complements the per-event
schema version proposed in #83 rather than replacing it: that answers "what
shape is this record", this answers "what build is this deployment".

## Jar discovery — design decision

Indexing needs a way to enumerate every registered jar. The previous contract
design kept that list on-chain as a `get_jar_ids() -> Vec<String>` view backed
by an instance-storage vector that grew by one entry on every `create_jar` call.
This had two problems:

1. **Unbounded growth.** The vector had no removal path and no size cap, so it
   grew permanently with every jar ever created.
2. **Escalating cost.** Instance storage is read and written in full on every
   `create_jar`. As the vector grew, each new jar creation cost more than the
   last, and the entry would eventually approach the ledger entry size limit,
   causing `create_jar` to fail for all callers.

**Decision — the design in use today:** keep no on-chain list and move discovery
to the event log. `create_jar` emits a `jar_crtd` event carrying the `jar_id`
and `owner`; indexers reconstruct the full jar list by scanning those events
from the contract's creation ledger (or from their last checkpoint). This is the
standard pattern for Soroban contracts where enumeration is needed but unbounded
on-chain state is not acceptable, and it keeps `create_jar` cost O(1) regardless
of how many jars exist.

### History, not instructions: this was never a `get_jar_ids` migration

The `get_jar_ids` view and its backing vector were **added and removed within a
single unreleased cycle** (see `CHANGELOG.md`), so no published build ever
exposed the function — there is nothing to migrate off or seed from. `v0.1.0`
ships only `__constructor`, `create_jar`, `update_splits`, `tip`, `get_jar` and
`get_token`.

The real gap is event coverage, not a removed function. `jar_crtd` is emitted
only by builds **after** `v0.1.0`:

- A deployment built from `v0.1.0` never emitted `jar_crtd` at all, so the jars
  it created are absent from the event log. The testnet contract referenced in
  the README predates the current event payloads as well.
- To find jars created on such a deployment, replay its `create_jar`
  invocations from Horizon (Soroban transaction history), or seed them from an
  off-chain snapshot — not from a `get_jar_ids` response, which no deployment
  has ever returned.

Every jar created by a build that emits `jar_crtd` needs no special handling:
scanning the event log from the contract's creation ledger is enough.

## Deploy & bootstrap

The scripts load `.env` from the repository root themselves — exported
variables win over `.env`, and no manual `source` step is needed. They work
when started from any directory.

```bash
./scripts/deploy.sh        # deploys, writes .contract-id.<NETWORK>
./scripts/create-jar.sh    # registers an example jar
./scripts/set-min-tip.sh   # sets (or clears) a jar's minimum tip
./scripts/transfer-jar.sh  # hands a jar to a new owner
```

See [`.env.example`](../.env.example) for required variables, and the
[README's scripts section](../README.md#scripts) for what each script needs and
how `MIN_TIP=none` reaches `Option::None`.
Once a jar exists:

```bash
./scripts/jar-info.sh                      # read-only: owner, recipients, minimum tip
AMOUNT=100000 ./scripts/jar-info.sh        # ... plus what a tip of that size pays each recipient
./scripts/update-splits.sh                 # replace a jar's splits (signed by the jar owner)
SPLITS='[{"to":"G...A","bps":7000},{"to":"G...B","bps":3000}]' ./scripts/update-splits.sh
```

`jar-info.sh` only simulates — it submits no transactions and pays no fee, and
any `SOURCE` identity works. `update-splits.sh` validates the new splits with
the same checks as `create-jar.sh` (both share `scripts/lib/splits.sh`), prints
the old and new splits side by side, refuses to send when they are unchanged,
and requires `SOURCE` to be the jar's owner before invoking `update_splits`.

See [`.env.example`](../.env.example) for required variables.

### USDC trustlines

Every classic `G...` account that sends **or receives** a tip needs a trustline
to the settlement asset first, and tips are all-or-nothing — one recipient
without a line fails the whole tip with the token's `Error(Contract, #13)`.
`scripts/trustline.sh` adds the line for the identities in `IDENTITIES`,
deriving the classic asset (`USDC:<issuer>`) from `USDC_TOKEN` itself, and
skips accounts that already have one. With `SLUG` set it also checks the jar's
recipients; `--check` reports without changing anything and exits non-zero if
anyone cannot be paid:

```bash
IDENTITIES="tipper alice bob" ./scripts/trustline.sh          # add lines
SLUG=@demo ./scripts/trustline.sh --check                     # report only
```

Testnet USDC is issued by the account encoded in the derived asset name
(`USDC:GBBD47IF6LWK7P7MDEVSCWR7DPUWV3NY3DTQEVFL4NAT4AQH3ZLLFLA5`
on testnet). There is no on-chain claim — fund an account by payment from a
holder (see the usage comment at the top of `scripts/trustline.sh`), after
`stellar fund` covers the XLM reserves.

### Inspecting events

When an indexer misses something, the first question is whether the event was
ever emitted. `scripts/events.sh` computes the topic filters with
`stellar xdr encode --type ScVal` instead of hand-rolled base64 (a wrong
encoding fails loudly, a wrong segment count silently returns nothing — see
the encoding walkthrough in the issues thread #57):

```bash
EVENT=tip SLUG=@demo ./scripts/events.sh   # tips to one jar
EVENT=all ./scripts/events.sh              # every event type, recent window
```

It lists the jar events `jar_crtd`, `splits`, `jar_xfer`, `jar_rnm`, `min_tip`
and `tip`, and the admin events `tk_allow`, `tk_dis`, `tip_hook`, `reg_set`,
`fee_set`, `pause`, `resume`, `adm_xfer` and `upgrade`, one line per event with
ledger, name, jar id and decoded payload, defaulting to a recent ledger window.
The [Events](#events) chapter has a section per event with its topics and data
fields, which is how to read the decoded payload. A `START_LEDGER` outside the
RPC's retention window gets an explanatory error. Sample output is in the
script's usage comment.

## Calling from another contract

`tip` authorizes `from` with `from.require_auth()`, which a calling contract
satisfies automatically — but the per-recipient `transfer(from, …)` calls it
then makes are invoked by `tip_splitter`, not by the caller, so they are not
covered. A contract tipping from its own balance must pre-authorize each
transfer with `env.authorize_as_current_contract`, listing every recipient and
the exact amount, or the tip fails with an auth error.

The amounts to authorize are what `preview_tip(jar_id, caller, amount)`
returns, skipping any `0` entry — a calling contract that is itself a recipient
makes no transfer to itself and keeps its share. When a
[protocol fee](#the-protocol-fee-79) is set, also authorize one transfer of the
fee to `get_admin()`: the fee is `amount - sum(preview_split(jar_id, amount))`.

`examples/tip-forwarder` is a worked example of exactly this — one function
that tips a jar from the contract's own balance — with tests that run it
against the real `TipSplitter` and a test SAC, including the failure mode where
the fee transfer is not authorized. See its
[`README.md`](../examples/tip-forwarder/README.md).

## Troubleshooting

Common failures encountered when building, deploying, or invoking the contract that do not map to contract error codes:

### 1. Missing WebAssembly target during build

**Symptom:**
```text
error: target 'wasm32-unknown-unknown' not found
error: the contract build failed. If it is a missing target, run:
       rustup target add wasm32-unknown-unknown
```
or:
```text
error[E0463]: can't find crate for 'core'
```

**Cause:** The Rust toolchain does not have the compilation target for Soroban WASM installed.

**Fix:** Add the target using `rustup`:
```bash
rustup target add wasm32-unknown-unknown
```

### 2. Recipient without a trustline for the token

**Symptom:**
```text
HostError: Error(Contract, #1)
```
or transaction simulation failure:
```text
txFailed: op_not_authorized / op_no_trustline
```

**Cause:** The Stellar Asset Contract (SAC) requires an existing trustline for custom/non-native assets before an account can receive tokens. If a recipient in a jar split has not established a trustline for the token, direct transfers to that address fail.

**Fix:**
- The recipient must open a trustline for the token's Stellar Asset Contract before receiving:
  ```bash
  stellar contract invoke --id <SAC_CONTRACT_ID> -- trustline --for <RECIPIENT_ADDRESS>
  ```
- Alternatively, if using the tip-splitter contract post-#171, undeliverable shares are automatically escrowed into contract storage. The recipient can withdraw their held funds via `withdraw_held(recipient)` once their trustline is established.

### 3. Unfunded source account

**Symptom:**
```text
error: transaction simulation failed: HostError: Error(Storage, MissingValue)
```
or:
```text
txNO_ACCOUNT: The source account does not exist
```

**Cause:** The invoking account has not been created on the network and holds 0 XLM (below the network reserve).

**Fix:** Fund the account with the minimum reserve:
- On Testnet, use Friendbot:
  ```bash
  curl "https://friendbot.stellar.org?addr=<ACCOUNT_ADDRESS>"
  ```
- On Mainnet, send at least 1–2 XLM from an existing wallet or exchange to establish the account.

### 4. Jar slug mismatch or invalid format

**Symptom:**
```text
HostError: Error(Contract, #3)   # JarNotFound
HostError: Error(Contract, #9)   # InvalidJarId
```

**Cause:** A tip or view was called with a slug that lacks the required leading `@` (producing `InvalidJarId`), uses invalid characters/length, or points to an unregistered jar slug (`JarNotFound`).

**Fix:** Ensure the slug matches `@` followed by 3–32 lowercase alphanumeric characters, dashes, or underscores (`@<slug>`), and verify registration using `get_jar(jar_id)` or `./scripts/jar-info.sh`.


## Transaction cost and recipient count (#55)

A `tip` makes one token transfer per recipient. The cost of a tip therefore
grows with the number of recipients in the jar. A solo jar makes one transfer.
A jar with twenty recipients makes up to twenty.

What adds to the cost of one tip:

- One `transfer` for each recipient with a non-zero share. A tipper who is also
  a recipient is skipped (see [when the tipper is also a recipient](#when-the-tipper-is-also-a-recipient-85)).
- One more `transfer` for the protocol fee, when a fee is set and the tipper is
  not the admin.
- For each recipient who cannot receive the token, one transfer into the
  contract and one persistent write under `Held(recipient)` (#171).
- A `tip` event with one `breakdown` entry per split, so the event grows with
  the recipient count.

What does not grow: the jar totals and the supporter totals are two writes
whatever the recipient count (see [reading tip totals](#reading-tip-totals)).

The tipper's wallet shows one transfer approval per recipient, so a twenty-way
split asks for twenty approvals in one transaction.

**Why the cap is twenty.** `MAX_RECIPIENTS` is a compile-time bound on the
fan-out of one tip. Without it a single tip could issue an unbounded number of
transfers. `tip_many` multiplies the bound by `MAX_BATCH_TIPS` (10), so the
worst case for one batch is 200 transfers (see
[tipping many jars at once](#tipping-many-jars-at-once-84)).

### Figures

The rows below are not yet measured. Do not quote numbers from this section
until they are filled in.

| Measure | Small split (2 recipients) | Large split (20 recipients) |
|---------|----------------------------|-----------------------------|
| Token transfers per tip (from the code, not measured) | 2 | 20 |
| CPU instructions | to be measured | to be measured |
| Read bytes | to be measured | to be measured |
| Write bytes | to be measured | to be measured |
| Ledger entries read / written | to be measured | to be measured |
| Minimum resource fee (stroops) | to be measured | to be measured |
| `tip` event size (bytes) | to be measured | to be measured |

### Method

1. Deploy the build from `main` to testnet. The testnet deployment listed in
   the README is behind `main`, so do not measure against it.
2. Create two jars: one with two recipients at 5 000 / 5 000 bps, and one with
   twenty recipients at 500 bps each. The last recipient takes any remainder.
3. Give every recipient a trustline to the settlement asset, so the escrow
   path is not measured by accident.
4. Use a supporter who is not on either jar, so no share is skipped.
5. Simulate `tip` for each jar with the same amount. Simulate only, so no
   balances change. Record the cost fields the simulation returns.
6. Repeat each simulation a few times and record the median.
7. Record the date, the ledger, and the Stellar CLI and SDK versions used.

As a cross-check, the test suite can take `env.cost_estimate()` before and
after a tip, as the totals section does. That gives relative deltas, not
testnet figures.

## Storage model and idle jars (#56)

### Where state lives

The contract uses two storage types. The table below lists every key in
`DataKey`, grouped by type.

| Storage | Keys | Extended when |
|---------|------|---------------|
| Instance | `Admin`, `Token`, `AllowedToken(addr)`, `AllowedTokens`, `TipCount`, `TipHook`, `Registrar`, `Paused`, `FeeBps`, `WasmHash`, `SchemaVersion` | `create_jar` and every settled tip. Reads do not extend it. |
| Persistent | `Jar(id)`, `MinTip(id)`, `Stats(id)`, `Supporter(id, addr)` | `Jar` and `MinTip` on every load or store of the jar, reads included. `Stats` and `Supporter` on every tip. |
| Persistent | `MaxTip(id)` | Only `set_max_tip_amount` and `rename_jar`. See the known gap below. |
| Persistent | `Held(addr)` | Each tip that escrows a share, and `get_held` while the balance is above zero. |
| Persistent | `Recurring(id)`, `RecurringCount` | Create, update, collect and get for the schedule. Create for the counter. |

Instance storage holds configuration that must not expire ahead of the
contract. Persistent storage holds per-jar and per-address records. The
allowlist is in instance storage for that reason: a silently archived entry
would look like a token that stopped working.

### Idle jars

Persistent entries have a time to live. Each access that finds the entry below
`JAR_TTL_THRESHOLD` (500 000 ledgers) extends it to `JAR_TTL_LEDGERS`
(1 000 000 ledgers). At a 5-second close rate, 1 000 000 ledgers is about 58
days. A jar that nobody reads or tips for about that long is archived.

- Reads count. Any call that loads the jar, such as `get_jar`, `preview_split`
  or `is_recipient`, keeps it alive. A jar that is only viewed does not
  archive.
- A call that touches an archived entry fails with a storage error from the
  host, not with a contract error code. Tips and reads of that jar fail until
  the entry is restored. Treat that failure as a storage problem, not as
  `JarNotFound`.
- To restore an entry, use a restore-footprint operation, then retry. The
  Stellar CLI provides this as `stellar contract restore`.
- An escrowed share has its own `Held` entry. If it archives, restore it before
  calling `withdraw_held`. The funds stay in the contract's token balance until
  then.
- The instance entry is extended only by `create_jar` and by tips. If the
  contract sees neither for about 58 days, the instance can archive, and every
  call fails until it is restored. The instance also holds the Wasm code.

**Known gap: `MaxTip`.** A jar's maximum tip is extended only by
`set_max_tip_amount` and `rename_jar`. Tips and reads do not extend it, even
though every tip reads it. An active jar with a maximum set can therefore
archive. Tips to that jar would then fail until the owner restores the entry or
clears the maximum. The fix belongs with the open issue on extending a jar's
time to live. That issue's number is not given here and should be added.

### What a client should do

- If a read or tip fails with a storage error, check whether the error names an
  archived or expired entry. If it does, restore the entry and retry.
- Keep the two error kinds apart: a contract code such as `JarNotFound` means
  the slug is free; a storage error means the record exists but is archived.
- Do not treat a failed `get_jar` as proof that a jar was deleted.

## Event encoding and `getEvents` filters (#57)

Every event has topics and data. Topics are XDR-encoded `ScVal` values. The
first topic is always the event name, as a `Symbol`. The data is a struct with
named fields, and its first field is `version`.

A filter matches topic segments in order. It must have one segment for each
topic the event publishes. A filter with the wrong number of segments, or with a
segment of the wrong type, returns no events and no error. A quiet filter looks
the same as a quiet contract.

### Topics and data by event

| Event | Topics, in order | Data |
|-------|------------------|------|
| `tk_allow` / `tk_dis` | `(sym)` | `TokenAllowlistEvent { version, admin, token, allowed }` |
| `jar_crtd` | `(sym, jar_id)` | `JarCreatedEvent { version, owner }` |
| `splits` | `(sym, jar_id)` | `SplitsUpdatedEvent { version, count, splits }` |
| `lock` | `(sym, jar_id)` | `JarLockEvent { version, locked }` (emitted by lock and unlock) |
| `jar_pause` | `(sym, jar_id)` | `JarPausedEvent { version, paused }` |
| `jar_xfer` | `(sym, jar_id)` | `JarTransferredEvent { version, previous_owner, new_owner }` |
| `jar_del` | `(sym, jar_id)` | `JarDeletedEvent { version, jar_id }` |
| `jar_rnm` | `(sym, old jar_id)` | `JarRenamedEvent { version, jar_id, new_jar_id }` |
| `min_tip` | `(sym, jar_id)` | `MinTipEvent { version, amount }` |
| `max_tip` | `(sym, jar_id)` | `MaxTipEvent { version, amount }` |
| `tip` | `(sym, jar_id)` | `TipEvent { version, from, amount, fee, message, breakdown, token, seq, reference }` |
| `withdraw` | `(sym, recipient)` | `HeldWithdrawnEvent { version, amount }` |
| `wd_to` | `(sym, recipient)` | `HeldWithdrawnToEvent { version, amount, to }` |
| `recr_add` | `(sym, supporter, jar_id)` | `RecurringAddedEvent { version, id, jar_id, amount, period_ledgers, next_due_ledger }` |
| `recr_upd` | `(sym, supporter)` | `RecurringUpdatedEvent { version, id, amount, message }` |
| `recr_del` | `(sym, supporter)` | tuple `(id, cancelled_by)` |
| `adm_xfer` | `(sym)` | `AdminTransferredEvent { version, admin, new_admin }` |
| `pause` / `resume` | `(sym)` | `PauseEvent { version, admin, paused }` |
| `upgrade` | `(sym)` | `UpgradedEvent { version, admin, previous_hash, new_hash }` |
| `migrate` | `(sym)` | `MigratedEvent { version, admin, from_version, to_version }` |
| `tip_hook` | `(sym)` | `TipHookEvent { version, admin, hook }` |
| `reg_set` | `(sym)` | `RegistrarEvent { version, admin, registrar }` |
| `fee_set` | `(sym)` | `FeeEvent { version, admin, fee_bps }` |

Notes on the table:

- `recr_add` has three topics. A creator who wants one jar's schedules filters
  on the third segment. A filter with two segments will not match it.
- `recr_del` is the one event whose data is a tuple, not a named struct.
- `max_tip`, `tip_hook`, `reg_set`, `fee_set` and `recr_upd` are emitted by the
  current build but were not in the earlier list of event names.

### Building a filter for `tip`

A `tip` publishes two topics: the symbol `tip`, then the jar id as a `String`.
The filter therefore has two segments.

- Segment 1, the symbol `tip`: `AAAADwAAAAN0aXAA`
- Segment 2, the jar id:
  - any jar: `*`
  - `@demo`: `AAAADgAAAAVAZGVtbwAAAA==`

A `getEvents` request that follows every tip, across all jars:

```json
{
  "startLedger": "<LEDGER>",
  "filters": [
    {
      "type": "contract",
      "contractIds": ["<TIP_SPLITTER_CONTRACT_ID>"],
      "topics": [["AAAADwAAAAN0aXAA", "*"]]
    }
  ]
}
```

The `*` in the second segment matches any jar id. It matches exactly one
segment. To follow one jar, replace `*` with that jar's encoded string.

Build each segment with `stellar xdr encode --type ScVal`, as
`scripts/events.sh` does, rather than by hand. The two strings above were
checked against the XDR layout (`ScVal` symbol is type 15, `ScVal` string is
type 14). The `*` wildcard and the outer and inner array shape follow the
Soroban RPC convention. They were not run against a live RPC in this document.

### Warnings

- A filter with the wrong segment count returns nothing. A `tip` filter with
  only the symbol segment matches no tips.
- A jar id must be a `String` segment. A `Symbol` segment, or a jar id without
  the leading `@`, matches nothing.
- The testnet deployment listed in the README is behind `main`. Its events may
  not have the shape above. Call [`version()`](#detecting-the-interface-version)
  before decoding.

## Security considerations (#58)

### Custody

- Tips do not sit in the contract. A tip moves funds from the tipper to each
  recipient, and the fee to the admin, in one call.
- One exception: a share that a recipient cannot receive, for example because of
  a missing trustline, is moved into the contract and recorded under
  `Held(recipient)`. The contract holds that balance until the recipient calls
  `withdraw_held`. So the contract is not strictly non-custodial. See
  [the trust implication of escrow](#splitting-rules).
- Only the named recipient can withdraw a held balance. `get_held` reports the
  amount.

### Who can do what

- **Tipper:** authorises its own tip. Nothing else is needed.
- **Anyone:** may call `collect_recurring`. It moves at most the supporter's
  approved amount for one due period, and it fails early with `NotDue`.
- **Jar owner:** splits, locking, pausing, minimum and maximum tips, renaming,
  deleting and transferring the jar. The full list is under
  [owner-only functions](#owner-only-functions).
- **Recipient:** `leave_jar`, `withdraw_held`, and consent when a locked jar's
  shares are reduced.
- **Supporter:** creates, updates and cancels a recurring tip. The jar owner can
  also cancel it.
- **Admin:** the ten admin-gated functions under
  [admin-only functions](#admin-only-functions).

### What the admin can and cannot do

The admin is the address stored at deploy time. Ten functions check it:
`allow_token`, `disallow_token`, `set_tip_hook`, `set_registrar`,
`set_fee_bps`, `transfer_admin`, `pause`, `resume`, `upgrade` and `migrate`.

The admin **can**:

- pause tipping for every jar (`tip`, `tip_with_ref`, `tip_in_token`, `tip_many`);
- set a protocol fee of up to 5% (500 bps), paid to the admin on each tip;
- add or remove tokens from the allowlist;
- point the post-tip hook at a contract, or clear it;
- require a registrar co-signature for new jars;
- transfer the admin role to another address, effective at once, with no
  acceptance step;
- replace the contract code with `upgrade`.

The admin **cannot**:

- move a jar's funds. No function sends a tip or a jar balance to the admin.
  The only transfer out of escrow is `withdraw_held`, and it pays only the
  named recipient;
- edit a jar it does not own. Splits, locks, pauses and ownership check the
  jar owner, and a locked jar also needs each affected recipient;
- change the settlement token after deploy;
- set a fee above 5%;
- stop `collect_recurring` or `withdraw_held` by pausing.

**The upgrade caveat.** `upgrade` replaces the whole contract code. A new build
can read or move anything the contract holds, including held balances. The
statement "the admin cannot move jar funds" describes the code as it is now,
not every future build. The admin key is the contract's root authority for that
reason.

### Other notes

- A recurring tip is pulled from the supporter's token allowance. Revoking the
  allowance, or cancelling the schedule, stops collection. The allowance is the
  ceiling on what any keeper can take.
- `withdraw_held` clears the escrow balance before it transfers, so a repeat
  call cannot withdraw the same amount twice.

### Status

- The contract has not been audited.
- It is for testnet use only. The README lists mainnet as pending an audit.
- The testnet deployment is behind `main`, so this document describes `main`,
  not the deployed build.
### 5. Archived persistent storage entry

**Symptom:**

```text
HostError: Error(Storage, MissingValue)   # or JarNotFound / Stats read as missing after long inactivity
```

When a persistent ledger entry's TTL expires without being bumped, Soroban archives it and subsequent reads treat the entry as absent until restored.

**Storage keys a jar can have:**

- `Jar(jar_id)` — jar owner and split configuration
- `MinTip(jar_id)` — optional per-jar minimum tip floor
- `Stats(jar_id)` — cumulative tip count and volume for the jar
- `Supporter(jar_id, supporter)` — per-supporter tip count and volume against the jar
- `Recurring(id)` — recurring tip schedule associated with a jar
- `Held(jar_id, recipient, token)` — escrowed payout held for a recipient who could not receive at tip time

**Fix:** Restore the archived persistent entry with the Stellar CLI before re-invoking the contract:

```bash
stellar contract restore \
  --id <CONTRACT_ID> \
  --source <ACCOUNT> \
  --network testnet \
  --key '{"vec":[{"symbol":"Jar"},{"symbol":"alice"}]}'
```

