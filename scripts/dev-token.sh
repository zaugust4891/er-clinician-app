```bash
# scripts/dev-token.sh — so `curl -H "Authorization: Bearer $(scripts/dev-token.sh)"` just works:
#!/usr/bin/env bash
set -euo pipefail
cargo run -q -p er-server --bin mint_dev_token -- token --sub "${1:-test-doc}"
```