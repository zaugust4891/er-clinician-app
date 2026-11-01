#!/usr/bin/env bash
set +x
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo"
cargo build --locked -p er-api --features client --example remote_smoke
exec python3 scripts/remote-smoke.py
