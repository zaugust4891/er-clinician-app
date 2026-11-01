#!/usr/bin/env bash
set +x
set -euo pipefail
exec python3 "$(dirname "$0")/deploy.py" "$@"
