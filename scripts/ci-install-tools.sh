#!/usr/bin/env bash
# Linux x86_64 CI tools. Verify downloaded bytes against reviewed upstream checksums.
set -euo pipefail
[[ "$(uname -sm)" == 'Linux x86_64' ]] || { echo 'CI tools require Linux x86_64' >&2; exit 1; }
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
destination="${CI_TOOLS_DIR:-$repo/target/ci-tools}"
mkdir -p "$destination"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fetch() {
  curl --fail --silent --show-error --location --retry 3 "$1" -o "$work/$2"
  echo "$3  $work/$2" | sha256sum --check --status
}
case "${1:-}" in
  scanners)
    fetch https://github.com/aquasecurity/trivy/releases/download/v0.69.3/trivy_0.69.3_Linux-64bit.tar.gz trivy.tar.gz 1816b632dfe529869c740c0913e36bd1629cb7688bd5634f4a858c1d57c88b75
    tar -xzf "$work/trivy.tar.gz" -C "$destination" trivy
    fetch https://github.com/gitleaks/gitleaks/releases/download/v8.24.3/gitleaks_8.24.3_linux_x64.tar.gz gitleaks.tar.gz 9991e0b2903da4c8f6122b5c3186448b927a5da4deef1fe45271c3793f4ee29c
    tar -xzf "$work/gitleaks.tar.gz" -C "$destination" gitleaks
    fetch https://github.com/rhysd/actionlint/releases/download/v1.7.7/actionlint_1.7.7_linux_amd64.tar.gz actionlint.tar.gz 023070a287cd8cccd71515fedc843f1985bf96c436b7effaecce67290e7e0757
    tar -xzf "$work/actionlint.tar.gz" -C "$destination" actionlint
    ;;
  terraform)
    [[ "$(cat "$repo/.terraform-version")" == 1.10.4 ]]
    fetch https://releases.hashicorp.com/terraform/1.10.4/terraform_1.10.4_linux_amd64.zip terraform.zip 64b7b60f35eb92e94a046b6c932ce53f632a9eb528e9d0097857f9f27420be3e
    unzip -oq "$work/terraform.zip" terraform -d "$destination"
    ;;
  *) echo 'usage: ci-install-tools.sh scanners|terraform' >&2; exit 2 ;;
esac
[[ -z "${GITHUB_PATH:-}" ]] || echo "$destination" >> "$GITHUB_PATH"
