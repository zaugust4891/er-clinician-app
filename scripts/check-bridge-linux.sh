#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo test --locked -p er-device -p er-ffi --no-default-features
cargo test --locked -p er-ffi --no-default-features --features ffi-test
cargo test --locked -p er-app --no-default-features
cargo clippy --locked -p er-device -p er-ffi -p er-bindgen --all-targets -- -D warnings
scripts/check-device-deps.sh
scripts/generate-swift-bindings.sh --check
python3 scripts/test_apple_bridge.py
