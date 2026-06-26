# `tip_splitter` — contract interface

Receives a single USDC tip and splits it across one or more recipients by
basis-point shares, atomically, in one transaction.

## Concepts

- **Jar** — a creator's tip target, identified by a public slug (e.g. `@alice`).
  Holds an `owner` and a list of `Split`s.
- **Split** — a recipient `Address` and its share in basis points (`bps`).
  All splits in a jar must sum to exactly `10_000` (= 100%).
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
| `create_jar(owner, jar_id, splits)` | `owner` | Register a new jar. Fails if the slug exists or splits are invalid. |
| `update_splits(jar_id, splits)` | jar `owner` | Replace a jar's splits. |
| `tip(from, jar_id, amount, message)` | `from` | Transfer `amount` USDC from `from`, split across the jar's recipients. |
| `get_jar(jar_id) -> Jar` | — | Read a jar's configuration. |
| `get_token() -> Address` | — | The USDC token address tips settle in. |

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

## Events

`tip` — published on every successful tip:

- **Topics:** `(symbol "tip", jar_id: String)`
- **Data:** `(from: Address, amount: i128, message: String)`

The backend indexer subscribes to this event to update balances, leaderboards,
and notifications.

## Deploy & bootstrap

```bash
set -a; source .env; set +a
./scripts/deploy.sh        # deploys, writes .contract-id
./scripts/create-jar.sh    # registers an example jar
```

See [`.env.example`](../.env.example) for required variables.
