#!/usr/bin/env bash
# The local CI: formatting, lints as errors, and every test.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --check
cargo clippy --all-targets --quiet -- -D warnings
cargo test --quiet
