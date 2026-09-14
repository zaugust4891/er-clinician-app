# Staging deployment runbook

Written 2026-09-14 for Phase 4 Step 8. This document grows one section at a time as the
step lands. **S3 is complete: everything under "State and ownership" below has been
implemented and can be run.** S4 network code and offline checks are also implemented;
its live checks await the complete foundation. The release, onboarding, and rollback sections are stubs
naming the sub-step that will fill them in.

Staging is **synthetic-only**. Nothing here authorises real patient audio, transcripts,
or notes; the pre-PHI conditions in [`deployment-contract.md`](deployment-contract.md)
remain binding.

---

## Contents

- [What Terraform state is, and why it gets its own apply](#what-terraform-state-is-and-why-it-gets-its-own-apply)
- [Before you start](#before-you-start)
- [State and ownership (S3)](#state-and-ownership-s3)
  - [1. Create the state bucket](#1-create-the-state-bucket)
  - [2. Move bootstrap's own state into the bucket](#2-move-bootstraps-own-state-into-the-bucket)
  - [3. Verify the bucket protections](#3-verify-the-bucket-protections)
  - [4. Initialise the environment roots](#4-initialise-the-environment-roots)
  - [5. Verify locking](#5-verify-locking)
  - [6. Verify staging cannot touch production state](#6-verify-staging-cannot-touch-production-state)
  - [7. Verify the wrong-account guard](#7-verify-the-wrong-account-guard)
- [Network and connectivity (S4)](#network-and-connectivity-s4)
- [The foundation manifest](#the-foundation-manifest)
- [Recovery procedures](#recovery-procedures)
- [Not yet written](#not-yet-written)

---

## What Terraform state is, and why it gets its own apply

Terraform keeps a JSON file mapping each resource in your configuration to the real
thing it created — this `aws_db_instance` is that RDS instance, with this identifier.
Without it, Terraform cannot tell "create a database" from "this database already
exists". Losing state does not delete anything in AWS; it deletes Terraform's memory of
it, and recovering means importing every resource by hand.

Three consequences drive the design in `infra/`:

1. **State must be shared and versioned.** Two people, or a person and a CI job, must
   see the same file, and a bad write must be recoverable.
2. **State must be locked.** Two applies running at once against one state file can
   interleave writes and produce a file describing infrastructure that never existed.
3. **State is privileged reading.** It contains every resource identifier, and for some
   resource types it contains values that are sensitive even when the configuration
   took care not to print them. Marking an output `sensitive = true` hides it from the
   CLI; it does **not** remove it from the state file. Treat read access to state as
   equivalent to read access to the environment's shape.

The bucket that holds all that is created once, by `infra/bootstrap`, and is protected
independently of the stacks whose state it stores. A staging environment is allowed to
be destroyed and rebuilt. Its state history is not.

---

## Before you start

You need the AWS CLI, Terraform `>= 1.10` (the tested release is in `.terraform-version`
at the repository root), and an Identity Center session for the staging account.

Run these in a shell that has **not** sourced `.env` — that file holds local emulator
credentials (`AWS_ACCESS_KEY_ID=dev`) which would silently take precedence over your
SSO session.

```bash
unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN
aws sso login --profile ernote-staging-admin
aws sts get-caller-identity --profile ernote-staging-admin
```

Compare the `Account` field with the staging account recorded in
[`deployment-contract.md`](deployment-contract.md). Everything below assumes they
match; if they do not, stop and resolve that first — the guards will refuse to run
anyway, which is the point.

Export the profile once so the rest of the commands are shorter:

```bash
export AWS_PROFILE=ernote-staging-admin
export AWS_REGION=us-east-2
```

---

## State and ownership (S3)

### 1. Create the state bucket

`infra/bootstrap` has a chicken-and-egg problem: it creates the bucket every other root
stores state in, so its own first run has nowhere remote to write. It therefore starts
with a **local** state file — the `backend "s3"` block in `infra/bootstrap/backend.tf`
ships commented out.

```bash
cd infra/bootstrap
cp terraform.tfvars.example terraform.tfvars
# Edit terraform.tfvars: set aws_account_id to the value you verified above.

terraform init
terraform plan
```

Read the plan before applying. You should see one KMS key and alias, two S3 buckets
with their versioning/encryption/public-access/lifecycle/policy resources, five IAM
policies, and five IAM roles. Nothing else.

```bash
terraform apply
```

The apply prints the values every other root needs:

```bash
terraform output state_bucket
terraform output state_kms_key_arn
terraform output deploy_artifacts_bucket
terraform output backend_config     # ready-made -backend-config values per root
```

Record the bucket names and key ARN in `deployment-contract.md`. They are not secrets —
they are the non-secret configuration every `terraform init` needs.

> **What was created, in one sentence each.** A customer-managed KMS key with annual
> rotation, encrypting both buckets. A state bucket with versioning on, public access
> blocked, ACLs disabled, superseded versions kept for a year, and a bucket policy that
> denies plain HTTP, denies TLS below 1.2, and denies any upload that tries to override
> the KMS default with a weaker algorithm or a different key. A deployment-artifact
> bucket with the same protections, holding one manifest per environment. Five IAM
> policies, one per Terraform root, each allowing only its own state object, allowing
> its lock object separately, and **explicitly denying** every other root's objects.
> Five roles to hang those policies on.

### 2. Move bootstrap's own state into the bucket

The local `terraform.tfstate` file now describes the bucket that should be holding it.
Move it.

Uncomment the `terraform { backend "s3" { ... } }` block at the bottom of
`infra/bootstrap/backend.tf`, then:

```bash
terraform init -migrate-state \
  -backend-config="bucket=$(terraform output -raw state_bucket)" \
  -backend-config="kms_key_id=$(terraform output -raw state_kms_key_arn)"
```

`-migrate-state` tells Terraform that the backend changed and that it should copy the
existing state to the new location rather than start empty. It asks for confirmation
and prints where it is copying from and to. Answer `yes` only if both match what you
expect.

The `bucket` and `kms_key_id` values are passed at init time rather than written into
`backend.tf` because they are account-specific. They are not credentials.

> Pass the key's **ARN**, not `alias/ernote-tfstate`. The bucket policy denies any
> upload whose `x-amz-server-side-encryption-aws-kms-key-id` header is present and not
> equal to the key ARN, so an alias here produces an `AccessDenied` on the first state
> write rather than a clear error at init. **No AWS key
belongs in a backend block, a `backend.hcl`, or a tfvars file** — Terraform reads
credentials from your SSO session, and in CI from a short-lived OIDC role.

Verify the remote copy exists and is encrypted with the right key before you touch the
local file:

```bash
BUCKET=$(terraform output -raw state_bucket)
aws s3api head-object --bucket "$BUCKET" --key bootstrap/terraform.tfstate \
  --query '{encryption:ServerSideEncryption,key:SSEKMSKeyId,size:ContentLength}'

terraform plan   # must report no changes
```

`terraform plan` reporting "No changes" is the real proof: Terraform read the remote
state, compared it to reality, and found them identical.

Only then remove the local artefacts. `-migrate-state` leaves a
`terraform.tfstate.backup`; both it and `terraform.tfstate` still contain a full copy
of the state.

```bash
shred -u terraform.tfstate terraform.tfstate.backup 2>/dev/null || rm -f terraform.tfstate terraform.tfstate.backup
```

Both filenames are gitignored, so neither was ever staged, but they are still readable
on disk and in any backup of your home directory until they are removed.

### 3. Verify the bucket protections

Each of these should **fail**. A command that succeeds here is a finding.

```bash
BUCKET=$(terraform output -raw state_bucket)

# Plain HTTP is denied by bucket policy.
curl -s -o /dev/null -w '%{http_code}\n' "http://$BUCKET.s3.us-east-2.amazonaws.com/bootstrap/terraform.tfstate"
# expect 403

# Public access is blocked at the account-facing level too.
aws s3api get-public-access-block --bucket "$BUCKET" \
  --query 'PublicAccessBlockConfiguration'
# expect all four values true

# Versioning is on, which is what makes a bad state write recoverable.
aws s3api get-bucket-versioning --bucket "$BUCKET" --query Status
# expect "Enabled"

# An upload that tries to downgrade encryption is denied.
echo test > /tmp/probe.txt
aws s3api put-object --bucket "$BUCKET" --key bootstrap/probe.txt \
  --body /tmp/probe.txt --server-side-encryption AES256
# expect AccessDenied
rm -f /tmp/probe.txt
```

Then prove a prior version of a state object can be recovered. Do this against a
**disposable** key, never against real state:

```bash
printf 'v1\n' > /tmp/v.txt && aws s3 cp /tmp/v.txt "s3://$BUCKET/bootstrap/recovery-drill.txt"
printf 'v2\n' > /tmp/v.txt && aws s3 cp /tmp/v.txt "s3://$BUCKET/bootstrap/recovery-drill.txt"

# List the versions, oldest last.
aws s3api list-object-versions --bucket "$BUCKET" --prefix bootstrap/recovery-drill.txt \
  --query 'Versions[].{id:VersionId,latest:IsLatest,when:LastModified}'

# Fetch the older one by its version id and confirm it still says v1.
aws s3api get-object --bucket "$BUCKET" --key bootstrap/recovery-drill.txt \
  --version-id <the id where latest is false> /tmp/restored.txt && cat /tmp/restored.txt

# Clean up every version of the drill object.
aws s3api list-object-versions --bucket "$BUCKET" --prefix bootstrap/recovery-drill.txt \
  --query 'Versions[].VersionId' --output text | tr '\t' '\n' | while read -r v; do
    aws s3api delete-object --bucket "$BUCKET" --key bootstrap/recovery-drill.txt --version-id "$v"
  done
rm -f /tmp/v.txt /tmp/restored.txt
```

> **MFA delete** cannot be configured through Terraform or through a role — it requires
> the account root user and the CLI. It is worth enabling on the state bucket before
> production. Record the decision in `deployment-contract.md` either way.

### 4. Initialise the environment roots

Each environment root stores state at its own key in the same bucket.

```bash
cd ../..    # back to the repository root
export STATE_BUCKET=$(terraform -chdir=infra/bootstrap output -raw state_bucket)
export STATE_KMS_KEY_ARN=$(terraform -chdir=infra/bootstrap output -raw state_kms_key_arn)
export DEPLOY_BUCKET=$(terraform -chdir=infra/bootstrap output -raw deploy_artifacts_bucket)

for root in staging/foundation staging/release prod/foundation prod/release; do
  terraform -chdir="infra/envs/$root" init \
    -backend-config="bucket=$STATE_BUCKET" \
    -backend-config="kms_key_id=$STATE_KMS_KEY_ARN"
  terraform -chdir="infra/envs/$root" validate
done
```

Then fill in each root's `terraform.tfvars` from its `terraform.tfvars.example`, using
`$STATE_BUCKET` / `$DEPLOY_BUCKET` / `$STATE_KMS_KEY_ARN`.

The staging foundation plan now includes S4 networking and its recurring NAT/IP
charges. Review the network and budget section before applying. The release root
requires an S4 manifest; an older S3-only manifest will fail its required-key guard.
Data, identity, compute and telemetry are still added in S5–S9.

### 5. Verify locking

Terraform 1.10 replaced the old DynamoDB lock table with **native S3 locking**. The
backend takes a lock by writing a small `terraform.tfstate.tflock` object next to the
state, using a conditional write that only succeeds if the object does not already
exist. That is why `use_lockfile = true` appears in every backend block and why there
is no DynamoDB table anywhere in this project.

The mechanism is proven locally, with no AWS spend, by:

```bash
docker compose up -d minio
./scripts/tf-lock-check.sh
# ok: second operation refused the lock (HTTP 412 PreconditionFailed)
```

To check the same thing against the real bucket, start a long operation in one terminal
and attempt any operation on the same root in another:

```bash
# terminal 1
terraform -chdir=infra/envs/staging/foundation apply

# terminal 2, while the first is still running
terraform -chdir=infra/envs/staging/foundation plan -lock-timeout=0
# expect: Error acquiring the state lock ... PreconditionFailed
```

`-lock-timeout=0` means "do not wait"; without it Terraform retries for a while, which
is the right default in CI and unhelpful when you are trying to observe the lock.

**Releasing a stuck lock.** If a process is killed mid-apply, the lock object stays.
Confirm nobody is actually running, then:

```bash
terraform -chdir=infra/envs/staging/foundation force-unlock <lock id from the error>
```

The lock id is printed in the error message. Note that the state-access policies grant
`s3:DeleteObject` **only** on the `.tflock` object, never on the state object itself:
releasing a lock is a normal operation and deleting state is not, so they are separate
permissions.

### 6. Verify staging cannot touch production state

This is the separation the policies exist for. Assume the staging role and try to read
a production key.

```bash
ACCOUNT=$(aws sts get-caller-identity --query Account --output text)

creds=$(aws sts assume-role \
  --role-arn "arn:aws:iam::$ACCOUNT:role/ernote-tfstate-staging-foundation-operator" \
  --role-session-name state-separation-check \
  --query Credentials --output json)

export AWS_ACCESS_KEY_ID=$(echo "$creds" | jq -r .AccessKeyId)
export AWS_SECRET_ACCESS_KEY=$(echo "$creds" | jq -r .SecretAccessKey)
export AWS_SESSION_TOKEN=$(echo "$creds" | jq -r .SessionToken)
unset AWS_PROFILE

# Its own key: allowed (NoSuchKey before the first apply is a pass — the request was
# authorised, there is simply nothing there yet).
aws s3api head-object --bucket "$STATE_BUCKET" --key staging/foundation/terraform.tfstate

# Production's key: denied.
aws s3api head-object --bucket "$STATE_BUCKET" --key prod/foundation/terraform.tfstate
# expect AccessDenied

# Production's manifest: denied.
aws s3api head-object --bucket "$DEPLOY_BUCKET" --key envs/prod/foundation-manifest.json
# expect AccessDenied
```

Unset the temporary credentials afterwards:

```bash
unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN
export AWS_PROFILE=ernote-staging-admin
```

Repeat in the opposite direction with `ernote-tfstate-prod-foundation-operator` against
the staging keys. The deny is an explicit `Deny` statement, not merely an absent
`Allow`, so it holds even if a broader policy is attached to the same role later.

### 7. Verify the wrong-account guard

Point a root at an account that is not the one your credentials belong to and confirm
it refuses before planning any resource:

```bash
terraform -chdir=infra/envs/staging/foundation plan -var 'aws_account_id=111111111111'
# expect: provider rejects the account, or
#   Wrong AWS account for the staging foundation root: expected 111111111111, got <yours>
```

Two things produce that failure: `allowed_account_ids` on the provider, and the
`precondition` in each root's `guard.tf` comparing the live caller identity with the
declared account. The precondition exists because it gives a readable message.

---

## Network and connectivity (S4)

The foundation now includes `infra/modules/network/`: six subnets in two AZs,
one staging NAT, isolated DB placement, dedicated API/worker/migration/onboarding
SGs, closed ALB ingress and an S3 gateway endpoint. Paid interfaces are opt-in and
require actual resource ARNs. Production remains disabled.

Before applying, pin the reviewed AZ pair, check CIDR overlap and account identity,
and resolve the full-stack budget. The network alone is estimated at $36.50/month
at 730 hours in Ohio, before traffic and all later components. See the dated rates,
connection matrix, safe commands, diagnostic prerequisites and pending evidence in
[network-verification.md](network-verification.md). Run `scripts/tf-network-check.sh`
for the local gate. Live checks remain unperformed until S5–S9 exist.

## The foundation manifest

When the staging foundation root applies, it writes one JSON object:

```
s3://<deploy bucket>/envs/staging/foundation-manifest.json
```

It names what the release pipeline needs — subnet ids, security groups, the ECR
repository, the cluster, log groups, queue URLs, and the ARNs of Secrets Manager
secrets. **ARNs of secrets, never secret values.** The module validates that: a
`secret_refs` entry that is not a Secrets Manager ARN fails the plan, and a `resources`
key named like a credential fails the plan. `./scripts/tf-manifest-guards.sh` exercises
those rules offline.

The release root reads that object, checks its schema version, environment, account,
region, and project match, and checks every resource key it needs is present. A
foundation that has not finished applying therefore fails the release plan instead of
producing an ECS task definition that points at nothing.

Because the artefact bucket is versioned, a release can be pinned to one exact manifest
version with `-var manifest_version_id=...`. Do that in CI: a release approved against
one description of the foundation must not apply against a different one.

---

## Recovery procedures

**A bad state write.** The state bucket keeps superseded versions for a year. List them
with `aws s3api list-object-versions --bucket "$STATE_BUCKET" --prefix
staging/foundation/terraform.tfstate`, copy the good version to a local file, inspect
it, and restore it with `terraform state push`. Take the lock first, and be certain no
apply is in flight.

**A stuck lock.** See [Verify locking](#5-verify-locking).

**A deleted state object.** Versioning means a delete creates a delete marker rather
than removing bytes. Remove the delete marker by its version id to restore the object.
Note also that no role here is granted `s3:DeleteObject` on a state key at all.

**A lost state file with live infrastructure.** Nothing in AWS was harmed. Recover by
restoring a version if one exists, or by `terraform import` for each resource. This is
slow, which is why the bucket is versioned, `prevent_destroy` is set on it and the KMS
key, and no policy grants deletion of state objects.

---

## Not yet written

| Section | Arrives with |
|---|---|
| Database, queue, and bucket provisioning | S5 |
| IAM roles, database logins, secret population, rotation | S6 |
| Cognito pool and synthetic clinician onboarding | S7 |
| ECS services, the HTTPS edge, first release | S8, S11 |
| Log groups, alarms, and what is safe to log | S9 |
| CI gates reproducing the local test suite | S10 |
| Release, rollback, and the staging gate evidence | S11, S12 |
