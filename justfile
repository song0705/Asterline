# Asterline developer tasks. Run `just <task>`; `just` alone runs the app.

default: run

# Launch the app (pass extra args after `--`, e.g. `just run --fake`).
run *ARGS:
    cargo run --quiet --bin asterline -- {{ARGS}}

# Install `asterline` and the short `ast` alias into ~/.cargo/bin.
install:
    cargo install --path . --force

# Build an optimized release binary.
build:
    cargo build --release

test:
    cargo test

fmt:
    cargo fmt

# The full CI gate, run locally.
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test
