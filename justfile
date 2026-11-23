set windows-shell := ["pwsh", "-NoLogo", "-Command"]

# cargo build --workspace
build:
    cargo build --workspace

# nextest for unit + integration tests; doctests need plain cargo test
test:
    cargo nextest run --workspace
    cargo test --workspace --doc

# fmt check + clippy (warnings are errors) + dependency licenses/advisories
lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo deny check
