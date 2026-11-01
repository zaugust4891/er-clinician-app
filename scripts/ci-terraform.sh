#!/usr/bin/env bash
# Static validation + reviewed Terraform-native security policy tests, no AWS plan.
set -euo pipefail
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
# Per-run provider cache only; the committed lockfiles still verify every package.
export TF_PLUGIN_CACHE_DIR="$work/providers"
mkdir -p "$TF_PLUGIN_CACHE_DIR"
export AWS_EC2_METADATA_DISABLED=true AWS_CONFIG_FILE=/dev/null AWS_SHARED_CREDENTIALS_FILE=/dev/null
export AWS_ACCESS_KEY_ID=testing AWS_SECRET_ACCESS_KEY=testing AWS_REGION=us-east-2
unset AWS_PROFILE AWS_SESSION_TOKEN AWS_WEB_IDENTITY_TOKEN_FILE AWS_ROLE_ARN AWS_CONTAINER_CREDENTIALS_RELATIVE_URI AWS_CONTAINER_CREDENTIALS_FULL_URI
mkdir -p "$work/infra"
cd "$repo"
# Never copy initialized backends, local tfvars or state into the CI check.
python3 - "$repo/infra" "$work/infra" <<'PYCODE'
import pathlib, shutil, sys
source, destination = map(pathlib.Path, sys.argv[1:])
for path in source.rglob("*"):
    relative = path.relative_to(source)
    if ".terraform" in relative.parts or not path.is_file():
        continue
    if path.name == "override.tf" or path.name.endswith("_override.tf"):
        continue
    if not (path.suffix == ".tf" or path.name.endswith(".tftest.hcl") or path.name == ".terraform.lock.hcl"):
        continue
    target = destination / relative
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(path, target)
PYCODE
terraform fmt -check -recursive "$work/infra"
for root in bootstrap envs/staging/foundation envs/staging/release envs/prod/foundation envs/prod/release; do
  terraform -chdir="$work/infra/$root" init -backend=false -input=false -lockfile=readonly -no-color
  terraform -chdir="$work/infra/$root" validate -no-color
done
terraform -chdir="$work/infra/bootstrap" test -no-color
scripts/tf-manifest-guards.sh
scripts/tf-network-check.sh
scripts/tf-data-check.sh
python3 scripts/check-db-log-privacy.py
