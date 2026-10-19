# `tip_splitter` — contract interface

Receives a single USDC tip and splits it across one or more recipients by
basis-point shares, in one transaction. A recipient who cannot receive the token
no longer blocks the others: their share is escrowed by the contract for later
withdrawal.

## Concepts

- **Jar** — a creator's tip target, identified by a public slug (e.g. `@alice`).
  Holds an `owner` and a list of `Split`s. A slug (`jar_id`) is canonical: a
  leading `@` followed by 3–32 lower-case ASCII letters, digits, `_` or `-`
  — exactly the backend's `SLUG_REGEX`. Anything else (upper case, whitespace,
  control bytes, a missing `@`, or a length outside that range) is rejected with
  `InvalidJarId`, so lookalikes cannot exist next to a real creator's slug.
  The storage bound is also `MAX_JAR_ID_LEN` (64 bytes) — it doubles as a
  storage key, an event topic, and a public URL slug.
- **Split** — a recipient `Address` and its share in basis points (`bps`).
  Every split must have `bps >= 1`, all splits in a jar must sum to exactly
  `10_000` (= 100%), and no address may appear more than once.
- **USDC token** — the Stellar Asset Contract id is fixed at deploy time; every
  tip settles in that asset.
- **Message** — the free-text note a supporter attaches to a tip. Capped at
  `280` bytes (`MAX_MESSAGE_LEN`), must be valid UTF-8, and may not contain
  control characters other than newline; echoed into the `tip` event.
- **Sequence number** — a contract-wide `u64`, incremented once per successful
  tip, returned by `tip` and published in the `tip` event. Gives each tip an
  identifier of its own where the transaction hash does not.
- **Reference** — an optional 32-byte opaque id a tipper attaches with
  `tip_with_ref`, echoed in the `tip` event so the calling integration can
  match the on-chain event back to whatever it created off-chain. Never stored
  and never checked by the contract.

## Types

```rust
struct Split    { to: Address, bps: u32 }
struct Jar      { owner: Address, splits: Vec<Split>, locked: bool }
struct Limits   { bps_denom: u32, max_recipients: u32, max_message_len: u32, max_batch_tips: u32 }
struct Recurring {
    supporter: Address, jar_id: String, amount: i128,
    period_ledgers: u32, message: String, next_due_ledger: u32,
}
struct Split          { to: Address, bps: u32 }
struct Jar            { owner: Address, splits: Vec<Split> }
struct Limits         { bps_denom: u32, max_recipients: u32, max_message_len: u32, max_batch_tips: u32 }
struct TipRequest     { jar_id: String, amount: i128, message: String }
struct JarStats       { tip_count: u64, total: i128 }
struct SupporterStats { tip_count: u64, total: i128 }
```

