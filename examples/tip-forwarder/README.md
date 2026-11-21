# `tip-forwarder` — tipping a jar from another contract

A minimal example of calling `tip_splitter` as a caller that is itself a
contract — a marketplace routing part of a sale to a creator's jar — and the
authorization catch that comes with it.

## The catch

`tip(env, from, jar_id, amount, message)` calls `from.require_auth()`. When a
contract calls `tip` with `from` set to its own address, that check is
satisfied automatically. But `tip` then calls the token's
`transfer(from, to, share)` once per recipient, and **those transfers are
invoked by `tip_splitter`, not by the caller** — so they are not covered. The
calling contract must pre-authorize each transfer with
`env.authorize_as_current_contract`, listing every recipient and the exact
amount, or the tip fails with an auth error.

The amounts to authorize are what `preview_tip(jar_id, forwarder, amount)`
returns — the sender-aware preview, which reports `0` for the forwarder's own
share because `tip` makes no transfer to it and that share stays put — plus,
once the admin has set a protocol fee (#79), **one more transfer: the fee,
paid to the splitter's admin**. Every preview reports post-fee shares, so the
fee is `amount - sum(preview_split(jar_id, amount))`.

## How the example does it

`forward_tip` (in `src/lib.rs`):

1. reads `preview_tip(jar_id, myself, amount)` for the shares and `get_jar`
   for the recipient order, skipping zero entries;
2. adds the fee transfer to `get_admin()` when `amount` exceeds the sum of
   `preview_split` (`fee_transfer`);
3. builds one `InvokerContractAuthEntry::Contract` per transfer
   (`token.transfer(from=forwarder, to, amount)`) and passes them to
   `env.authorize_as_current_contract` — in the same invocation, immediately
   before the cross-contract call;
4. calls `client.tip(&myself, &jar_id, &amount, &message)`.

`forward_tip_naive` skips step 2. It exists only so the tests can show the
failure mode; production callers should use `forward_tip`.

## Held-share limitation (#268)

When a recipient cannot receive the token (for example, their trustline was
removed or deauthorized), `tip_splitter` falls back to
`transfer(from=forwarder, to=tip_splitter, share)` so the share is escrowed in
the splitter under `Held(recipient)` (#171). Because `forward_tip` only
pre-authorizes `transfer(from=forwarder, to=recipient, share)` for each
recipient reported by `preview_tip`, that fallback transfer to `tip_splitter`
is **not** pre-authorized, and `forward_tip` reverts with an authorization
error (moving no funds).

Pre-authorizing both transfers per recipient is not a general fix either:
Soroban consumes authorized `InvokerContractAuthEntry` trees as sub-invocations
run, and when the direct transfer to `recipient` fails inside `try_transfer`
after checking `from.require_auth()`, or when multiple recipients fall back to
escrow, the static auth list predicted from `preview_tip` no longer matches the
transfers actually executed. Contract callers should ensure all jar recipients
can receive the settlement token before forwarding a tip (for example, with
`scripts/trustline.sh --check`).

## Tests

```bash
cargo test -p tip-forwarder
```

The tests register the real `TipSplitter` from `contracts/tip-splitter` and a
test SAC in the same env and run under strict `mock_auths` — only the
forwarder owner's signature is mocked, so the token transfers succeeding is
proof the runtime authorization works. `forward_tip_naive` is asserted to
*fail* once a protocol fee is set, and nothing moves when it does; a forwarder
that is itself a recipient keeps its own share.

The `tip_splitter` interface is declared locally with `#[contractclient]`
rather than imported with `contractimport!` — `cargo test` needs no built
wasm, matching CI, which runs tests before it builds wasm.
