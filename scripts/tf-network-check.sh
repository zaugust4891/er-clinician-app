#!/usr/bin/env bash
# S4 security/placement gates. All providers are mocked: no AWS credentials or
# cloud mutations. Copy only Terraform source/tests and committed provider locks,
# excluding local tfvars, backend configuration, and state.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

# modules/auth, modules/cluster and modules/data are copied but not tested here: the
# foundation roots call them, so their configuration cannot be initialised without
# them. Their own gate is scripts/tf-data-check.sh.
for relative in modules/network modules/manifest modules/edge modules/task modules/compute modules/auth modules/cluster modules/data modules/identity modules/s3-encryption-guards envs/staging/foundation envs/prod/foundation; do
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
cp "${repo_root}/infra/envs/staging/foundation/.terraform.lock.hcl" "${work}/infra/modules/network/"

for relative in modules/network envs/staging/foundation envs/prod/foundation; do
  test_dir="${work}/infra/${relative}"
  # A local filesystem provider mirror is optional; the lock still verifies it.
  init_args=()
  if [[ -n "${TF_NETWORK_PLUGIN_DIR:-}" ]]; then
    init_args+=("-plugin-dir=${TF_NETWORK_PLUGIN_DIR}")
  fi
  terraform -chdir="${test_dir}" init -backend=false -input=false -lockfile=readonly -no-color "${init_args[@]}" >"${work}/init.log" 2>&1 || {
    cat "${work}/init.log"
    exit 1
  }
  terraform -chdir="${test_dir}" validate -no-color
  terraform -chdir="${test_dir}" test -no-color
done
