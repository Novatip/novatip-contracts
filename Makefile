.PHONY: build test fmt lint clean deploy

# Build all contracts to optimized wasm (requires the Stellar CLI).
build:
	stellar contract build

# Run the test suite.
test:
	cargo test

# Auto-format all code.
fmt:
	cargo fmt --all

# Lint and fail on any warning (matches CI).
lint:
	cargo clippy --all-targets -- -D warnings

# Remove build artifacts.
clean:
	cargo clean

# Deploy to the configured network (see scripts/deploy.sh).
deploy:
	./scripts/deploy.sh
