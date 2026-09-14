#!/usr/bin/env bash
# Prove that Terraform's S3 backend refuses a second concurrent operation on the same
# state key, without spending anything in AWS.
#
# Terraform 1.10 replaced the old DynamoDB lock table with native S3 locking: the
# backend takes a lock by writing a small `<key>.tflock` object with a conditional
# "only if it does not already exist" header. S3 answers `412 PreconditionFailed` when
# someone else already holds it. This script runs two applies against the same key in
# the local MinIO container and checks that the second one is refused.
#
# It exercises the mechanism, not the real backend: MinIO stands in for S3 and there is
# no KMS, no bucket policy, and no IAM. The equivalent check against the real staging
# bucket is in docs/ops/staging-deploy.md.
#
#   docker compose up -d minio
#   ./scripts/tf-lock-check.sh
set -euo pipefail

endpoint="${MINIO_ENDPOINT:-http://localhost:9000}"
bucket="${TF_LOCK_CHECK_BUCKET:-tf-lock-check}"
hold_seconds="${TF_LOCK_CHECK_HOLD_SECONDS:-25}"

# The credentials docker-compose.yml gives MinIO. Local emulator values, not secrets.
export AWS_ACCESS_KEY_ID=dev
export AWS_SECRET_ACCESS_KEY=devdevdev
export AWS_REGION=us-east-1

work="$(mktemp -d)"
cleanup() {
  rm -rf "${work}"
  aws --endpoint-url "${endpoint}" s3 rm "s3://${bucket}" --recursive >/dev/null 2>&1 || true
  aws --endpoint-url "${endpoint}" s3api delete-bucket --bucket "${bucket}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

aws --endpoint-url "${endpoint}" s3api create-bucket --bucket "${bucket}" >/dev/null 2>&1 || true

for role in holder contender; do
  mkdir -p "${work}/${role}"
  cat > "${work}/${role}/main.tf" <<EOF
terraform {
  required_version = ">= 1.10.0"
  backend "s3" {}
}

# Creates nothing in the cloud; it just keeps the apply running long enough for the
# second process to collide with its lock.
resource "terraform_data" "hold" {
  input = "hold"
  provisioner "local-exec" {
    command = "sleep ${hold_seconds}"
  }
}
EOF
  cat > "${work}/${role}/backend.hcl" <<EOF
bucket                      = "${bucket}"
key                         = "lock-check/terraform.tfstate"
region                      = "us-east-1"
use_lockfile                = true
use_path_style              = true
encrypt                     = false
skip_credentials_validation = true
skip_metadata_api_check     = true
skip_region_validation      = true
skip_requesting_account_id  = true
endpoints = { s3 = "${endpoint}" }
EOF
  ( cd "${work}/${role}" && terraform init -backend-config=backend.hcl -input=false >/dev/null )
done

echo "holder: taking the lock"
( cd "${work}/holder" && terraform apply -auto-approve -input=false >"${work}/holder.log" 2>&1 ) &
holder_pid=$!

# Wait for the lock object to appear rather than guessing at a sleep duration.
for _ in $(seq 1 60); do
  if aws --endpoint-url "${endpoint}" s3api head-object \
       --bucket "${bucket}" --key "lock-check/terraform.tfstate.tflock" >/dev/null 2>&1; then
    break
  fi
  sleep 0.5
done

echo "contender: attempting the same key with -lock-timeout=0"
set +e
( cd "${work}/contender" && terraform plan -lock-timeout=0 -input=false >"${work}/contender.log" 2>&1 )
contender_status=$?
set -e

wait "${holder_pid}" || true

if [ "${contender_status}" -eq 0 ]; then
  echo "FAIL: the second operation acquired the same lock"
  sed -e 's/\x1b\[[0-9;]*m//g' "${work}/contender.log"
  exit 1
fi

if grep -q "Error acquiring the state lock" "${work}/contender.log" &&
   grep -q "PreconditionFailed" "${work}/contender.log"; then
  echo "ok: second operation refused the lock (HTTP 412 PreconditionFailed)"
else
  echo "FAIL: the second operation failed for some other reason"
  sed -e 's/\x1b\[[0-9;]*m//g' "${work}/contender.log"
  exit 1
fi
