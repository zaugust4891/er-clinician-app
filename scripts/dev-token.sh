#!/usr/bin/env bash
# scripts/dev-token.sh — so `curl -H "Authorization: Bearer $(scripts/dev-token.sh)"` just works:
set -euo pipefail
# Run from the repo root so the binary finds dev/keys/*.pem regardless of caller's cwd.
cd "$(dirname "$0")/.."
cargo run -q -p er-server --bin mint_dev_token -- token --sub "${1:-test-doc}"
