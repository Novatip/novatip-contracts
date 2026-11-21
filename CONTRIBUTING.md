# Contributing to `novatip-contracts`

Thanks for your interest. The full contribution guide for every Novatip
repository lives in one place:

**[Novatip contributing guide](https://novatip-docs.pages.dev/contributing)**
([source](https://github.com/Novatip/novatip-docs/blob/main/CONTRIBUTING.md))

Read that first. This page only covers what is specific to this repository.

## Quick start

```bash
git clone https://github.com/YOUR_USERNAME/novatip-contracts
cd novatip-contracts
git remote add upstream https://github.com/Novatip/novatip-contracts
git checkout -b fix/short-description
```

Building to wasm needs the target the Stellar CLI uses:

```bash
rustup target add wasm32v1-none
make build
```

`make test` runs natively and does not need it.

## Before you push

Run everything CI checks with one command:

```bash
make ci
```

It runs the format check, clippy, tests, the release wasm build and the wasm
size check, in the same order with the same flags as CI. See
[RELEASING.md](./RELEASING.md) for how a contract release is versioned,
tagged and deployed.

## Opening a pull request

- One issue per pull request, opened against `main`
- Reference the issue so it closes on merge: `Closes #123`
- Use [conventional commits](https://novatip-docs.pages.dev/contributing):
  `feat:`, `fix:`, `docs:`, `test:`, `chore:`, `refactor:`

## Reporting security issues

Do not open a public issue. See
[SECURITY.md](https://github.com/Novatip/novatip-docs/blob/main/SECURITY.md).

## Local checks (match CI)

| Check | Command |
| ----- | ------- |
| Format / lint / test | `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` |
| MSRV pin sync (#133) | `make check-msrv` — `rust-toolchain.toml` `channel` and workspace `rust-version` must share major.minor. The crate inherits `rust-version.workspace = true`. |
| Dependency audit (#132) | `cargo deny check` (`cargo install cargo-deny`) — advisories, licenses, bans. See `deny.toml`. |
| Contract size (#131) | `make build && ./scripts/check-wasm-size.sh` — measures the `stellar contract build` artifact. |
| Public interface (#134) | `make interface` to regenerate `contracts/tip-splitter/interface.json`, then `./scripts/check-interface.sh`. **When it changes:** regenerate, add a CHANGELOG entry, and never renumber existing `Error` variants — append only. |

## Required CI checks & PR safety check

`main` only accepts a pull request when both required checks pass:

- **`build-test`**: runs formatting (`cargo fmt --all -- --check`), clippy (`cargo clippy --all-targets -- -D warnings`), unit tests (`cargo test`), wasm build & size check, MSRV sync, doc checks, and interface checks.
- **`PR safety check`**: flags risky changes for extra maintainer review — including changes under `.github/`, `build.rs`, `scripts/`, new binaries, hidden Unicode characters, very long lines, and commits credited to someone else. A flagged change is not necessarily wrong; it simply requires a maintainer to review and approve the exception before merge.
