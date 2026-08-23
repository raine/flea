# Rust project checks

set positional-arguments
set shell := ["bash", "-euo", "pipefail", "-c"]

# List available commands
default:
    @just --list

# Run dependency boundaries and project checks
check:
    scripts/check-dependency-boundaries
    checkle run all

# Run check and fail if there are uncommitted changes for CI
check-ci: check
    #!/usr/bin/env bash
    set -euo pipefail
    if ! git diff --quiet || ! git diff --cached --quiet; then
        echo "Error: check caused uncommitted changes"
        echo "Run 'just check' locally and commit the results"
        git diff --stat
        exit 1
    fi

# Install shims into the Git hooks directory
install-hooks:
    scripts/install-git-hook-shims

# Check Rust formatting through checkle
format:
    checkle run format-check

# Check clippy through checkle
clippy:
    checkle run clippy

# Check the build through checkle
build:
    checkle run build

# Run tests through checkle
test:
    checkle run test

# Install release binary globally from local source
install:
    cargo install --offline --path . --locked

# Install release binary globally from GitHub releases
install-release:
    #!/usr/bin/env bash
    set -euo pipefail
    install_root="${CARGO_INSTALL_ROOT:-${CARGO_HOME:-$HOME/.cargo}}"
    FLEA_INSTALL_DIR="$install_root/bin" bash scripts/install

# Install debug binary globally via symlink
install-dev:
    cargo build && ln -sf $(pwd)/target/debug/flea ~/.cargo/bin/flea

# Run the application
run *ARGS:
    cargo run -- "$@"

# Internal release helper
_release bump *ARGS:
    @cargo-release {{bump}} {{ARGS}}

# Release a new patch version
release *ARGS:
    @just _release patch {{ARGS}}
