# `tip_splitter` — contract interface

Receives a single USDC tip and splits it across one or more recipients by
basis-point shares, atomically, in one transaction.

## Concepts

- **Jar** — a creator's tip target, identified by a public slug (e.g. `@alice`).
  Holds an `owner` and a list of `Split`s. The slug (`jar_id`) must be
  non-empty and at most `64` bytes (`MAX_JAR_ID_LEN`) — it doubles as a
  storage key, an event topic, and a public URL slug.
- **Split** — a recipient `Address` and its share in basis points (`bps`).
  Every split must have `bps >= 1`, all splits in a jar must sum to exactly
  `10_000` (= 100%), and no address may appear more than once.
- **USDC token** — the Stellar Asset Contract id is fixed at deploy time; every
  tip settles in that asset.
- **Message** — the free-text note a supporter attaches to a tip. Capped at
  `280` bytes (`MAX_MESSAGE_LEN`) and echoed into the `tip` event.

## Types

```rust
struct Split { to: Address, bps: u32 }
struct Jar   { owner: Address, splits: Vec<Split> }
```

## Functions

| Function | Auth | Description |
|----------|------|-------------|
| `__constructor(admin, token)` | — | Deploy-time init. Stores the admin and USDC token address. |
| `create_jar(owner, jar_id, splits)` | `owner` | Register a new jar. Fails if the slug is empty, over `MAX_JAR_ID_LEN` bytes, already exists, or splits are invalid. Emits a `jar_crtd` event. |
| `update_splits(jar_id, splits)` | jar `owner` | Replace a jar's splits. Subject to the same validation as `create_jar`. Emits a `splits` event. |
| `transfer_jar_ownership(jar_id, new_owner)` | current jar `owner` | Hand control of a jar to `new_owner`. Splits are unchanged; the new owner does not need to authorize. Emits a `jar_xfer` event. |
| `tip(from, jar_id, amount, message)` | `from` | Transfer `amount` USDC from `from`, split across the jar's recipients. |
| `get_jar(jar_id) -> Jar` | — | Read a jar's configuration. Panics with `JarNotFound` if the slug is free. |
| `jar_exists(jar_id) -> bool` | — | Whether the slug is already registered. |
| `get_token() -> Address` | — | The USDC token address tips settle in. |
| `get_admin() -> Address` | — | The contract admin recorded at deploy time. |

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

`create_jar` rejects a `jar_id` that is empty or longer than `64` bytes
(`MAX_JAR_ID_LEN`) — `InvalidJarId`. The id is used as a storage key, an event
topic, and a public URL slug, so unbounded input would cost unnecessary rent
and could produce jars no frontend can address. The bound is inclusive: a
`jar_id` of exactly 64 bytes is accepted.

### Validation rules

`create_jar` and `update_splits` share one validator, so both enforce all of:

- The list must be non-empty — `InvalidSplits`.
- At most 20 entries (`MAX_RECIPIENTS`) — `TooManyRecipients`.
- **No entry may have `bps == 0`** — `InvalidSplits`.
- **No address may appear twice** — `DuplicateRecipient`.
- **No entry may have `bps > 10_000`** — `InvalidSplits`.
- The `bps` values must sum to exactly `10_000` — `InvalidSplits`.

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
with `InvalidSplits` rather than trapping.

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
- The whole tip reverts if any single transfer fails — tips are all-or-nothing.

## Errors

| Code | Name | Cause |
|------|------|-------|
| 1 | `NotInitialized` | Token address missing (should never happen post-deploy). |
| 2 | `JarExists` | Slug already registered. |
| 3 | `JarNotFound` | Slug not registered. |
| 4 | `InvalidSplits` | **No longer raised.** Split validation now reports the specific failure as code 10, 11 or 12. The variant is retained so existing codes keep their values. |
| 5 | `InvalidAmount` | Tip amount ≤ 0, or so large that `amount * bps` overflows `i128` before the division. |
| 6 | `TooManyRecipients` | More than 20 recipients. |
| 7 | `DuplicateRecipient` | The same address appears more than once in the splits. |
| 8 | `MessageTooLong` | Tip message exceeds 280 bytes. |
| 9 | `InvalidJarId` | `jar_id` is empty or exceeds 64 bytes (`MAX_JAR_ID_LEN`). |
| 10 | `SplitsEmpty` | The splits list is empty. |
| 11 | `SplitOutOfRange` | An entry has `bps == 0` or `bps > 10_000`. Checked per entry, before the sum. |
| 12 | `SplitSumNot100Pct` | The shares sum to something other than exactly 10 000 bps, including a sum that would overflow `u32`. |

