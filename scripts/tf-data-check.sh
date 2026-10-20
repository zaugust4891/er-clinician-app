#!/usr/bin/env bash
# S5-S9 data, identity, auth, compute, edge and telemetry gates. All providers are mocked: no AWS credentials or
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

for relative in modules/observability modules/edge modules/task modules/compute modules/auth modules/cluster modules/data modules/identity modules/network modules/manifest modules/s3-encryption-guards envs/staging/foundation envs/staging/release envs/prod/foundation envs/prod/release; do
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

# Terraform 1.10 mock-apply teardown still enforces prevent_destroy. Verify the
# two source guards exist, then relax ONLY the disposable mocked copy for cleanup.
# Nothing below runs an unmocked plan/apply or touches a real backend/state.
for file in main.tf audit.tf; do
  guard_file="${repo_root}/infra/modules/observability/${file}"
  rg -q 'prevent_destroy = true' "${guard_file}"
  sed -i 's/prevent_destroy = true/prevent_destroy = false/' "${work}/infra/modules/observability/${file}"
done
for module in observability auth cluster data identity edge compute task; do
  cp "${repo_root}/infra/envs/staging/foundation/.terraform.lock.hcl" "${work}/infra/modules/${module}/"
done

targets=(modules/observability modules/edge modules/task modules/compute modules/s3-encryption-guards modules/auth modules/cluster modules/data modules/identity envs/staging/foundation envs/staging/release envs/prod/foundation envs/prod/release)
# Optional explicit targets let a focused rerun use the same isolated workflow.
if (( $# )); then
  for requested in "$@"; do
    case " ${targets[*]} " in
      *" ${requested} "*) ;;
      *) echo "Unknown Terraform test target: ${requested}" >&2; exit 2 ;;
    esac
  done
  targets=("$@")
fi
for relative in "${targets[@]}"; do
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
