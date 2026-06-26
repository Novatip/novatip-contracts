# Changelog

All notable changes to `novatip-contracts` are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/), and this
project adheres to [Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-06-26

### Added
- `tip_splitter` contract:
  - `create_jar` / `update_splits` to register and re-split tip jars.
  - `tip` for atomic, basis-point USDC splitting with rounding dust sent to the
    last recipient, emitting a `tip` event.
  - `get_jar` / `get_token` read accessors.
  - Typed errors and split validation (sum = 10_000, ≤ 20 recipients).
- Full test suite: splits, rounding, validation, error paths, `update_splits`.
- CI (fmt, clippy, test, wasm build) and a Makefile.
- Deploy and jar-bootstrap scripts.
- Contract interface documentation.
