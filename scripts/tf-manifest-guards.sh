#!/usr/bin/env bash
# Negative-test the foundation-manifest guards in infra/modules/manifest.
#
# The manifest is the one file the release pipeline reads, so the rules about what may
# appear in it are worth proving rather than asserting. Each case below builds a
# throwaway root that calls the module with a manifest that should be rejected, and one
# that should be accepted. No AWS credentials and no network calls beyond provider
# installation are involved: every check is Terraform variable validation. Use the
# committed AWS provider lock, matching the other Terraform gates. Set
# TF_MANIFEST_PLUGIN_DIR to a provider filesystem mirror for offline installation.
#
#   ./scripts/tf-manifest-guards.sh
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
module="${repo_root}/infra/modules/manifest"
work="$(mktemp -d)"
trap 'rm -rf "${work}"' EXIT

failures=0

# render <dir> <resources-hcl> <secret-refs-hcl> <environment>
render() {
  mkdir -p "$1"
  cp "${repo_root}/infra/envs/staging/foundation/.terraform.lock.hcl" "$1/"
  cat > "$1/main.tf" <<EOF
terraform {
  required_version = ">= 1.10.0, < 2.0.0"
  required_providers { aws = { source = "hashicorp/aws", version = "~> 6.0" } }
}
provider "aws" { region = "us-east-2" }

module "under_test" {
  source      = "${module}"
  bucket      = "example-bucket"
  key         = "envs/staging/foundation-manifest.json"
  kms_key_arn = "arn:aws:kms:us-east-2:000000000000:key/00000000-0000-0000-0000-000000000000"

  manifest = {
    schema_version = "1.0"
    project        = "ernote"
    environment    = "$4"
    account_id     = "000000000000"
    region         = "us-east-2"
    generated_by   = "guard-check"
    resources      = $2
    secret_refs    = $3
  }
}
EOF
}

# check <expectation: reject|accept> <name> <resources> <secret-refs> <environment>
#       [expected-validation-message]
check() {
  local expectation="$1" name="$2" dir="${work}/${2}"
  render "${dir}" "$3" "$4" "$5"
  local init_args=()
  if [[ -n "${TF_MANIFEST_PLUGIN_DIR:-}" ]]; then
    init_args+=("-plugin-dir=${TF_MANIFEST_PLUGIN_DIR}")
  fi
  terraform -chdir="${dir}" init -backend=false -input=false -lockfile=readonly \
    -no-color "${init_args[@]}" >"${work}/init.log" 2>&1 || {
    cat "${work}/init.log"
    exit 1
  }
  if terraform -chdir="${dir}" validate -no-color >"${work}/validate.log" 2>&1; then
    actual="accept"
  else
    actual="reject"
  fi
  if [ "${actual}" = "${expectation}" ] &&
     { [[ "${expectation}" == "accept" ]] ||
       { grep -Eq '^Error: Invalid value for (input )?variable' "${work}/validate.log" &&
         grep -Fq "$6" "${work}/validate.log"; }; }; then
    printf '  ok    %-34s %s\n' "${name}" "${actual}ed"
  else
    printf '  FAIL  %-34s expected %s, got %s\n' "${name}" "${expectation}" "${actual}"
    cat "${work}/validate.log"
    failures=$((failures + 1))
  fi
}

echo "foundation-manifest guards"
check reject secret_value_in_secret_refs '{}' '{ db_password = "hunter2-actual-password" }' staging 'manifest.secret_refs must contain Secrets Manager ARNs only'
check reject non_secretsmanager_arn      '{}' '{ db = "arn:aws:ssm:us-east-2:000000000000:parameter/db" }' staging 'manifest.secret_refs must contain Secrets Manager ARNs only'
check reject credential_named_resource   '{ db_password = "x" }' '{}' staging 'manifest.resources must not name credential material'
check reject api_key_named_resource      '{ deepgram_api_key = "x" }' '{}' staging 'manifest.resources must not name credential material'
check reject empty_resource_value        '{ vpc_id = "" }' '{}' staging 'Each manifest.resources value must be between 1 and 512 characters'
check reject unknown_environment         '{}' '{}' sandbox 'manifest.environment must be staging or prod'
check accept secretsmanager_arn_reference '{ audio_bucket = "ernote-audio-staging" }' '{ worker_db = "arn:aws:secretsmanager:us-east-2:000000000000:secret:ernote/staging/worker-db-AbCdEf" }' staging

if [ "${failures}" -ne 0 ]; then
  echo "${failures} guard check(s) failed"
  exit 1
fi
echo "all guard checks passed"
