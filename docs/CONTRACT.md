# `tip_splitter` — contract interface

Receives a single USDC tip and splits it across one or more recipients by
basis-point shares, atomically, in one transaction.

## Concepts

- **Jar** — a creator's tip target, identified by a public slug (e.g. `@alice`).
  Holds an `owner` and a list of `Split`s.
- **Split** — a recipient `Address` and its share in basis points (`bps`).
  All splits in a jar must sum to exactly `10_000` (= 100%), and no address may
  appear more than once.
- **USDC token** — the Stellar Asset Contract id is fixed at deploy time; every
  tip settles in that asset.

## Types

```rust
struct Split { to: Address, bps: u32 }
struct Jar   { owner: Address, splits: Vec<Split> }
```

## Functions

| Function | Auth | Description |
|----------|------|-------------|
| `__constructor(admin, token)` | — | Deploy-time init. Stores the admin and USDC token address. |
| `create_jar(owner, jar_id, splits)` | `owner` | Register a new jar. Fails if the slug exists or splits are invalid. Emits a `jar_crtd` event. |
| `update_splits(jar_id, splits)` | jar `owner` | Replace a jar's splits. Subject to the same validation as `create_jar`. |
| `tip(from, jar_id, amount, message)` | `from` | Transfer `amount` USDC from `from`, split across the jar's recipients. |
| `get_jar(jar_id) -> Jar` | — | Read a jar's configuration. |
| `get_token() -> Address` | — | The USDC token address tips settle in. |

### Validation rules

`create_jar` and `update_splits` share one validator, so both enforce all of:

- The list must be non-empty — `InvalidSplits`.
- At most 20 entries (`MAX_RECIPIENTS`) — `TooManyRecipients`.
- **No address may appear twice** — `DuplicateRecipient`.
- The `bps` values must sum to exactly `10_000` — `InvalidSplits`.

Duplicates are rejected rather than merged. A repeated address is not a
loss-of-funds bug — the shares still total 100% — but it makes `tip` issue
several separate transfers to one destination in a single call, wasting fees,
and leaves an on-chain record that per-collaborator accounting has to
de-duplicate after the fact. Clients that want to let a user enter the same
collaborator twice should sum the shares before submitting.

The check is a pairwise comparison over the vector, so position doesn't matter:
`[a, b, a]` is rejected just as `[a, a, b]` is.

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
| 4 | `InvalidSplits` | Empty list, or bps don't sum to 10_000. |
| 5 | `InvalidAmount` | Tip amount ≤ 0. |
| 6 | `TooManyRecipients` | More than 20 recipients. |
| 7 | `DuplicateRecipient` | The same address appears more than once in the splits. |

## Events

### `jar_crtd` — published on every successful `create_jar`

- **Topics:** `(symbol "jar_crtd", jar_id: String)`
- **Data:** `owner: Address`

Indexers must subscribe to this event to build and maintain the full list of
registered jars. There is no on-chain `get_jar_ids` function — event scanning
is the canonical discovery mechanism. This keeps `create_jar` cost constant
(O(1) storage writes) regardless of how many jars have been created.

### `tip` — published on every successful tip

- **Topics:** `(symbol "tip", jar_id: String)`
- **Data:** `(from: Address, amount: i128, message: String)`

The backend indexer subscribes to this event to update balances, leaderboards,
and notifications.

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
