#!/usr/bin/env bash
# Build/inspect the exact local candidate once; no push, export, cloud, or provider calls.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo"
work="$(mktemp -d)"
name="ernote-image-$(basename "$work" | tr '[:upper:].' '[:lower:]-')"
pg_image='postgres:16@sha256:081f1bc7bd5e143dbb6e487b710bbc27712cdcfaced4c071b8e47349aa1b4171'
cleanup() {
  local status=$?
  trap - EXIT
  docker rm -f "$name-db" >/dev/null 2>&1 || true
  docker network rm "$name" >/dev/null 2>&1 || true
  if [[ -z "${CI_IMAGE_ID_FILE:-}" && -f "$work/image.id" ]]; then docker image rm "$(cat "$work/image.id")" >/dev/null 2>&1 || true; fi
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
docker build --no-cache-filter runtime --platform linux/amd64 --provenance=false --sbom=false --iidfile "$work/image.id" .
image="$(cat "$work/image.id")"
[[ "$(docker image inspect --format '{{.Architecture}} {{.Config.User}}' "$image")" == 'amd64 65532:65532' ]]
docker run --rm --network none --entrypoint /bin/sh "$image" -ec '
  test -s /etc/ssl/certs/ca-certificates.crt
  test -s /etc/ssl/certs/rds-global-bundle.pem
  for binary in er-server er-worker er-migrate er-db-bootstrap er-onboard; do
    ldd "/usr/local/bin/$binary" > /tmp/ldd.txt
    if grep -q "not found" /tmp/ldd.txt; then exit 1; fi
  done
'
for binary in er-server er-worker er-migrate er-db-bootstrap er-onboard; do
  if timeout 30 docker run --rm --network none --entrypoint "/usr/local/bin/$binary" "$image" >"$work/start.log" 2>&1; then
    echo "$binary unexpectedly started without required APP_ENV" >&2; exit 1
  else
    code=$?
    [[ "$code" == 1 ]] || { echo "$binary failed outside the configuration guard" >&2; exit 1; }
    rg -q 'configuration_invalid' "$work/start.log"
  fi
done
docker network create --internal "$name" >/dev/null
docker run -d --name "$name-db" --network "$name" --network-alias postgres \
  --tmpfs /var/lib/postgresql/data -e POSTGRES_PASSWORD=ci -e POSTGRES_DB=ernote_ci "$pg_image" >/dev/null
deadline=$((SECONDS + 90))
until docker exec "$name-db" pg_isready -h 127.0.0.1 -U postgres -d ernote_ci >/dev/null 2>&1; do
  (( SECONDS < deadline )) || { echo 'Image test database not ready' >&2; exit 1; }
  sleep 1
done
migration=(docker run --rm --network "$name" -e APP_ENV=dev -e AWS_REGION=us-east-2 \
  -e DATABASE_URL=postgres://postgres:ci@postgres/ernote_ci --entrypoint /usr/local/bin/er-migrate "$image")
"${migration[@]}"
"${migration[@]}"
docker exec "$name-db" psql -U postgres -d ernote_ci -v ON_ERROR_STOP=1 -c \
  "UPDATE _sqlx_migrations SET checksum = decode('00', 'hex') WHERE version = (SELECT min(version) FROM _sqlx_migrations)" >/dev/null
if "${migration[@]}" >"$work/migration.log" 2>&1; then
  echo 'Image migration accepted a changed checksum' >&2; exit 1
fi
rg -q checksum_mismatch "$work/migration.log"
# Local daemon image ID binds the scan to the bytes just built and exercised.
# No ignore-unfixed: every HIGH/CRITICAL finding blocks until explicitly resolved.
trivy image --config /dev/null --ignorefile /dev/null --scanners vuln --severity HIGH,CRITICAL --exit-code 1 --timeout 15m "$image"
echo "Candidate image build, runtime, migration and vulnerability gates passed: $image"

# The protected deployment job publishes these already exercised/scanned bytes.
if [[ -n "${CI_IMAGE_ID_FILE:-}" ]]; then cp "$work/image.id" "$CI_IMAGE_ID_FILE"; fi
