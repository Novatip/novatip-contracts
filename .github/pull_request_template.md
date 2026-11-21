## Linked issue

- Closes #

## What changed

<!-- One paragraph a reviewer can act on: what the PR does and why. -->

## Checks

- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --all-targets -- -D warnings` passes
- [ ] `cargo test` passes from a clean checkout (before any wasm build)
- [ ] The CI wasm size check still measures `tip_splitter.wasm` (not an example or test artifact)
- [ ] Reviewed [required CI checks & PR safety check](../CONTRIBUTING.md#required-ci-checks--pr-safety-check) if touching `.github/`, `scripts/`, or `build.rs`

## Interface changes

Complete every row that applies; tick nothing you did not touch.

- [ ] No public interface changed — the rows below do not apply.
- [ ] A public function was added, changed, or removed, and `docs/CONTRACT.md` reflects it.
- [ ] A new error code was **appended** (never renumbered or reused) and is documented in the error table.
- [ ] An event's topics or payload changed, and this PR carries a **Consumer impact** note saying exactly what a decoder must change (`@novatip/sdk`, the backend indexer, `docs/CONTRACT.md`).
- [ ] `CHANGELOG.md` has an entry under [Unreleased] describing the change from the reader's point of view.
- [ ] A bound or constant changed that clients read via `get_limits`, and the literal-value test was updated with it.

## Consumer impact

<!-- Required for any event or signature change: who reads this on-chain data,
     what breaks if they do not update, and what keeps working unchanged.
     Write "None" if nothing on the wire changed. -->
