.PHONY: build optimize test fmt lint clean deploy check-docs
.PHONY: build optimize test fmt lint clean deploy ci lint-scripts
.PHONY: build optimize test fmt lint clean deploy check-docs check-msrv interface deny
.PHONY: build optimize test fmt lint clean deploy ci

# Build all contracts to wasm (requires the Stellar CLI).
# Embeds source repo and commit in the wasm metadata (#127).
build:
	@./scripts/check-stellar-cli.sh
	@REPO="github:Novatip/novatip-contracts"; \
	COMMIT=$$(git rev-parse --short HEAD 2>/dev/null || echo "unknown"); \
	DIRTY=""; \
	if [ -n "$$(git status --porcelain 2>/dev/null)" ]; then DIRTY="-dirty"; fi; \
	stellar contract build --optimize \
		--meta source_repo=$$REPO \
		--meta source_commit=$$COMMIT$$DIRTY

# `stellar contract build --optimize` already produces the optimized artifact,
# so this target just builds and reports its size (#121). Kept as an alias for
# existing workflows.
optimize: build
	@WASM=$$(./scripts/find-wasm.sh || true); \
	if [ -z "$$WASM" ] || [ ! -f "$$WASM" ]; then \
		echo "error: no built tip_splitter.wasm found under target/. Run 'make build' first." >&2; \
		exit 1; \
	fi; \
	SIZE=$$(wc -c < "$$WASM"); \
	echo "==> $$WASM: $$SIZE bytes (optimized by stellar contract build)"

# Run the test suite.
test:
	cargo test --locked

# Auto-format all code.
fmt:
	cargo fmt --all

# Lint and fail on any warning (matches CI).
lint:
	cargo clippy --locked --all-targets -- -D warnings

# Lint shell scripts with shellcheck (#125).
lint-scripts:
	shellcheck scripts/*.sh scripts/lib/*.sh

# Remove build artifacts.
clean:
	cargo clean

# Deploy to the configured network (see scripts/deploy.sh).
deploy:
	./scripts/deploy.sh

# Build the contract and fail if docs/CONTRACT.md's function or error tables
# no longer match the built interface (requires the Stellar CLI and jq).
check-docs:
	./scripts/check-doc-length.sh
	./scripts/check-docs.sh
# Run everything CI checks, in the same order with the same flags,
# including the wasm build and size check.
ci:
	cargo fmt --all -- --check
	cargo clippy --locked --all-targets -- -D warnings
	cargo test --locked
	cargo build --locked --target wasm32-unknown-unknown --release
	stellar contract build
	./scripts/check-wasm-size.sh

# Regenerate the committed public interface snapshot (#134).
interface:
	./scripts/update-interface.sh

# Fail when rust-toolchain.toml and workspace rust-version disagree (#133).
check-msrv:
	./scripts/check-msrv.sh

# Dependency advisory / license / ban audit (#132).
deny:
	cargo deny check