Error codes are part of the public interface: `@novatip/sdk` and the frontend
both map these numbers to user-facing messages. New variants are **appended**
and existing values are never renumbered, which is why code 4 stays in place
rather than being removed.

## Events

### `jar_crtd` — published on every successful `create_jar`

- **Topics:** `(symbol "jar_crtd", jar_id: String)`
- **Data:** `owner: Address`

Indexers must subscribe to this event to build and maintain the full list of
registered jars. There is no on-chain `get_jar_ids` function — event scanning
is the canonical discovery mechanism. This keeps `create_jar` cost constant
(O(1) storage writes) regardless of how many jars have been created.

Enumeration and existence are separate concerns: `jar_exists` answers "is this
one slug taken?" straight from storage, so a client never has to scan the event
log or an indexer's jar list just to validate a name.

### `splits` — published on every successful `update_splits`

- **Topics:** `(symbol "splits", jar_id: String)`
- **Data:** `split_count: u32`

Lets an indexer that has cached a jar's splits know they went stale, without
having to re-poll every jar on a schedule. The indexer refetches the jar via
`get_jar` when it sees this event.

### `jar_xfer` — published on every successful `transfer_jar_ownership`

- **Topics:** `(symbol "jar_xfer", jar_id: String)`
- **Data:** `new_owner: Address`

Lets an indexer update who controls a jar without re-polling `get_jar` for
every jar on a schedule.

### `tip` — published on every successful tip

- **Topics:** `(symbol "tip", jar_id: String)`
- **Data:** `(from: Address, amount: i128, message: String)`

The backend indexer subscribes to this event to update balances, leaderboards,
and notifications. `message` is at most 280 bytes, so the payload size is
bounded and a `varchar(280)` column is enough to store it.

## Jar discovery — design decision

The previous contract exposed a `get_jar_ids() -> Vec<String>` view backed by
an instance-storage vector that grew by one entry on every `create_jar` call.
This had two problems:

1. **Unbounded growth.** The vector had no removal path and no size cap, so it
   grew permanently with every jar ever created.
2. **Escalating cost.** Instance storage is read and written in full on every
   `create_jar`. As the vector grew, each new jar creation cost more than the
   last, and the entry would eventually approach the ledger entry size limit,
   causing `create_jar` to fail for all callers.

**Decision:** drop the on-chain list entirely and move discovery to the event
log. `create_jar` now emits a `jar_crtd` event carrying the `jar_id` and
`owner`. Indexers reconstruct the full jar list by scanning those events from
ledger 0 (or from their last checkpoint). This is the standard pattern for
Soroban contracts where enumeration is needed but unbounded on-chain state is
not acceptable.

### Migration impact for the backend indexer

- **`get_jar_ids` is removed.** Any indexer code that calls this function must
  be updated.
- **Backfill required.** On first deploy of this version, the indexer must
  replay all historical `jar_crtd` events from the contract's creation ledger
  to reconstruct the jar list. If the previous contract was deployed with the
  old version, existing jars will not have emitted `jar_crtd` events. Those
  jars must be seeded into the indexer's database from the old `get_jar_ids`
  response before upgrading, or discovered by replaying `create_jar`
  invocation history from Horizon.
- **Going forward,** every new jar emits `jar_crtd`, so no polling or
  `get_jar_ids` calls are needed.

## Deploy & bootstrap

```bash
set -a; source .env; set +a
./scripts/deploy.sh        # deploys, writes .contract-id
./scripts/create-jar.sh    # registers an example jar
```

See [`.env.example`](../.env.example) for required variables.
