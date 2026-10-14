#!/usr/bin/env bash
# S5 storage/queue/database gates. All providers are mocked: no AWS credentials or
# cloud mutations, and no RDS instance, bucket, queue or key is created. Copy only
# Terraform source/tests and committed provider locks, excluding local tfvars, backend
# configuration, and state.
#
# The foundation roots are included because the data module is wired there: they are
# what prove the audio bucket name reaches both the bucket and S4's gateway endpoint
# policy, that the manifest publishes the S5 keys, and that production stays disabled.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

for relative in modules/data modules/network modules/manifest modules/s3-encryption-guards envs/staging/foundation envs/prod/foundation; do
  source_dir="${repo_root}/infra/${relative}"
  test_dir="${work}/infra/${relative}"
  mkdir -p "${test_dir}"
  cp "${source_dir}"/*.tf "${test_dir}/"
  if [[ -d "${source_dir}/tests" ]]; then
    cp -R "${source_dir}/tests" "${test_dir}/"
  fi
  if [[ -f "${source_dir}/.terraform.lock.hcl" ]]; then
    cp "${source_dir}/.terraform.lock.hcl" "${test_dir}/"
  fi
done
cp "${repo_root}/infra/envs/staging/foundation/.terraform.lock.hcl" "${work}/infra/modules/data/"

for relative in modules/s3-encryption-guards modules/data envs/staging/foundation envs/prod/foundation; do
  test_dir="${work}/infra/${relative}"
  # A local filesystem provider mirror is optional; the lock still verifies it.
  init_args=()
  if [[ -n "${TF_DATA_PLUGIN_DIR:-}" ]]; then
    init_args+=("-plugin-dir=${TF_DATA_PLUGIN_DIR}")
  fi
  terraform -chdir="${test_dir}" init -backend=false -input=false -lockfile=readonly -no-color "${init_args[@]}" >"${work}/init.log" 2>&1 || {
    cat "${work}/init.log"
    exit 1
  }
  terraform -chdir="${test_dir}" validate -no-color
  terraform -chdir="${test_dir}" test -no-color
done
