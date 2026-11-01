# Staging deployment runbook

Written 2026-09-14; status updated **2026-10-14** for Phase 4 Step 8. Approved bootstrap
is applied and its remote state verified. All five scoped backends initialized;
application network/data/workloads remain unapplied. Current data/network gates each
pass **25 runs**, with overlapping foundation cases. See the
[dated audit](step8-audit-2026-10-14.md) and
[budget worksheet](staging-cost-review-2026-10-14.md). **2026-10-15:** S6 IAM/secret
containers, DB bootstrap/grants and rotation tooling are authored; see
[identity verification](identity-verification.md). Their live gate remains unperformed.
S7 Cognito, onboarding and local auth gates are authored; its live acceptance is
pending. S8–S9 compute, edge and telemetry are authored and locally tested; live acceptance
remains in [compute/edge](compute-edge-verification.md) and
[telemetry verification](telemetry-verification.md). Release/rollback workflows
and private onboarding task templates remain S8/S11 work.

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
- [Data stores and acceptance (S5)](#data-stores-and-acceptance-s5)
- [Identity and credential bootstrap (S6)](#identity-and-credential-bootstrap-s6)
- [Cognito and synthetic onboarding (S7)](#cognito-and-synthetic-onboarding-s7)
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

The current account's bootstrap already exists and uses S3. Reconnect this checkout
with `terraform -chdir=infra/bootstrap init -backend-config=backend.hcl`; its ignored
backend file selects the bootstrap state role. Do not recreate the existing buckets.

For a **new account**, `infra/bootstrap` has a chicken-and-egg problem: it creates the
bucket that will store its state. Temporarily comment out its currently enabled
`backend "s3"` block for that account's first local apply, then migrate that exact state.
The following first-apply instructions describe that workflow.

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
> upload selecting an unapproved key. The October 14 live probe accepted an alias
> resolving to the approved key, but backend configuration deliberately uses the
> immutable key ARN. **No AWS credential
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

The staging foundation plan includes S4 networking, S5 data and S6 identity resources, with
recurring NAT/IP, RDS, storage and key charges. Review the full-stack budget before
applying. The staging release root requires the authored S4–S7 manifest keys,
including valid Cognito pool/client IDs in its Region; older or auth-disabled
manifests fail the release guard. S8–S9 now supply compute, edge and telemetry; live acceptance remains pending.
Production remains disabled.

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

# Its own key: readable after its first apply. Before that, a 404 establishes only
# absence; scoped backend initialization and lock checks provide separate evidence.
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

S3 can return 404 for a missing opposite-environment object even when the deployed
policy explicitly denies it. Use an existing approved object for the live denial
assertion; never write dummy content into actual state or manifest keys. The October
14 audit used disposable objects under each state prefix, then separately simulated
explicit denies on the actual state/manifest paths. Live denial of an existing
published manifest remains pending foundation publication.

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
for the local gate. S5 data configuration is authored; complete live task diagnostics
still need S6 roles, S8 compute and safe telemetry. No live connectivity pass is recorded.

## Data stores and acceptance (S5)

`infra/modules/data/` is wired into both foundation roots. It defines private,
KMS-encrypted PostgreSQL 16 with capped gp3 storage, backups and deletion protection;
the versioned audio bucket with separate lifecycle clocks; a standard queue/DLQ pair;
and a dedicated rotating application-data key. It publishes identifiers through the
foundation manifest without runtime credentials or the master-credential secret ARN.

The shared encryption module retains header-presence guards and adds SSE-KMS missing-key
and SSE-C denies. Its guards passed signed live probes on both bootstrap buckets;
application audio uploads/runtime grants remain unverified. Production examples keep
`enabled = false` and audio values unset pending approval. The October 14 data gate
passed **25 runs**: shared policy 1, data 12, staging 4, production 8. All five roots
validated in backend-disabled copies against locked AWS provider 6.64.0.

Before the complete foundation apply, verify identity, regional RDS options, retention
inputs and budget; finish the S6–S9 dependencies with workloads disabled and ingress
closed. Use [data-verification.md](data-verification.md) for the pending live inspections,
correctly signed upload probes, KMS/IAM tests and backup evidence. Local assertions do
not prove real uploads, state/lock writes, role isolation or restore capability.
The audio overwrite/version binding and restore rehearsal remain Step 9 work.

## Identity and credential bootstrap (S6)

Follow [identity-verification.md](identity-verification.md) after the approved
foundation, ECR/image and log groups exist. First bootstrap creates four separate
logins before the first owner-run migration. Credentials are staged outside
Terraform, pinned to a one-shot private ECS task, and promoted only after that exact
task succeeds. API/worker tasks receive only their own AWSCURRENT secret fields.
Rotation uses a scheduled maintenance window and replacement tasks; no live rotation
success is recorded yet. S6 CI templates default to disabled pending S11 protection
and final provisioning-policy review.

## Cognito and synthetic onboarding (S7)

`module.auth` is off until the read-only inventory of the Step 4 sandbox pool is
recorded. Follow [auth-onboarding.md](auth-onboarding.md): inventory, enable the
synthetic pool (or import a matching one with a no-replacement plan), create two
synthetic users with passwords only in the scoped smoke secret, seed two orgs
through `er-onboard`, and validate tokens and cross-org denial through the deployed
router. It also records the staging-only MFA exception, cache lifetimes and the
emergency offboarding sequence. Local implementation/tests are available; none of
these AWS/onboarding acceptance steps has been run against staging yet.

## The foundation manifest

When the staging foundation root applies, it writes one JSON object:

```
s3://<deploy bucket>/envs/staging/foundation-manifest.json
```

Its authored payload contains network interfaces, audio/queue/database identifiers,
the data-key ARN, application task/execution role ARNs and runtime secret ARN references.
When auth is enabled it also contains Cognito pool/client/Region settings.
S8–S9 provide ECR, cluster, edge and log groups; apply/verify them before starting tasks.
**ARNs of secrets, never secret values.** The module validates that: a
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

## Automated staging releases (S11/S12)

Start with [the final AWS setup checklist](step8-final-aws-live-checklist.md).
It supersedes the earlier setup inventory and lists every external setting, secret,
fixture, GitHub variable and live test. This implementation has not deployed AWS.

`Deploy staging` follows successful `CI` for this repository's current `master`.
It rejects PR-triggered CI and superseded commits, pins the source SHA and holds
the same `aws-staging-mutation` concurrency group as the foundation workflow for
the entire release/smoke/recovery sequence. Runs queue; they do not cancel an active
migration. Terraform adds its own remote-state lock.

1. `scripts/deploy.py deploy` reuses an existing immutable commit tag on retry or
   builds/exercises/scans the candidate with `ci-image.sh`, publishes it once, and
   reads/scans the registry digest. It requires `linux/amd64`.
2. It snapshots the accepted predecessor, pins the current foundation-manifest S3
   version, and saves a release plan with `services_enabled=true`. The separate
   plan-time `deployment` output provides the migration template/network without
   applying any service changes.
3. It refuses to start while another task in the migration family is active,
   registers the exact candidate migration, checks RunTask failures, and waits up
   to 15 minutes for the exact task/container to stop with exit code zero. Timeout
   stops only that migration and fails. Task startup, missing exit code and nonzero
   exits leave services unchanged. The task ARN is printed as safe correlation.
4. It marks `rollout_started` before applying the saved plan. Terraform's circuit
   breaker is only the first check: the verifier also checks desired/running/pending
   counts, completed deployment, exact task revisions and container image digests,
   and healthy ALB targets matching both running API task IPs.
5. The `staging-smoke` job assumes a different role, reads only the synthetic
   credential/fixture, authenticates A/B/unprovisioned users and passes tokens on
   stdin to the Rust `remote_smoke` example. The example reuses `er-api` DTOs and
   `RemoteRepository`. No token appears in argv or a file; the checksum-verified
   synthetic WAV uses a private temporary directory and is removed on exit.
6. Only a successful smoke allows `accept` to update the versioned
   `envs/staging/accepted-release.json` pointer. Each attempt is recorded at
   `envs/staging/releases/<GitHub run>-<attempt>.json`, including source, digest,
   architecture, exact input config/manifest version, migration hashes and prior
   accepted release. Only one predecessor is embedded; S3 preserves record history.

Terraform/process diagnostics are captured to prevent accidental data disclosure
in hosted logs. On a fixed-label failure, inspect the restricted plan, AWS task
status and process log group with the operator role. Do not paste raw task or
Terraform output into a public issue.

### Rollback, interruption and failure exercises

The workflow's recovery job runs on deployment/smoke failure after a record was
created. A failed preflight/migration does not touch services. After rollout begins,
recovery checks schema compatibility, extracts the **previous accepted source
commit**, initializes its release root, and applies its exact recorded input config
and manifest version. It never runs a down migration. It then verifies the prior
digest and runs the same synthetic smoke through the separate smoke identity.
The candidate workflow remains failed even when recovery succeeds.

Automatic rollback is allowed when migration filenames/hashes are identical. A
schema-changing release requires a reviewed entry in
`ci/release-policy.json` → `rollback_compatible_migration_sets`:

```json
{"from": {"<previous migration filename>": "<sha256>"},
 "to": {"<candidate migration filename>": "<sha256>"}}
```

Each map must contain the **complete** migration set, exactly as recorded in the
release records. Approval means the old code can safely operate against the new
schema. An empty policy is deliberately the default. Without compatibility, or on
first-deploy failure with no accepted predecessor, the release root disables services
and records a stopped state for forward repair. If Terraform itself cannot stop or
restore resources, recovery fails visibly and the operator must close ingress and
inspect the state; do not report successful recovery.

GitHub cancellation or runner loss can interrupt even the recovery job. Before
retrying, inspect the attempt record, active/stopped migration tasks and live service
revisions. If needed, under approved release-equivalent SSO authority and the same
source commit, set the non-secret `STAGING_RELEASE_CONFIG` and original `RELEASE_ID`
and run:

```bash
scripts/deploy-staging.sh recover
# If recovery reports rollback=true, authenticate with the separate smoke identity
# and use the restored API origin/client ID plus fixture variables from the checklist:
scripts/demo-remote.sh
# Only after that smoke succeeded, restore release authority and record it:
scripts/deploy-staging.sh recovery-verified
```

Do not rerun `deploy` with an existing attempt ID. A new workflow attempt gets a new
record; completed SQLx migrations are idempotent, and active migration tasks block
another submission. A lost RunTask response is an inspection case, not permission to
start another task blindly. Re-running only the smoke job after recovery is unsafe;
rerun the complete CI/deployment sequence after investigating.

Rehearse failures only in synthetic staging with an operator present:

- Accept two distinct source commits first. Record both manifests/task definitions.
- In an isolated synthetic DB rehearsal, use the existing image checksum-mismatch
  procedure from `ci-image.sh`, or a reviewed failing migration candidate. Verify
  explicit failure and unchanged API/worker definitions. Never corrupt a production
  schema or use a down migration to restore accepted records.
- Use a reviewed staging-only candidate with an API startup failure (migration
  entrypoint must remain valid). Confirm the circuit breaker/explicit verifier
  rejects it and recovery restores the accepted digest/config; record rollback smoke.
- Stop one worker task during an identified synthetic job; allow replacement and
  inspect eventual completion/explicit failure and DLQ counts. Do not purge queues.
- Capture safe evidence in [step8-verification.md](step8-verification.md). Remove
  temporary failure code/settings and verify both roots converge afterward.

### Foundation changes and production

`Staging foundation` saves encrypted plans to a restricted, versioned S3 prefix.
An operator reviews the exact plan under SSO; a separate dispatch supplies its ID,
version and SHA256. Apply checks source commit and plan bytes and lets Terraform
reject stale state. The infrastructure environment must require review. Its role's
provisioning policy remains separately approved/manual; broad provisioning authority
was rejected by automatic review and is not included in this change.

The [production promotion contract](production-promotion.md) remains inactive and
receives no AWS credentials. Activation requires separate production authentication,
account controls, pre-PHI closure and digest-preserving promotion.

## Live acceptance still pending

| Section | Arrives with |
|---|---|
| Live database, queue and bucket acceptance | S5 configuration authored; S6–S9 dependencies and applied-resource evidence pending |
| Live IAM/DB/secret/rotation acceptance | S6 code and runbook authored; applied S7–S9 dependencies and probes pending |
| Live Cognito pool and synthetic clinician onboarding evidence | S7 procedure authored; applied pool, users and live checks pending |
| ECS services, the HTTPS edge, first release | S8/S11 code authored; applied-resource and hosted-release evidence pending |
| Log groups, alarms, and what is safe to log | S9 implemented locally; [live telemetry acceptance](telemetry-verification.md) pending |
| CI gates reproducing the local test suite | S10 implemented; [local evidence and GitHub activation](ci-verification.md) |
| Release, rollback, and the staging gate evidence | S11/S12 automation authored; [live verification record](step8-verification.md) remains unperformed |
