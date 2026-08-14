# Asterline developer tasks. Run `just <task>`; `just` alone runs the app.

default: run

# Launch the app (pass extra args after `--`, e.g. `just run --fake`).
run *ARGS:
    cargo run --quiet --bin asterline -- {{ARGS}}

# Install `asterline` and the short `ast` alias into ~/.cargo/bin.
install:
    cargo install --path . --locked --force

# Build an optimized release binary.
build:
    cargo build --release --locked

test:
    cargo test --all-targets --locked --no-fail-fast

fmt:
    cargo fmt

# The portable code-quality gate used by CI. Platform packaging stays in Actions.
check:
    cargo fmt --check
    cargo clippy --all-targets --locked -- -D warnings
    cargo test --all-targets --locked --no-fail-fast
    cargo audit