`TipRequest` is one entry of a [`tip_many`](#tipping-many-jars-at-once-84) batch: the
slug to tip, the amount, and the message that rides along with that jar's `tip`
event. It is exactly the per-jar argument list of `tip`, so a batched entry
carries no new fields to reason about.

`JarStats` and `SupporterStats` are the on-chain totals behind
[reading tip totals](#reading-tip-totals). They share a shape but not a key:
one belongs to a jar, the other to a `(jar, supporter)` pair.

## Functions

| Function | Auth | Description |
|----------|------|-------------|
| `__constructor(admin, token)` | — | Deploy-time init. Stores the admin and USDC token address, and puts that token on the tip allowlist as its first entry. |
| `create_jar(owner, jar_id, splits)` | `owner`, plus the `registrar`'s co-signature over `(jar_id, owner)` while one is registered | Register a new jar. Fails if the slug is not a canonical `@`-prefixed `[a-z0-9_-]{3,32}` slug, already exists, or splits are invalid. Emits a `jar_crtd` event. See [the registrar gate](#the-jar-registrar-and-its-trust-model-178). |
| `update_splits(jar_id, splits)` | jar `owner`, plus every current recipient whose share **decreases or is removed** when the jar is locked | Replace a jar's splits. Subject to the same validation as `create_jar`. On a locked jar the owner alone cannot cut a collaborator — see [`lock_splits`](#locking-a-jars-splits). Emits a `splits` event. |
| `lock_splits(jar_id)` | jar `owner` | Lock a jar so `update_splits` needs each reduced/removed recipient's consent. Emits a `lock` event with data `true`. |
| `unlock_splits(jar_id)` | jar `owner` **and every current recipient** | Remove the lock. Consent mirrors a reduction: all recipients must sign, or the owner could disarm the lock and then cut shares. Emits a `lock` event with data `false`. |
| `is_splits_locked(jar_id) -> bool` | — | Whether the jar's splits are locked. Panics with `JarNotFound` if the slug is free. |
| `leave_jar(jar_id, recipient)` | `recipient` | Remove `recipient` from the jar's splits. Their share is added to the last remaining entry so the splits still sum to 10 000. Rejected with `SoleRecipient` (only entry) or `NotRecipient` (not in the jar). Emits a `splits` event. |
| `set_min_tip_amount(jar_id, min_amount)` | jar `owner` | Set or clear an optional minimum tip amount for a jar. `min_amount` must be strictly positive (`> 0`) or `None` to clear. Emits a `min_tip` event. |
| `transfer_jar_ownership(jar_id, new_owner)` | current jar `owner` | Hand control of a jar to `new_owner`. Splits are unchanged; the new owner does not need to authorize. Emits a `jar_xfer` event carrying both owners. |
| `rename_jar(jar_id, new_jar_id)` | jar `owner` | Move a jar, its stats and its minimum to a new slug. The old slug stops resolving immediately; `new_jar_id` must be free and pass the same length checks as `create_jar`. Emits a `jar_rnm` event carrying both ids. |
| `tip(from, jar_id, amount, message)` | `from` | Move `amount` USDC out of `from` and route it across the jar's recipients. A recipient who cannot receive the token has their share **escrowed in the contract** (see `get_held`/`withdraw_held`) instead of reverting the tip. Emits a `tip` event carrying the per-recipient breakdown. Rejects amounts below the jar's minimum with `BelowMinTipAmount`. |
| `get_held(recipient) -> i128` | — | How much the contract is holding for `recipient` because a past tip could not deliver their share. `0` when nothing is held. |
| `withdraw_held(recipient)` | `recipient` | Pay a recipient their held balance once they can receive again. Rejected with `NothingHeld` when nothing is owed. Emits a `withdraw` event. |
| `create_recurring(supporter, jar_id, amount, period_ledgers, message) -> u128` | `supporter` | Register a recurring-tip schedule. Validates the jar exists and the amount/message pass the same checks `tip` applies; `period_ledgers` must be non-zero. Assigns a fresh id, stores the entry with its first `next_due_ledger`, and emits a `recr_add` event. Returns the id. The schedule moves funds through `collect_recurring`: the supporter must have approved this contract as a token spender for at least `amount` per period. |
| `cancel_recurring(id)` | the entry's `supporter` | Remove a recurring-tip record. Emits a `recr_del` event. |
| `get_recurring(id) -> Recurring` | — | Read a stored recurring-tip entry. Panics with `RecurringNotFound` for an unknown id. |
| `collect_recurring(id)` | — (any caller, typically a keeper) | Collect **one due period** of a recurring tip (#175). Pays the schedule's `amount` from the supporter's approved allowance with `transfer_from` (this contract is the spender), split by the same arithmetic as `tip`, and publishes the identical `tip` event. `next_due_ledger` advances by exactly `period_ledgers`. Too early — `NotDue`; allowance below `amount` (exhausted or expired) — `InsufficientAllowance`; supporter balance below `amount` — `InsufficientBalance`; all raised before any transfer, with zero state change. See [collecting recurring tips](#collecting-a-due-recurring-tip-175). |
| `tip(from, jar_id, amount, message)` | `from` | Transfer `amount` of the **default token** from `from`, split across the jar's recipients. Emits a `tip` event carrying the per-recipient breakdown and the token paid in. Rejects amounts below the jar's minimum with `BelowMinTipAmount`. |
| `tip_in_token(from, jar_id, token, amount, message)` | `from` | Same split, paid in `token`, which must be on the allowlist — otherwise `TokenNotAllowed`, raised before any transfer. The jar minimum applies only when `token` is the default token. Emits the same `tip` event. |
| `allow_token(token)` | contract `admin` | Add `token` to the allowlist. Idempotent. Emits a `tk_allow` event carrying the admin who authorised it. |
| `disallow_token(token)` | contract `admin` | Remove `token` from the allowlist. Refuses the default token with `CannotDisallowDefault`. Emits a `tk_dis` event carrying the admin. |
| `set_tip_hook(hook)` | contract `admin` | Register or clear (`None`) the post-tip hook contract (#176). Emits a `tip_hook` event carrying the admin who made the change. See [the post-tip hook](#the-post-tip-hook-176). |
| `get_tip_hook() -> Option<Address>` | — | The registered hook, or `None` when tips settle without one. |
| `set_registrar(registrar)` | contract `admin` | Register or clear (`None`) the registrar whose co-signature `create_jar` requires (#178). Emits a `reg_set` event carrying the admin. See [the registrar gate](#the-jar-registrar-and-its-trust-model-178). |
| `get_registrar() -> Option<Address>` | — | The registrar `create_jar` currently requires, or `None` when jar creation is permissionless. |
| `set_fee_bps(fee_bps)` | contract `admin` | Set the protocol fee taken from every tip before the split, in basis points (#79). Defaults to `0`; above `500` (5%) is rejected with `FeeTooHigh`. The fee is paid to the admin. Emits a `fee_set` event. See [the protocol fee](#the-protocol-fee-79). |
| `get_fee_bps() -> u32` | — | The protocol fee currently taken from every tip, in basis points. `0` until the admin sets one. |
| `tip(from, jar_id, amount, message) -> u64` | `from` | Transfer `amount` USDC from `from`, split across the jar's recipients. Emits a `tip` event carrying the per-recipient breakdown and this tip's sequence number, and returns that number. Rejects amounts below the jar's minimum with `BelowMinTipAmount`. |
| `tip_with_ref(from, jar_id, amount, message, reference) -> u64` | `from` | Identical to `tip`, plus an optional 32-byte opaque `reference` echoed in the `tip` event. Same transfers, same validation, same counter — both entry points call one internal implementation. |
| `tip_many(from, tips) -> Vec<u64>` | `from` | Tip several jars in one transaction, each with its own amount and message. `from` authorises once for the whole batch, and every entry settles exactly as a single `tip` would — same validation, same minimum-tip check, same protocol fee, same self-transfer skip — and publishes its own `tip` event. The batch is bounded by `MAX_BATCH_TIPS` (`BatchTooLarge`); an empty batch is `BatchEmpty`. **All-or-nothing**: a failure in any jar reverts the whole call, leaving the earlier jars untouched. Returns one sequence number per entry, in order. See [tipping many jars at once](#tipping-many-jars-at-once-84). |
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
| `transfer_admin(new_admin)` | contract `admin` | Hand admin control to `new_admin`. Rejected with `AdminExists` when it is already the current admin. Emits an `adm_xfer` event carrying both addresses. |
| `pause()` | contract `admin` | Pause tipping: `tip`, `tip_with_ref` and `tip_in_token` reject with `ContractPaused`. Emits a `pause` event. |
| `resume()` | contract `admin` | Lift a pause. Emits a `resume` event. |
| `is_paused() -> bool` | — | Whether tipping is currently paused. Reads instance storage. |
| `upgrade(new_wasm_hash)` | contract `admin` | Replace the running contract Wasm while keeping the address and all stored state. Emits an `upgrade` event carrying the new hash. |
| `get_limits() -> Limits` | — | The bounds this contract enforces: `bps_denom`, `max_recipients`, `max_message_len`, `max_batch_tips`. Reads no storage. |
| `get_limits() -> Limits` | — | The bounds this contract enforces: `bps_denom`, `max_recipients`, `max_message_len`, `max_jar_id_len`. Reads no storage. |
| `version() -> u32` | — | Which interface shape this deployment speaks. Reads no storage. See [detecting the interface version](#detecting-the-interface-version). |
| `get_jar_stats(jar_id) -> JarStats` | — | A jar's on-chain tip count and total. Zeros for a jar nobody has tipped; `JarNotFound` if the slug is free. |
| `get_supporter_stats(jar_id, supporter) -> SupporterStats` | — | One address's tip count and total against one jar. Zeros for an address that has never tipped it; `JarNotFound` if the slug is free. |
| `is_token_allowed(token) -> bool` | — | Whether the admin has put `token` on the tip allowlist. The default token is always allowed. |
The **Errors** column lists the `tip_splitter` error codes a call can return,
**in the order the contract checks them** — only the first failure a call hits
is reported, so the order tells a client what it will actually see. An error is
written as raised only when every earlier check has passed.

| Function | Auth | Errors (in check order) | Description |
|----------|------|-------------------------|-------------|
| `__constructor(admin, token)` | — | — | Deploy-time init. Stores the admin and USDC token address. |
| `create_jar(owner, jar_id, splits)` | `owner` (+ registrar co-auth when set) | `InvalidJarId` → `JarExists` → `SplitsEmpty` → `TooManyRecipients` → `SplitOutOfRange` / `DuplicateRecipient` (per entry) → `SplitSumNot100Pct` → Soroban auth error (missing registrar co-signature, #178) | Register a new jar. Fails if the slug is not a canonical `@`-prefixed `[a-z0-9_-]{3,32}` slug, already exists, or splits are invalid. The registrar's co-signature is demanded only after validation passes, so a malformed request never burns one. Emits a `jar_crtd` event. |
| `update_splits(jar_id, splits)` | jar `owner` | `JarNotFound` → `SplitsEmpty` → `TooManyRecipients` → `SplitOutOfRange` / `DuplicateRecipient` (per entry) → `SplitSumNot100Pct` | Replace a jar's splits. Subject to the same validation as `create_jar`. Emits a `splits` event. |
| `set_min_tip_amount(jar_id, min_amount)` | jar `owner` | `JarNotFound` → `InvalidMinimumAmount` | Set or clear an optional minimum tip amount for a jar. `min_amount` must be strictly positive (`> 0`) or `None` to clear. Emits a `min_tip` event. |
| `transfer_jar_ownership(jar_id, new_owner)` | current jar `owner` | `JarNotFound` → `InvalidRecipient` (owner is the contract or its token) | Hand control of a jar to `new_owner`. Splits are unchanged; the new owner does not need to authorize. Emits a `jar_xfer` event carrying both owners. |
| `rename_jar(jar_id, new_jar_id)` | jar `owner` | `InvalidJarId` (new id not a canonical `@`-prefixed `[a-z0-9_-]{3,32}` slug) → `JarNotFound` → `JarIdInUse` | Move a jar, its stats and its minimum to `new_jar_id`. The old id stops resolving once the move is complete. Emits a `jar_rnm` event carrying both ids. |
| `tip(from, jar_id, amount, message)` | `from` | `ContractPaused` → `NotInitialized` → `AmountZero` (amount = 0) / `AmountNegative` (amount < 0) → `MessageTooLong` → `JarNotFound` → `BelowMinTipAmount` → `InsufficientBalance` → `SplitAmountTooSmall` (a share truncates to zero) / `AmountOverflow` (`amount * bps` overflows `i128`) | Transfer `amount` USDC from `from`, split across the jar's recipients. Emits a `tip` event carrying the per-recipient breakdown. Rejects amounts below the jar's minimum with `BelowMinTipAmount` and amounts that cannot pay every recipient with `SplitAmountTooSmall`; see [the smallest amount a jar accepts](#the-smallest-amount-a-jar-accepts-86). A failing token `transfer` reverts the whole call with the **token contract's own error**, not one of this contract's codes (e.g. an insufficient balance). |
| `get_jar(jar_id) -> Jar` | — | `JarNotFound` | Read a jar's configuration. Panics with `JarNotFound` if the slug is free. |
| `get_jar_owner(jar_id) -> Address` | — | `JarNotFound` | Read just a jar's owner. Panics with `JarNotFound` if the slug is free. |
| `get_split_count(jar_id) -> u32` | — | `JarNotFound` | How many recipients a jar pays. Panics with `JarNotFound` if the slug is free. |
| `preview_split(jar_id, amount) -> Vec<i128>` | — | `AmountZero` / `AmountNegative` → `JarNotFound` → `BelowMinTipAmount` → `SplitAmountTooSmall` / `AmountOverflow` | What each recipient would receive from a tip of `amount`, in split order. Rejects amounts below the jar's minimum with `BelowMinTipAmount` as well as the amounts `tip` rejects, in the same order; panics with `JarNotFound` if the slug is free. |
| `preview_tip(jar_id, from, amount) -> Vec<i128>` | — | `AmountZero` (amount = 0) / `AmountNegative` (amount < 0) → `JarNotFound` → `BelowMinTipAmount` → `SplitAmountTooSmall` / `AmountOverflow` | The sender-aware preview (#177): exactly the amounts `tip` will transfer for `from`, helper-shared with the payout. |
| `preview_tip(jar_id, from, amount) -> Vec<i128>` | — | `InvalidAmount` (amount ≤ 0) → `JarNotFound` → `BelowMinTipAmount` → `InvalidAmount` (a share truncates to zero, or `amount * bps` overflows) | The sender-aware preview (#177): exactly the amounts `tip` will transfer for `from`, helper-shared with the payout. |
| `tip_many(from, tips) -> Vec<u64>` | `from` | Soroban auth error (missing `from` signature) → `ContractPaused` → `BatchEmpty` → `BatchTooLarge` → `NotInitialized`, then per entry the same sequence as `tip`: `AmountZero` / `AmountNegative` → `MessageTooLong` → `JarNotFound` → `BelowMinTipAmount` → `SplitAmountTooSmall` / `AmountOverflow` → `InsufficientBalance` → the token contract's own error | Tip several jars in one transaction (#84). The batch-size and pause checks run first, before any jar is looked up; each entry is then settled exactly as `tip` would settle it, and a failure in any entry reverts the whole call. |
| `collect_recurring(id)` | — | `RecurringNotFound` → `NotDue` → `JarNotFound` → `NotInitialized` → `InsufficientAllowance` → `InsufficientBalance` | Collect one due period of a recurring schedule through the supporter's token allowance (#175). Publishes the same `tip` event and fires the post-tip hook. |
| `jar_exists(jar_id) -> bool` | — | — | Whether the slug is already registered. Reads one key and returns a plain `bool`; never panics on a free slug. |
| `is_recipient(jar_id, address) -> bool` | — | `JarNotFound` | Whether `address` appears in the jar's splits. Panics with `JarNotFound` if the slug is free. |
| `get_min_tip_amount(jar_id) -> Option<i128>` | — | `JarNotFound` | Read a jar's optional minimum tip amount. Returns `None` if no minimum is set. Panics with `JarNotFound` if the slug is free. |
| `get_min_accepted_tip(jar_id) -> i128` | — | `JarNotFound` | The smallest tip this jar accepts: the larger of the truncation floor and the configured minimum. See [the smallest amount a jar accepts](#the-smallest-amount-a-jar-accepts-86). |
| `get_token() -> Address` | — | `NotInitialized` | The USDC token address tips settle in. Reads instance storage; `NotInitialized` if the deploy-time token is absent. |
| `get_admin() -> Address` | — | `NotInitialized` | The contract admin recorded at deploy time. Reads instance storage; `NotInitialized` if the deploy-time admin is absent. |
| `get_limits() -> Limits` | — | — | The bounds this contract enforces: `bps_denom`, `max_recipients`, `max_message_len`, `max_batch_tips`. Reads no storage. |
| `get_limits() -> Limits` | — | — | The bounds this contract enforces: `bps_denom`, `max_recipients`, `max_message_len`, `max_jar_id_len`. Reads no storage. |

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

`get_limits` returns all four:

```rust
Limits {
    bps_denom: 10_000,       // BPS_DENOM — the denominator every `bps` is a share of
    max_recipients: 20,      // MAX_RECIPIENTS — most recipients in one jar
    max_message_len: 280,    // MAX_MESSAGE_LEN — longest tip message, in UTF-8 bytes
    max_batch_tips: 10,      // MAX_BATCH_TIPS — most jars one `tip_many` call may tip
    max_jar_id_len: 33,      // MAX_JAR_SLUG_LEN + 1 — longest canonical jar id ("@" + 32)
}
```

All four are compile-time constants, so the view reads no storage and costs
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
contract_constants` in `test.rs`), so the view and the enforcement cannot drift
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
of his 30 — the dust goes to the **last** recipient, so a UI that rounds evenly
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
`net * bps >= 10_000`, where `net` is what is left after the
[protocol fee](#the-protocol-fee-79). The smallest net that pays everyone is

```
net_min = max over every recipient of ceil(10_000 / bps)
```

Every entry is checked, including the last one — even though the last entry
absorbs the rounding remainder, the guard runs before any share is computed.

**2. The fee gross-up.** The fee comes off the top, so the signed amount must
be large enough that `net_min` survives it. Since `net = amount -
floor(amount * fee_bps / 10_000)`, the first gross amount that leaves a net of
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
  normally and the tip still succeeds. `withdraw_held` releases the escrow once
  the recipient can receive again.
- `preview_split(jar_id, amount)` runs this same calculation as a view, so a
  client can show the exact per-recipient amounts before the supporter signs.
- The amounts actually transferred are published in the `tip` event's
  `breakdown`, so no consumer has to reproduce this arithmetic after the fact
  either.

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
| `transfer_jar_ownership(jar_id, new_owner)` | the jar's current `owner` | The contract call only. `new_owner` signs nothing. |
| `set_min_tip_amount(jar_id, min_amount)` | the jar's current `owner` | The contract call only. |

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
- **Data:** `SplitsUpdatedEvent { count: u32 }`

Lets an indexer that has cached a jar's splits know they went stale, without
having to re-poll every jar on a schedule. The indexer refetches the jar via
`get_jar` when it sees this event.

### `jar_xfer` — published on every successful `transfer_jar_ownership`

- **Topics:** `(symbol "jar_xfer", jar_id: String)`
- **Data:** `JarTransferredEvent { previous_owner: Address, new_owner: Address }`

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

`prev_owner` is the address that authorized the call — only the current owner
may transfer a jar — and is always different from `new_owner` in practice,
though the contract does not reject a transfer to the existing owner. Splits
are unchanged by a transfer, so no `splits` event accompanies this one.

> **Consumer impact.** Event data is a named map. Decoders should read fields
> by name and ignore unknown fields added in future versions.

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

### `lock` — published on `lock_splits` and `unlock_splits`

- **Topics:** `(symbol "lock", jar_id: String)`
- **Data:** `locked: bool` (`true` when locked, `false` when unlocked)

Lets an indexer cache whether a jar protects its collaborators against a
unilateral owner edit (#173).

### `withdraw` — published on every successful `withdraw_held`

- **Topics:** `(symbol "withdraw", recipient: Address)`
- **Data:** `amount: i128`

Records the moment an escrowed share (#171) is released to its recipient. The
recipient is a topic (not just data) so an indexer can subscribe to one
address's withdrawals.

### `recr_add` / `recr_del` — recurring-tip registration and cancellation

- **`recr_add` Topics:** `(symbol "recr_add", supporter: Address)`, **Data:** `id: u128`
- **`recr_del` Topics:** `(symbol "recr_del", supporter: Address)`, **Data:** `id: u128`

Publishes the assigned schedule id (and `recr_del` the id it removed).
A schedule becomes a payment rail through `collect_recurring` (#175): the
supporter approves this contract as a token spender, and any keeper collects
due periods one at a time. Each collection publishes the ordinary `tip` event,
so indexers need no separate decoder for recurring payments.

> The `splits` event is also emitted by `leave_jar` (#172), not only by
> `update_splits`, so an indexer refreshes its cached splits after a recipient
> removes themselves.

### `tip_hook` — published on every `set_tip_hook`

- **Topics:** `(symbol "tip_hook", admin: Address)`
- **Data:** `hook: Option<Address>` (`None` when the hook was cleared)

Records who changed the post-tip hook configuration and to what, so the
setting's history is attributable in the event log (#176).

### `reg_set` — published on every `set_registrar`

- **Topics:** `(symbol "reg_set", admin: Address)`
- **Data:** `registrar: Option<Address>` (`None` when cleared)

Records every installation or removal of the `create_jar` registrar (#178),
including the moment the squatting gate goes up or down.

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
| Bytes written | 884 | 1288 | **+404 bytes** |
| Persistent rent bumps | 3 | 5 | **+2** |

The write columns were re-measured against the merged payout path — the
"without totals" baseline is now 4 entries, not 3, because the tip sequence
counter writes the contract instance each tip and the escrow fallback (#171)
enlarged the per-tip footprint. The **differential the #167 PR documented still
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

`set_fee_bps` emits a `fee_set` event: topics `(symbol "fee_set", admin)`, data
the new `fee_bps: u32`.

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

This build ships `version() == 3`. Version 3 adds `set_fee_bps` /
`get_fee_bps`, the `fee_set` event, the `FeeTooHigh` error (35), and the `fee`
field in the `tip` event (#79); it also changes what a tipper who is a
recipient pays (#78), which `preview_tip` already reports.

Version 2 Version 1 covered the escrow/`held`
breakdown, the token allowlist, the totals views, and the sequence-numbered
`tip` event. This PR adds four functions and one view beyond that point —
`collect_recurring`, `preview_tip`, `set_tip_hook`/`get_tip_hook`,
`set_registrar`/`get_registrar` — plus the `tip_hook` and `reg_set` events,
the single canonical seven-element `tip` event data, and the repair of the
14/15/16 error-code collision by renumbering the affected codes to 19–22.
Every one of those is a function, event-shape, or error-code change, so the
constant moves up under the rule above.

**How a client should use it.** Treat an unrecognised number as "newer than I
was built for", not as an error. Every field a lower version documented still
exists at a higher one — changes are appended — so the safe response is to read
what you know and ignore the rest. Refusing to connect on a version you have
not seen would turn a future additive release into an outage.

`version()` is per-contract feature detection. It complements the per-event
schema version proposed in #83 rather than replacing it: that answers "what
shape is this record", this answers "what build is this deployment".

## Jar discovery — design decision

The previous contract exposed a `get_jar_ids() -> Vec<String>` view backed by
an instance-storage vector that grew by one entry on every `create_jar` call.
This had two problems:
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

## Jar discovery — design note

Indexing needs a way to enumerate every registered jar. An earlier design kept
that list on-chain as a `get_jar_ids() -> Vec<String>` view backed by an
instance-storage vector that grew by one entry on every `create_jar`. It had
two problems:

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

It lists each of the five event types (`jar_crtd`, `splits`, `jar_xfer`,
`min_tip`, `tip`), one line per event with ledger, name, jar id and decoded
payload, defaulting to a recent ledger window; a `START_LEDGER` outside the
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
