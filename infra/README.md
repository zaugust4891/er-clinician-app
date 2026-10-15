# `infra/` — Terraform for the ER Clinician App

Phase 4 Step 8 S3 created this tree. It establishes **where Terraform state lives, who
may touch which part of it, and how a release learns what a foundation built**. S4's
[network module](modules/network/README.md) and S5's [data module](modules/data/README.md)
and S6's [identity module](modules/identity/README.md) are authored and wired into both
foundations; production remains disabled. S7–S9 still supply Cognito adoption,
compute, edge and telemetry. The release roots have manifest
guards but no ECS services or task definitions yet.

Status updated **2026-10-14**: approved bootstrap is applied, remote state migrated,
and live storage/locking/recovery/role checks pass. Data and network gates each pass
**25 runs** (overlapping foundation cases); all five roots validate. Shared encryption
guards preserve headerless default-KMS uploads and deny wrong algorithms/keys,
explicit SSE-KMS without a key, and SSE-C. Production retention guards remain in place.
Application network/data/runtime acceptance is pending S6–S9 and a budget decision.
See the [review checklist](../docs/ops/step8-s1-s5-review-checklist.md) and
[dated audit](../docs/ops/step8-audit-2026-10-14.md).

Nothing in this tree processes patient data. Staging is synthetic-only; see
[`docs/ops/deployment-contract.md`](../docs/ops/deployment-contract.md) for the
pre-PHI conditions that remain binding.

## The roots

A *root* is a directory Terraform runs in. Each one owns a separate state file, which
is what makes them separately permissioned and separately breakable.

| Root | Owns | State key |
|---|---|---|
| `bootstrap/` | The state bucket, its KMS key, the deployment-artifact bucket, and the IAM policies and roles that gate access to them | `bootstrap/terraform.tfstate` |
| `envs/staging/foundation/` | Authored: network, data stores, IAM/secret containers and manifest wiring. Planned: Cognito, edge, observability, ECR and cluster | `staging/foundation/terraform.tfstate` |
| `envs/staging/release/` | Authored: manifest consumption and guards. Planned ownership: ECS services and task definitions only | `staging/release/terraform.tfstate` |
| `envs/prod/foundation/` | Same as staging foundation. Authored, `enabled = false` | `prod/foundation/terraform.tfstate` |
| `envs/prod/release/` | Same as staging release. Authored, `enabled = false` | `prod/release/terraform.tfstate` |

### Why foundation and release are split

A deployment happens many times a day and should need permission to do one thing:
replace ECS task definitions and point services at them. A foundation change happens
rarely, is reviewed, and can delete a database. Putting them in one state would force
every routine deploy to carry permission to do the second thing.

The split also removes a subtler problem. If CI registers task definitions directly
while Terraform also declares them, the two disagree about what the current revision
is, and Terraform's next plan proposes to undo the deploy. Here Terraform owns the task
definition in the release state; CI supplies the image digest as an input.

### How release learns resource names

The foundation root publishes a small JSON **manifest** — resource ARNs, subnet ids,
queue URLs, and Secrets Manager ARNs (references, never values) — to the versioned
deployment-artifact bucket. The release root reads that one object.

It could have read the foundation's state file with `terraform_remote_state` instead.
That would hand the release identity everything in the state, including values that are
only there because Terraform had to store them. A manifest is an explicit allowlist,
and the release role's IAM policy can be scoped to a single object key. See
[`modules/manifest/README.md`](modules/manifest/README.md).

## Running a root

```bash
# Every root is initialised with its bucket and key supplied at init time. Neither is
# secret; neither is written into the repository.
terraform -chdir=infra/envs/staging/foundation init \
  -backend-config="bucket=$STATE_BUCKET" \
  -backend-config="kms_key_id=$STATE_KMS_KEY_ARN"

terraform -chdir=infra/envs/staging/foundation plan
```

Credentials come from an AWS SSO session locally, or a short-lived GitHub OIDC role in
CI. There are no long-lived AWS keys anywhere in this tree.

The first run of all of this — creating the state bucket, migrating bootstrap's own
state into it, and verifying the guards — is written out step by step in
[`docs/ops/staging-deploy.md`](../docs/ops/staging-deploy.md).

## Guards

Every root refuses to run against the wrong account twice over: `allowed_account_ids`
on the provider stops it at the provider level, and a `precondition` in `guard.tf`
compares the live STS caller identity with the account the operator declared, failing
during `plan` before anything is created.

The release roots additionally refuse a manifest whose environment, account, region, or
project does not match, and a manifest missing any resource key the release needs.

## Checks that run without AWS

```bash
terraform fmt -check -recursive infra/
./scripts/tf-network-check.sh    # mocked network, endpoint and foundation gates
./scripts/tf-data-check.sh       # mocked data, identity/CI and foundation gates
python3 -m unittest scripts/test_db_secrets.py
./scripts/tf-manifest-guards.sh   # manifest cannot carry secret values
./scripts/tf-lock-check.sh        # two concurrent operations cannot share a lock
```

## Conventions

- Terraform `>= 1.10` (native S3 state locking, no DynamoDB table). The tested release
  is pinned in `.terraform-version` at the repository root.
- AWS provider `~> 6.0`, with `.terraform.lock.hcl` committed per root and locked for
  `linux_amd64`, `linux_arm64`, and `darwin_arm64`.
- `terraform.tfvars` is gitignored. Each root ships a `terraform.tfvars.example`.
- Resource names are `<project>-<purpose>-...`, and carry no patient, clinician, or
  hospital identifier.
