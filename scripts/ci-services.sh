#!/usr/bin/env bash
# Run the whole application gate in a disposable source tree and Compose project.
# No .env, developer keys, cloud credentials, or developer databases are consumed.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/ernote-ci.XXXXXXXX")"
chmod 700 "$work"
project="ernote-ci-$(basename "$work" | tr '[:upper:].' '[:lower:]-')"
compose=(docker compose -f "$repo/ci/compose.yml" -p "$project")
cleanup() {
  local status=$?
  trap - EXIT
  "${compose[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || status=1
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Explicit allowlist of environment entries, including only build-tool locations.
# Run a new shell so AWS profiles, web identity, ECS credentials and provider keys
# inherited from an operator shell cannot leak into test credential resolution.
if [[ "${1:-}" != --isolated ]]; then
  trap - EXIT INT TERM
  rm -rf "$work"
  exec env -i PATH="$PATH" HOME="$HOME" \
    CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" \
    CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo/target}" \
    K6_BIN="${K6_BIN:-}" \
    CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}" \
    CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}" \
    CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}" CARGO_INCREMENTAL=0 \
    bash "$repo/scripts/ci-services.sh" --isolated
fi
export APP_ENV=dev SQLX_OFFLINE=true
export AWS_ACCESS_KEY_ID=dev AWS_SECRET_ACCESS_KEY=devdevdev AWS_REGION=us-east-2 AWS_DEFAULT_REGION=us-east-2
export AWS_EC2_METADATA_DISABLED=true AWS_CONFIG_FILE=/dev/null AWS_SHARED_CREDENTIALS_FILE=/dev/null
export AWS_ENDPOINT_URL_BEDROCK_RUNTIME=http://127.0.0.1:9
export RUST_BACKTRACE=0 RUST_LOG=off

mkdir -p "$work/source"
# Copy current source (including local edits), not .git, .env, data or key fixtures.
# Tests and SQLx preparation share these fresh keys for the entire run.
(cd "$repo" && tar --exclude=target --exclude='.env*' --exclude='*.pem' --exclude='*.db*' --exclude='*.sqlite*' --exclude='*.wav' --exclude='*.mp3' --exclude='*.m4a' \
  -cf - Cargo.toml Cargo.lock rust-toolchain.toml crates .sqlx docs/api load) | tar -xf - -C "$work/source"
cd "$work/source"
# Stop dotenv from searching parent directories outside this disposable source tree.
: > .env
mkdir -p dev/keys
umask 077
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out dev/keys/dev_private.pem 2>/dev/null
openssl pkey -in dev/keys/dev_private.pem -pubout -out dev/keys/dev_public.pem 2>/dev/null

"${compose[@]}" up -d --wait --wait-timeout 120
pg_port="$("${compose[@]}" port postgres 5432 | awk -F: '{print $NF}')"
s3_port="$("${compose[@]}" port minio 9000 | awk -F: '{print $NF}')"
sqs_port="$("${compose[@]}" port elasticmq 9324 | awk -F: '{print $NF}')"
export DATABASE_URL="postgres://ci:ci@127.0.0.1:$pg_port/ernote_ci"
export S3_ENDPOINT_URL="http://127.0.0.1:$s3_port" SQS_ENDPOINT_URL="http://127.0.0.1:$sqs_port"
# A bounded protocol probe, not just container-running/TCP-open status.
wait_for_services() {
  local deadline=$((SECONDS + $1))
  until "${compose[@]}" exec -T postgres pg_isready -U ci -d ernote_ci >/dev/null 2>&1 &&
    curl --fail --silent --max-time 2 "$S3_ENDPOINT_URL/minio/health/ready" >/dev/null &&
    curl --fail --silent --max-time 2 -d 'Action=ListQueues&Version=2012-11-05' "$SQS_ENDPOINT_URL" >/dev/null; do
    if (( SECONDS >= deadline )); then
      echo 'CI services failed readiness (Postgres, MinIO or ElasticMQ)' >&2
      return 1
    fi
    sleep 1
  done
}
wait_for_services 90
# ElasticMQ must return queue URLs usable from the host even with dynamic ports.
# The SDK fixture also uses the configured endpoint for requests.
echo 'CI services ready; running offline compilation and full workspace tests'
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo run --quiet --locked -p er-api --example openapi --features schema > "$work/openapi.json"
cmp docs/api/openapi.json "$work/openapi.json"
cargo clippy -p er-api --example openapi --features schema --locked -- -D warnings
cargo run --quiet --locked -p er-server --bin mint_dev_token -- jwks > dev/keys/jwks.json
test_gate() {
  local status=0
  "$@" > "$work/tests.log" 2>&1 || status=$?
  # Test names and totals only: panic output can include synthetic clinical text.
  rg '^test [[:alnum:]_:]+ \.\.\. (ok|FAILED|ignored)|^test result:' "$work/tests.log" || true
  if (( status )); then
    echo 'Test gate failed; raw fixture/panic output is deliberately not published' >&2
    return "$status"
  fi
}
test_gate cargo test --workspace --locked
if [[ -n "${K6_BIN:-}" ]]; then
  test_gate cargo test -p er-server --test load_gate --locked -- --ignored
fi
test_gate cargo test -p er-ai --locked
cargo tree -p er-ai --locked -e normal > "$work/ai-tree.txt"
cat "$work/ai-tree.txt"
if rg -q 'aws-(sdk-bedrockruntime|config|smithy)' "$work/ai-tree.txt"; then
  echo 'The standalone er-ai build acquired an AWS dependency' >&2
  exit 1
fi
# A different database from sqlx::test's per-case databases, freshly migrated.
RUST_LOG=info cargo run --quiet --locked -p er-server --bin er-migrate
SQLX_OFFLINE=false cargo sqlx prepare --workspace --check -- --all-targets --locked
# Verify the freshness command really rejects a missing cached query. This only
# alters the disposable copy, and requires the specific SQLx mismatch diagnostic.
queries=(.sqlx/query-*.json)
mv "${queries[0]}" "$work/query.json"
if SQLX_OFFLINE=false cargo sqlx prepare --workspace --check -- --all-targets --locked > "$work/stale.log" 2>&1; then
  echo 'SQLx accepted a missing metadata query' >&2
  exit 1
fi
rg -q 'prepare check failed: .sqlx is missing one or more queries' "$work/stale.log"
mv "$work/query.json" "${queries[0]}"
echo 'SQLx freshness rejected missing metadata as expected'
# Each absent service must fail the readiness gate independently. Restore it before
# the next case; these are this run's disposable containers only.
for service in postgres minio elasticmq; do
  "${compose[@]}" stop "$service" >/dev/null 2>&1
  if wait_for_services 3 > "$work/readiness.log" 2>&1; then
    echo "Readiness incorrectly accepted absent $service" >&2
    exit 1
  fi
  echo "Readiness rejected absent $service as expected"
  "${compose[@]}" start "$service" >/dev/null 2>&1
  # Docker may allocate a new host port when a stopped container restarts.
  s3_port="$("${compose[@]}" port minio 9000 | awk -F: '{print $NF}')"
  sqs_port="$("${compose[@]}" port elasticmq 9324 | awk -F: '{print $NF}')"
  export S3_ENDPOINT_URL="http://127.0.0.1:$s3_port" SQS_ENDPOINT_URL="http://127.0.0.1:$sqs_port"
  wait_for_services 90
done
echo 'Application, SQLx freshness and missing-service gates passed'
