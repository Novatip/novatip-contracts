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

CI runs exactly these, so run them locally first:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Opening a pull request

- One issue per pull request, opened against `main`
- Reference the issue so it closes on merge: `Closes #123`
- Use [conventional commits](https://novatip-docs.pages.dev/contributing):
  `feat:`, `fix:`, `docs:`, `test:`, `chore:`, `refactor:`

## Reporting security issues

Do not open a public issue. See
[SECURITY.md](https://github.com/Novatip/novatip-docs/blob/main/SECURITY.md).
