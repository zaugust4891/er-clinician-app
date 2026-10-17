# Phase 4 — Step 8 Implementation Steps (Terraform + CI/CD + First Staging Deploy)

*Written 2026-09-11 against the repository after the Step 7 implementation record dated
2026-09-10. Implements `phase4-implementation-guide.md` §8 and `phase4.md` §6/§9, with the
corrections below. This was an implementation plan; nothing was implemented by writing it.
S0-S3 have since been implemented; S4/S5 code and offline checks are now implemented — see the
[S3 implementation record](#implementation-record--s3-2026-09-14) and
[S4 implementation record](#implementation-record--s4-2026-09-14) and
[S5 implementation record](#implementation-record--s5-2026-10-09), authoritative for their
respective outlines. **2026-10-14:** approved S3 bootstrap is applied and remote-state
verification passes; current local gates and account/provider discovery are recorded in
the [dated audit](../ops/step8-audit-2026-10-14.md). S4/S5 application resources remain
unapplied pending budget and later dependencies. **2026-10-15:** S6 code/offline checks
are implemented; its live IAM/RDS/ECS gate is unperformed. See the
[S6 implementation record](#implementation-record--s6-2026-10-15). S7 code and local
checks are complete; its AWS acceptance remains pending in the
[S7 implementation record](#implementation-record--s7). S8 onward remain a plan.*

**Sources:** [Phase 4 playbook](phase4-implementation-guide.md), [Phase 4 plan](phase4.md),
[Step 3 Postgres guide](phase4-step3-postgres-guide.md),
[Step 4 implementation steps](phase4-step4-implementation-steps.md) and
[Cognito setup](phase4-step4-cognito-setup.md),
[Step 5 implementation steps](phase4-step5-implementation-steps.md),
[Step 6 implementation steps](phase4-step6-implementation-steps.md), and
[Step 7 implementation steps](phase4-step7-implementation-steps.md).
Where an older design disagrees with implemented behavior, the code and Step 7's implementation
record take precedence. External references are linked beside the decisions they support.

**Gate:** a reviewed commit produces one immutable application image; the pipeline migrates a
private staging database, deploys the API and worker, and proves the remote lifecycle using
Cognito, S3, SQS, Deepgram, and Bedrock with synthetic data. A failed migration cannot advance
the services, and a failed release has a rehearsed application rollback.

**What Step 8 is, in one sentence:** make the system that already works locally reproducible,
observable, and safe to operate across real cloud trust boundaries.

Step 8 delivers **synthetic-only staging and a production deployment path**. It does not declare
the product HIPAA compliant or authorize real patient use. The pre-PHI conditions in S0 and the
handoff remain binding even after the staging gate passes.

---

## Table of contents

- [0. Where Step 8 sits in the app's flow](#0-where-step-8-sits-in-the-apps-flow)
- [Implementation record — S3 (2026-09-14)](#implementation-record--s3-2026-09-14)
- [Implementation record — S4 (2026-09-14)](#implementation-record--s4-2026-09-14)
- [Implementation record — S5 (2026-10-09)](#implementation-record--s5-2026-10-09)
- [Implementation record — S6 (2026-10-15)](#implementation-record--s6-2026-10-15)
- [Implementation record — S7](#implementation-record--s7)
- [Implementation record — S8](#implementation-record--s8)
- [Current state audit](#current-state-audit)
- [Target file layout](#target-file-layout)
- [Concepts and decisions](#concepts-and-decisions)
- [S0 — Fix the deployment contract and data boundary](#s0--fix-the-deployment-contract-and-data-boundary)
- [S1 — Make cloud startup and resource use fail closed](#s1--make-cloud-startup-and-resource-use-fail-closed)
- [S2 — Package reproducible binaries and a real migration command](#s2--package-reproducible-binaries-and-a-real-migration-command)
- [S3 — Bootstrap Terraform state and establish ownership](#s3--bootstrap-terraform-state-and-establish-ownership)
- [S4 — Build the network and prove every required connection](#s4--build-the-network-and-prove-every-required-connection)
- [S5 — Provision encrypted storage, queues, and recoverable data](#s5--provision-encrypted-storage-queues-and-recoverable-data)
- [S6 — Separate IAM roles, database privileges, and secrets](#s6--separate-iam-roles-database-privileges-and-secrets)
- [S7 — Adopt Cognito and provision synthetic tenants](#s7--adopt-cognito-and-provision-synthetic-tenants)
- [S8 — Define ECS services and the HTTPS edge](#s8--define-ecs-services-and-the-https-edge)
- [S9 — Install privacy-aware telemetry and deployment alarms](#s9--install-privacy-aware-telemetry-and-deployment-alarms)
- [S10 — Make pull-request CI reproduce the local gates](#s10--make-pull-request-ci-reproduce-the-local-gates)
- [S11 — Wire OIDC deployment, migrations, and rollback](#s11--wire-oidc-deployment-migrations-and-rollback)
- [S12 — Perform the staging gate and hand off the remaining work](#s12--perform-the-staging-gate-and-hand-off-the-remaining-work)
- [Scope fence](#scope-fence)
- [Decisions and deviations recap](#decisions-and-deviations-recap)
- [External setup and first-deploy walkthrough](#external-setup-and-first-deploy-walkthrough)

---

## 0. Where Step 8 sits in the app's flow

Steps 3–5 established the durable event store, tenancy, sync contract, and reference client.
Step 6 moved audio to object storage and transcription to a worker. Step 7 moved summarization
and MDM there too. Step 8 deploys those boundaries; it does not create a new application model.

```text
Reviewed commit → CI → ECR image digest
                          │
                          ├─ one-shot er-migrate → private RDS
                          │                         │ success required
                          └─ Terraform release → API + worker revisions

Synthetic CLI ── Cognito access token ── HTTPS ALB/WAF ── HTTPS API
      │                                                    ├─ RDS: tenant-scoped records
      └─ presigned HTTPS PUT ── S3 audio                    └─ SQS: opaque job ids
                                  │                               │
                                  └──────── worker ◀───────────────┘
                                              ├─ Deepgram: raw synthetic audio
                                              ├─ Bedrock: minimized clinical text
                                              └─ RDS: transcript, summary, MDM, usage

CLI reload/poll ◀── existing event cursor / job routes ◀── API
```

There are two kinds of tests here. Local tests establish application behavior cheaply and
deterministically. Staging tests establish the things an emulator cannot: IAM/KMS permissions,
TLS verification, real Cognito tokens, provider model access, task replacement, and cloud log
behavior. Passing one does not imply the other passes.

**Build order:** S0 → S1–S2 → S3 → S4–S7 → S8–S9 → S10–S11 → S12. Terraform modules and CI
definitions can be authored before their dependencies exist, but live actions follow these
gates. Keep ECS workloads disabled and application ingress closed until the image, credentials,
migrations, and telemetry are ready.

S4–S9 describe the foundation components in build order, not a requirement to finish each live
check before authoring the next module. Apply the complete foundation (including ECR, cluster,
roles, and logs), publish the candidate image, then perform S6/S7's private bootstrap and live
checks. E3 describes that first-run sequence; S11 owns subsequent automatic migrations/releases.

**Effort:** plan on roughly 7–12 focused engineering days, plus procurement, account/DNS work,
and review lead time. This is a planning estimate, not a measured schedule. The original
3–4-day estimate omitted application deployment prerequisites, identity onboarding, secret
rotation behavior, and recovery validation. Re-estimate after S0 and the first clean image build.

---

## Implementation record — S3 (2026-09-14)

This section records what is in the working tree for **S3 only**. S0 produced
`docs/ops/deployment-contract.md`; S1 landed as the startup-safeguards/TLS/deadlines commit and
S2 as the `er-migrate`/pinned-image commit, and neither is detailed here. S4 onward are still
design.

Where this record and the S3 outline disagree, this record is authoritative.

**Nothing has been applied to an AWS account.** Every Terraform root is authored, formatted,
validated, and locked; no bucket, key, policy, or role exists in the cloud yet. The checks
below are the ones that can be settled without an account, and the live checks that remain are
listed at the end.

### What was implemented and where it fits

| Component | Files | Architectural role |
|---|---|---|
| Bootstrap root | `infra/bootstrap/{versions,backend,providers,variables,locals,guard,state,bucket_policies,iam,outputs}.tf` | Creates the one thing that must outlive every stack: the state bucket, its KMS key, the deployment-artifact bucket, and the per-root access policies. Ships with its `backend "s3"` block commented out because it creates the bucket its own state belongs in |
| State storage | `infra/bootstrap/state.tf` | One customer-managed KMS key with annual rotation; two buckets, both versioned, public access blocked, ACLs disabled (`BucketOwnerEnforced`), `prevent_destroy`, superseded versions kept 365 days (state) and 180 days (manifests), incomplete uploads aborted after 7 days |
| Bucket floor | `infra/bootstrap/bucket_policies.tf` | Denies plain HTTP, denies TLS below 1.2, and denies any upload whose encryption header overrides the KMS default with a weaker algorithm or a different key. A bucket policy binds every caller including an account administrator, so this is the floor no IAM policy can raise |
| Per-root access | `infra/bootstrap/iam.tf` | Five managed policies and five assumable roles, one pair per Terraform root. Each allows only its own state object, grants the `.tflock` object separately, and **explicitly denies** every other root's objects and every other environment's manifest |
| Environment roots | `infra/envs/{staging,prod}/{foundation,release}/` | Four roots, four distinct backend keys, `use_lockfile = true`, committed lockfiles. Foundation owns everything durable; release owns ECS services and task definitions only. Production is authored with `enabled = false` |
| Account guards | each root's `providers.tf` + `guard.tf` | `allowed_account_ids` refuses at the provider level; an STS `precondition` compares the live caller identity with the declared account and fails during `plan`, before any resource is created |
| Manifest module | `infra/modules/manifest/` | Publishes the non-secret foundation→release contract as one versioned JSON object, with variable validation that makes a secret value structurally unrepresentable |
| Manifest consumption | `infra/envs/*/release/manifest.tf` | Reads that one object, checks schema version, environment, account, region, and project, and fails the plan when a required resource key is missing |
| Offline gates | `scripts/tf-manifest-guards.sh`, `scripts/tf-lock-check.sh` | Prove the two claims that matter most — a manifest cannot carry a credential, and two concurrent operations cannot share a lock — without an AWS account |
| Version pins | `.terraform-version`, `infra/**/versions.tf`, `infra/**/.terraform.lock.hcl` | Terraform `>= 1.10.0, < 2.0.0` with the tested release (`1.10.4`) pinned separately; AWS provider `~> 6.0` resolved to `6.64.0` and locked for `linux_amd64`, `linux_arm64`, `darwin_arm64` |
| Artifact isolation | `.gitignore` | Adds crash logs, `backend.hcl`, `*.auto.tfvars`, override files, and CLI rc files to the existing state/plan/tfvars/`.terraform` rules. `.dockerignore` already excludes `infra/` through its source-only allowlist |
| Operator documentation | `infra/README.md`, `docs/ops/staging-deploy.md`, the S3 section of `docs/ops/deployment-contract.md` | The ownership map, the first-run runbook with the live checks spelled out, and the decision register with the values to record after the first apply |

### How the pieces connect

```text
infra/bootstrap                     state bucket (versioned, SSE-KMS, prevent_destroy)
  creates ───────────────────────▶    bootstrap/terraform.tfstate
                                      staging/foundation/terraform.tfstate
                                      staging/release/terraform.tfstate
                                      prod/foundation/terraform.tfstate
                                      prod/release/terraform.tfstate
                                        │  each key + its .tflock reachable by exactly
                                        │  one managed policy; all others explicitly denied
                                        ▼
envs/staging/foundation  ──publishes──▶ deploy bucket: envs/staging/foundation-manifest.json
  (network, data, identity, edge,       (versioned; version_id + checksum_sha256 as outputs)
   observability, ECR, cluster)                      │
                                                     │ reads one object, not a state file
                                                     ▼
envs/staging/release  ◀── image digest from CI ──  ECS services + task definitions only
```

The release root could have read the foundation's state with `terraform_remote_state`. It does
not, for two reasons. A state read hands the reader everything in the file, including values
that are only there because Terraform had to store them; a manifest is an explicit allowlist and
the release role's policy can be scoped to one object key. And because the artifact bucket is
versioned, a release can pin the exact manifest version it was approved against, which a live
state read cannot offer.

### Necessary deviations and corrections

**`infra/modules/manifest/` is not in the proposed target file layout.** That layout names
network, data, auth, identity, edge, observability, and compute. The manifest writer is called
by both foundation roots, which is the "reusable boundary" test the layout section sets, and the
alternative was duplicating the same twenty lines in two roots. It is a module.

**The deployment-artifact bucket lives in `bootstrap/`, not in the foundation root that writes
to it.** S3 task 6 requires the manifest to be persisted in a versioned, access-controlled
object; task 2 requires protected storage to stay out of the lifecycle of a disposable staging
stack. A staging foundation is explicitly allowed to be destroyed and rebuilt, and the release
pipeline's contract is not, so the bucket is bootstrap-owned and `prevent_destroy`.

**One KMS key encrypts both buckets rather than one key each.** Both hold deployment metadata
rather than clinical content, and the staging/production separation is enforced per object in
IAM, not per key — a role with `kms:Decrypt` still cannot `GetObject` a prefix its policy
denies. This saves one key's monthly charge against a `$50` budget. Split it if artifact
consumers multiply.

**Bootstrap also creates five state-operator roles, not only policies.** Task 2 asks for
narrowly scoped principals and the "Done when" requires proving staging credentials cannot
reach production state, which needs an assumable identity to test with. These are state-access
roles for operators; S6 and S11 still own the ECS task, execution, and CI roles, and the
managed policies are standalone so those roles can attach them. Trust defaults to account
delegation, narrowable to exact Identity Center permission-set ARNs through
`state_operator_principals`.

**`required_version` is a floor, not an exact pin.** The constraint is
`>= 1.10.0, < 2.0.0` and the tested release is pinned in `.terraform-version` (`1.10.4`), which
is what CI installs. An exact `required_version` in five files makes a routine upgrade a
five-file change for no added safety, since the lockfile and `.terraform-version` already fix
what actually runs. **Note that S3 native locking is experimental in 1.10 and generally
available in 1.11**; upgrading is recommended and is a one-line change to `.terraform-version`.

**The expected account id cannot be committed as a reviewed constant.** The repository's
pre-existing `.gitignore` rule `**/*.tfvars` blocks it, so each root ships a
`terraform.tfvars.example` instead and the guard compares operator-supplied input against the
live STS identity. This catches the realistic failure — running a staging root while holding
production credentials — but not an operator who deliberately supplies the wrong expected
account. Directory and backend-key separation covers the rest.

**Updated 2026-10-09: encryption denies combine `Null = false` with `StringNotEquals`
for the same header.** Both buckets default to SSE-KMS. Each deny therefore matches
only a present, wrong override, leaving headerless uploads eligible to use the default.
The original `StringNotEqualsIfExists` shape also denied missing headers in a Deny
statement and was corrected in bootstrap and the S5 audio policy. Correctly signed
live uploads and actual backend state/lock writes remain acceptance checks.

One consequence is worth knowing before the first apply: `-backend-config="kms_key_id=..."`
must be the key **ARN**, not `alias/ernote-tfstate`. An alias produces a header that does not
equal the ARN, so the deny fires on the first state write rather than at init. The runbook says
so at the step where it matters.

**`s3:ListBucket` on the state bucket is granted unconditioned.** The S3 backend lists the
bucket during `init`. Listing exposes key names only, and the separation that matters is
enforced on object actions with an explicit `Deny`, which beats any `Allow` attached to the
same role later.

**Two scripts were added under `scripts/` that the target layout does not name.** The layout
proposes `ci-services.sh`, `deploy-staging.sh`, and `demo-remote.sh`, all of which belong to
S10 and S11. `tf-manifest-guards.sh` and `tf-lock-check.sh` exist because two of S3's own "Done
when" criteria are testable today and would otherwise be asserted rather than demonstrated.

### Verification performed

Everything below ran on 2026-09-14 against the working tree, with no AWS account and no cloud
resource created.

- `terraform fmt -check -recursive infra/`: clean.
- `terraform validate` on all five roots after `init -backend=false`: valid.
- `terraform providers lock -platform=linux_amd64 -platform=linux_arm64 -platform=darwin_arm64`:
  `.terraform.lock.hcl` present and committed for all five roots, AWS provider `6.64.0`.
- `scripts/tf-manifest-guards.sh`: 7 of 7. A raw secret value under `secret_refs`, a non-Secrets
  Manager ARN, a `db_password`-style resource key, a `deepgram_api_key`-style resource key, an
  empty resource value, and an unknown environment are each rejected by variable validation; a
  genuine Secrets Manager ARN reference is accepted.
- `scripts/tf-lock-check.sh` against the Compose MinIO container: a second concurrent operation
  on the same state key is refused with `Error acquiring the state lock` and HTTP `412
  PreconditionFailed`. This exercises the real mechanism — Terraform 1.10's conditional-write
  `.tflock` object — against an S3-compatible endpoint. It is not a substitute for the same
  check against the real bucket, which is in the runbook.
- `git add -An` over the new tree: 63 files, and no `.terraform/` directory, state file, plan,
  or `terraform.tfvars` among them. `git check-ignore` confirms `.terraform.lock.hcl` and
  `terraform.tfvars.example` are **not** ignored while `terraform.tfvars`, `backend.hcl`,
  `.terraform/`, `crash.log`, and `*.tfstate` are.
- Greps over `infra/`: no `ignore_changes`, no `dynamodb_table`, no `force_destroy`, and no
  credential-shaped assignment anywhere in the tree.
- A separate throwaway root confirmed that HCL's conditional operator short-circuits, so the
  production roots' `var.enabled ? jsondecode(data.aws_s3_object.manifest[0].body) : null`
  does not evaluate an index into a zero-count resource. This is what lets both prod roots plan
  while unprovisioned.

### Live checks that remain

These need the staging account and are written out step by step in
[`docs/ops/staging-deploy.md`](../ops/staging-deploy.md); their status rows are in the S3
section of [`docs/ops/deployment-contract.md`](../ops/deployment-contract.md).

| Check | S3 "Done when" clause it closes |
|---|---|
| Bootstrap applies; its state migrates to the protected remote key; `plan` then reports no changes; local state and backup are destroyed | Migrate initial local bootstrap state and verify the remote copy |
| Plain HTTP denied, public access blocked, versioning enabled, an `AES256` upload denied | State storage protections |
| A prior version of a disposable state object is recovered and its versions cleaned up | A prior version of a disposable state object can be recovered |
| Two real concurrent operations collide on the same key in the real bucket | Two simultaneous state operations cannot both acquire the same lock |
| The staging operator role is denied production state and the production manifest, and the reverse | Staging credentials cannot read/write production state |
| A root run with a mismatched `aws_account_id` fails before resource creation | Wrong-account execution fails before resource creation |
| MFA delete decision on the state bucket (requires the account root user; not expressible in Terraform) | Recorded in the deployment contract either way |

---

## Implementation record — S4 (2026-09-14)

S4 is implemented in source and verified with mocked Terraform providers. **No AWS
resources were created by the S4 implementation and the live S4 connectivity gate is
not complete.** S5 data stores are now authored; S6–S9 still supply scoped roles,
diagnostic execution infrastructure, actual HTTPS server listener, edge and logging
needed for those checks.

- `infra/modules/network/` defines a DNS-enabled IPv4 VPC, six disjoint subnets over
  two AZs, explicit public/task/isolated-DB route tables, one staging NAT, optional
  per-AZ NAT, and a two-AZ DB subnet group. The default SG is emptied.
- Dedicated ALB/API/worker/migration/onboarding/DB SGs implement the connection matrix.
  ALB client ingress defaults closed, backend TLS port is 8443, database access is
  5432 by SG reference, and task egress is TCP 443. SGs do not verify TLS or filter
  hostnames; S5/S6/S8 must enforce RDS TLS and service public-IP settings separately.
- An S3 gateway endpoint covers both task route tables and permits regional ECR
  image-layer reads. S5 supplies actual audio bucket ARNs before application use.
  No VPC-only bucket deny is installed, preserving external presigned PUTs.
- All six interface endpoint switches exist with private DNS, per-service SGs and
  scoped actions/resource ARNs. ECR switches must be paired. Only ECR's authorization
  action uses a wildcard resource; logs permit trailing stream scope. NAT remains.
- Both foundations call the module; production is gated by `enabled = false`.
  Module outputs feed foundation consumers and the manifest's task interfaces.
  The existing `map(string)` manifest requires JSON-encoded subnet lists and a
  string port; release guards now require those keys. Publication waits for routing
  and endpoint creation as well as resource IDs.
- `scripts/tf-network-check.sh` runs isolated mocked tests from committed source and
  provider locks, excluding operator state and tfvars. Its 17 test runs passed
  (13 module, 2 staging, 2 production), covering network/security behavior, invalid
  inputs, manifest wiring, account mismatch and production gating. All five roots
  validate; Terraform formatting and shell syntax checks pass.
- Outbound provider/JWKS TLS defaults and URL ownership were audited in source.
  Real positive/negative certificate tests remain in the live gate, not claimed from
  source inspection. Incoming API TLS is deliberately still S8's work.

The DB subnet group lives in network because S4 owns placement; S5 must reuse it.
One-shot migration and onboarding use distinct SGs, preserving separate future task
roles. An explicit `network` input object keeps environment roots aligned while
requiring actual ARN inputs for optional endpoints; it does not guess future
resource names. The ordered AZ pair should be pinned before apply.

Runbook, cost assumptions and pending evidence are in
[`docs/ops/network-verification.md`](../ops/network-verification.md), with module
interfaces in [`infra/modules/network/README.md`](../../infra/modules/network/README.md).
The estimated Ohio network idle cost is $36.50/month at 730 hours, before traffic
and later infrastructure; budget review remains open. Paid interface ENIs are
counted per service **and per AZ**. No AWS connectivity or billing evidence exists yet.

---

## Implementation record — S5 (2026-10-09)

S5 is **authored and tested locally; live acceptance remains pending**. Source landed
in `3698415`; the October 9 follow-up commits corrected encryption conditions,
production retention guards and tests. No cloud apply is recorded. See the
[review checklist](../ops/step8-s1-s5-review-checklist.md) for dated evidence and open gates.

- `infra/modules/data/` defines private encrypted PostgreSQL 16, capped gp3 storage,
  enforced server TLS, backups, deletion protection and final-snapshot settings.
- The audio bucket has public-access blocks, disabled ACLs, versioning, default KMS
  encryption/Bucket Keys, separate current/noncurrent expiry and multipart cleanup.
  Both audio and bootstrap encryption denies require a present, incorrect header.
- Standard job and dead-letter queues use KMS, TLS restrictions, 300-second visibility,
  20-second polling, three deliveries and separate 4/14-day retention defaults.
- Both foundations wire the data/network/manifest interfaces. Release guards require
  the application-facing S5 keys; the release roots still contain no ECS services.
  Runtime IAM grants, secret population and database logins remain S6 work.
- Production requires explicit Multi-AZ, backup retention and both audio-retention
  values before enablement. The example keeps production disabled and audio values
  unset; actual retention and legal-hold approvals remain pending.
- The latest operator-provided `scripts/tf-data-check.sh` run passed **24 mocked tests**:
  12 data module, 4 staging foundation and 8 production foundation. Foundation tests
  overlap the network script, so script totals must not be added as unique cases.
- The Sonnet fix at `6845878` passed 19 Bedrock-enabled AI tests, 18 tests without the
  Bedrock feature (overlapping coverage), six config gates, two worker tests, workspace
  formatting and Clippy. AWS account access and source-Region price verification are open.

Live acceptance requires identity/budget/RDS discovery, the complete S6–S9 foundation,
correctly signed uploads and KMS grants, runtime permission-denial probes, backup
progress and per-subnet connectivity/TLS evidence. A fresh image/registry digest and
remote state/lock/recovery evidence remain pending. Audio version/checksum binding
and a restore rehearsal remain Step 9 requirements before patient use. Costs in the
older runbooks are planning assumptions, not current bills or repriced estimates.

The [data module README](../../infra/modules/data/README.md) and
[data verification runbook](../ops/data-verification.md) describe interfaces and live gates.

---

## Implementation record — S6 (2026-10-15)

This record is authoritative for S6's implemented code. **No S6 resources were
applied and no AWS/RDS/ECS acceptance pass is claimed.** The approved bootstrap
remains the only applied stack; the budget decision and S7–S9 resources still gate
application provisioning and live probes.

The agent-loop artifacts under `.agents/runs/*step8-S6` show three applied edits:
cloud DB connection parts, runtime privilege convergence/startup checks, and an
unwired identity module. Later runs stopped on a usage limit and stale patch hunks;
the latest recovered bootstrap review was never applied. This implementation adapts
that draft to the current files and completes the missing integration.

| Component | Implemented behavior |
|---|---|
| `infra/modules/identity/` | Distinct task/execution roles, account/Region-scoped ECS trust, actual S3/SQS/Bedrock actions, exact-context KMS use, five value-free encrypted secret containers and secret read policies |
| Foundation/data integration | Both roots call identity; disabled production creates none. A separate data-key policy resource merges service grants without a key/role cycle; it is the sole policy owner |
| Release contract | API/worker/migration role ARNs and four secret pointers are published and required by release guards. Master/bootstrap/onboarding references stay in operator outputs |
| `db.rs`, config and `privileges.rs` | Cloud DB parts with verified TLS; runtime refuses administrative/owning/CREATE-capable logins; owner-run grants clear stale table, column, sequence, routine and default ACLs and apply an explicit per-relation allowlist |
| `er-db-bootstrap` | Private one-shot master operation creates/rotates owner, API, worker and onboarding logins plus three NOLOGIN groups; validates privileges before commit and verifies authentication afterward; sends SCRAM verifiers instead of plaintext SQL passwords |
| `scripts/db-secrets.py` | Stages generated logins through AWS CLI stdin, saves only version metadata, renders pinned ECS secret references and promotes only after the successful task/digest/definition matches the rotation; interrupted operations can retry the same record |
| CI templates | Optional staging infrastructure/release/smoke roles with exact repository/environment OIDC trust, separate state policies and narrow deployment/PassRole or synthetic-secret use. Default is null; S11 still attaches the final reviewed infrastructure provisioning policy and implements protected workflows/production promotion |
| Packaging and runbook | The shared image now carries four binaries; [identity verification](../ops/identity-verification.md) defines the private bootstrap, maintenance rotation/recovery and live acceptance matrix |

**Decisions/corrections:** onboarding receives its own DB login, with authority only
on orgs/clinicians. First bootstrap precedes first migration. New objects get no
runtime privilege until classified; defaults are revoked for the actual schema
owner. API bucket listing preserves missing-upload semantics and adds no listing
route. Secret values never enter Terraform. Bootstrap task definitions pin login
version IDs; their execution role permits those secret ARNs without a VersionStage
condition because ECS cannot select version ID and stage together. Runtime injection
remains AWSCURRENT. A timeout after credential commit may leave staged passwords
applied; recovery retains the record and reruns the same credentials before promotion.
Rotation uses a stopped-workload maintenance window; alternating-user rotation is
still Step 9 work.

**Validation:** Rust format, workspace Clippy/tests, mocked data/identity/foundation
and network gates, and credential-helper regressions are the local gates. They cover
positive runtime queries, denied DDL/history changes, narrow onboarding, stale ACLs,
unsafe bootstrap rollback, rejected old passwords, exact-version promotion and CI
trust/PassRole boundaries. The [verification record](../ops/identity-verification.md)
distinguishes offline evidence from unperformed actual-role encrypted operations,
secret injection/logging, Bedrock invocation, RDS behavior and ECS rotation.

---

## Implementation record — S7

**Status (2026-10-15): code and local validation complete; live acceptance pending.**
This pass finishes S7 implementation before AWS setup. It creates no pool, user,
secret or smoke role and claims no live `/v1/me` or cross-org pass. The October 14
audit discovered the sandbox pool; the detailed S7 adoption inventory is still
pending. Local gates do not substitute for the live criteria below, which need
an authorized operator with staging AWS access following
`docs/ops/auth-onboarding.md`:

| Live acceptance item | Evidence |
|---|---|
| Read-only sandbox inventory and recorded import-or-new decision | Pending |
| Pool/client applied with a reviewed no-replacement plan | Pending |
| Two synthetic users created; passwords only in the smoke secret | Pending |
| `er-onboard` apply against the staging DB: `created, created` | Pending |
| Re-run reports `unchanged, unchanged`, no added rows or transfers | Pending |
| Both real access tokens: deployed `/v1/me` 200 with distinct expected orgs | Pending |
| Expired, wrong-pool, wrong-client, ID, dev, missing-bearer, unknown-`sub`: all 401 | Pending |
| Cross-org patient/event/snapshot/audio/job/export reads and writes: same 404 as absent ids | Pending |

Implemented code:

| Component | Implemented behavior |
|---|---|
| `crates/er-server/src/auth/keys.rs` | JWKS fetch is HTTPS-only (`from_url` refuses other schemes), follows no redirects, has a 5 s connect/whole-request timeout and a 64 KiB streamed body cap, and rejects malformed or empty key sets and any set containing a key that is not a valid RSA public key usable for RS256 signatures (`oct`, `use: enc`, another declared `alg`, a missing/empty `kid`, a modulus under 2048 bits or even, or an exponent that is even, below 2 or out of `rsa` crate bounds is refused) before it can replace the cache. One refresh runs at a time; concurrent misses wait for it and re-check the cache. At most one attempt per 30 s cooldown, successful or not; unknown `kid`s inside the window fail from cache. A failed fetch keeps last-known-good keys and never admits the unknown key |
| `crates/er-server/src/auth/mod.rs` | Distinct `AuthError` variants for JWKS status, size, parse, empty-set and unusable-key failures (all still 401 at the middleware) |
| `crates/er-server/src/onboarding.rs` | Privileged membership path run as the onboarding login. Validates canonical lowercase UUID subjects, bounded printable names, and a `Synthetic ` prefix outside production; one subject per request. Applies all entries in one transaction: creates orgs/clinicians, re-runs are `unchanged`, same-org display-name changes are the only update, and an existing subject bound to another org or an org id with a different name refuses and rolls back the whole request. Revocation rebinds the subject to `revoked:<sub>` (no token can carry it) and keeps the row and its org for history; a later request for a revoked subject is refused (`subject_revoked` for the same org, `subject_bound_to_another_org` otherwise), so re-seeding neither re-grants nor moves it. Apply and revoke transactions serialize on one advisory lock |
| `infra/envs/staging/foundation/{auth,identity,locals,variables}.tf` | `var.auth` (default disabled) gates `module.auth[0]`; when enabled the manifest gains `cognito_pool_id`, `cognito_client_id`, `cognito_region`, and the identity module's onboarding role gets `AdminGetUser` on that pool. Production root unchanged. `scripts/tf-network-check.sh` now copies the module so the root initialises |
| `docs/ops/auth-onboarding.md`, `docs/ops/staging-deploy.md` | Runbook: read-only sandbox inventory and import-or-new decision, pool enablement, synthetic users with passwords only in the scoped secret, `sub` lookup and `er-onboard` request, smoke access-token flow, recorded staging MFA exception, cache lifetimes, emergency offboarding sequence. Not executed |
| `infra/modules/auth/` | New module, composed only by the staging root. Cognito pool with self-signup off, `admin_only` recovery, mandatory TOTP MFA, no custom attributes (no `custom:org_id`), `deletion_protection = ACTIVE` and `prevent_destroy`. Secretless native client with SRP and refresh flows, explicit 60 min access/ID and 12 h refresh lifetimes (bounded by validation), token revocation, `prevent_user_existence_errors`. Hosted login is off unless exact `callback_urls` are given, then code flow only; wildcard and plain-http URLs are refused. `synthetic_smoke_exception` (staging-only by validation) makes MFA optional, adds `ALLOW_USER_PASSWORD_AUTH`, and creates an empty `<name>/synthetic-smoke-credentials` secret under a dedicated rotating KMS key for the identity module's `ci.smoke_*` inputs. No users or passwords in Terraform |
| `config.rs`, `src/bin/onboard.rs`, `Cargo.toml`, `Dockerfile` | `OnboardingConfig` takes `ONBOARDING_ACTION=apply\|revoke` and a 1–100 entry `ONBOARDING_REQUEST` JSON list that refuses unknown fields; errors never echo values. The `er-onboard` one-shot binary prints only per-entry outcomes (`created/unchanged/renamed/revoked/absent`) and a fixed status/reason, bounded by a 60 s timeout |

**Tests:** `keys.rs` covers real key rotation to an independently generated RSA
pair (new token verifies, a retired-key signature relabeled with the new kid
fails, the retired kid is refused), an unavailable (503), empty, malformed-RSA,
kid-less, invalid-RSA-parameter (zero/1/even exponent, short or even modulus) or
incompatible (HMAC, encryption-use, RS512) JWKS keeping cached keys, including
through the refresh cooldown, 200 concurrent
random kids causing exactly one upstream fetch, refused redirects, oversized and
malformed bodies, and a slow endpoint bounded by the timeout. `onboarding.rs`
runs as the converged onboarding grant: two orgs seeded once and resolved to
distinct orgs, idempotent rerun with no added rows, refused transfer with full
rollback, org-name mismatch, rename, revocation denial for a fresh resolver, a
revoked subject refused both re-grant to its org and transfer to another (no
second row, tombstone keeps its org), and input validation. `tests/config_gate.rs` covers the onboarding request gate.
`infra/modules/auth/tests/auth.tftest.hcl` (mocked plans) pins the default pool
and client settings, code-only hosted login with exact redirects, the scoped
staging exception, and refuses the exception in production, wildcard and
plain-http callbacks, logout URLs without hosted login, and over-long tokens.
The pool's schema comes only from `local.custom_attributes`, which the test pins
empty (a computed schema is unknown at mocked plan time); a pool postcondition
also refuses any `org` attribute on the real pool after apply. The module is
validated and tested by `scripts/tf-data-check.sh` against the staging lock.
`tests/cognito_token_gate.rs` runs the real router in `AuthConfig::Cognito` mode
(synthetic pool/client, dev key standing in for the pool key): two access tokens
reach `/v1/me` with distinct orgs and clinician ids; expired, other-pool issuer,
other-pool key under a colliding kid, wrong or missing `client_id`, ID token,
missing `token_use`, dev-issuer token, unknown subject, garbage, missing bearer
and non-bearer scheme are each 401 against a 200 positive control. Org B's
patient is seeded with real events and an owner-stored snapshot, so note,
events, snapshot and export are each 200 for org B; for org A each returns the
same 404 as a never-existed id. A cross-org snapshot PUT is the same 404 as one
to an absent id and leaves org B's checkpoint unchanged. An org-B job is 200 for
its owner and 404 for org A. An existing audio row is readable by its owner;
foreign status/completion calls match absent IDs, including response bodies.
Valid cross-org event/audio/MDM writes and shift reads/end requests return the
same 404 as absent resources without changing events, chunks, jobs or the owner's
active shift. Offboarding is exercised through the router: the old task's cached
membership still works, while a fresh task denies the same unexpired token.
These tests pin the Cognito token contract with local stand-in keys, not live Cognito.

The staging root's `data.tftest.hcl` asserts auth is off by default (no
`cognito_*` manifest keys) and that a plan with `auth.enabled` creates the module
and publishes all three keys. The root's non-secret `auth` output (pool/client ids,
issuer, smoke secret and KMS key ARNs) is null by default, carries the smoke ARNs
when enabled (the mocked plan cannot populate ARNs, so the test pins the plan-known
`synthetic_smoke_secret_name` and the output's keys), and has a null smoke secret
name and ARNs without the exception. The runbook reads `SMOKE_SECRET_ARN` from that output, disables shell
tracing before generating or setting passwords and keeps it off through smoke
authentication, and captures the smoke access token directly into a variable, so
neither passwords nor tokens are printed.

**Final local closure (2026-10-15):**

- JWKS startup/refresh rejects duplicate key IDs. The regression proves that an
  ambiguous response preserves the last-known-good keys and consumes the refresh
  cooldown, so it cannot replace the cache or trigger unlimited refetches.
- API config rejects empty/malformed pool suffixes and pools in another Region
  before JWKS I/O. The staging release root requires all three Cognito settings
  and rejects absent/empty IDs and pool/Region mismatch. Five mocked release
  consumer cases are included in `scripts/tf-data-check.sh`.
- `cargo test --workspace --locked` passed **338 tests**, with zero failures or
  ignored tests. After the final configuration validation change, the focused
  `config_gate` suite passed **10 tests**, including the new malformed-pool case.
- Workspace format and `cargo clippy --workspace --all-targets --locked -- -D warnings`
  passed. Terraform format and the data/auth/identity/foundation/release gate passed
  **48 mocked runs** using the installed, locked AWS provider 6.64.0 in temporary
  backend-disabled copies. The network/foundation regression gate passed **27
  mocked runs** (foundation cases overlap with the data gate). These results do
  not demonstrate AWS authorization.

Rust tests use local PostgreSQL/MinIO/ElasticMQ and synthetic credentials. For
reproduction, start the Compose stack and set only its development credentials:

```bash
DATABASE_URL=postgres://postgres:dev@localhost:5433/ernote \
AWS_ACCESS_KEY_ID=dev AWS_SECRET_ACCESS_KEY=devdevdev \
AWS_REGION=us-east-1 AWS_EC2_METADATA_DISABLED=true \
cargo test --workspace --locked
./scripts/tf-data-check.sh
./scripts/tf-network-check.sh
```

Private ECS templates and deployment/smoke workflow activation remain S8/S11.
The auth module remains disabled by default and production has not been enabled.

**Limitations / remaining S7 work:** `ci.smoke_*` stays an operator input fed from
the auth outputs; the read-only sandbox inventory, apply, user provisioning,
onboarding runs and every live check in `docs/ops/auth-onboarding.md` are
unperformed, so no real Cognito token has reached `/v1/me`. A running API task keeps a
cached membership after revocation until its cache entry expires or the task is
replaced; Cognito user disablement does not revoke already-issued access tokens.

---

## Implementation record — S8

**Status: implementation complete; live acceptance pending.** All S8 resources,
runtime changes and local gates are authored. Nothing has been applied or exercised
in AWS for S8. Services and ingress remain disabled by default. S9 telemetry and
S11's migration/release workflow are prerequisites to activation. See
[compute/edge verification](../ops/compute-edge-verification.md) for exact inputs,
operator templates and the remaining live assertions.

**Repository and cluster (task 1).** `infra/modules/cluster/` creates the ECR
repository `<project>-<environment>` (the name S6's execution roles already scope
pulls to), with immutable tags, scan-on-push, `force_delete = false`, and a
Fargate-only cluster (Container Insights off until S9). Retention relies on a
tag convention S11 must follow: builds push `sha-<sha>`, and every image is
tagged `release-<sha>` before any task definition references it. No tagged
image ever expires automatically. A lifecycle rule's only action is expire, so
any rule over tagged images has a threshold at which it can delete the running
or rollback image. The policy's single rule expires untagged images 7 days after
push, and a precondition rejects any rule that selects tagged images. The build
must push a single-platform manifest with no index or attestation children.
Tagged images accumulate until reference-aware cleanup exists; storage cost is
accepted. (Review fixes: a 20-image count window, and then a 10,000-count
"shield" rule, each still had an eviction threshold.) Wired into staging
(`cluster.tf`) and the disabled production
root, which publish `ecr_repository_url`/`ecs_cluster_arn` in the manifest.
The target layout's single `compute/` module is split: `cluster/` is the
foundation half, and release services live in `compute/`, with shared task JSON
in `task/`.
Tests: `infra/modules/cluster/tests/cluster.tftest.hcl` (repository settings,
single untagged-only rule, Fargate-only). The `tests/simulate/` helper evaluates
the policy JSON as ECR documents it. With 200 failed promotions, a
4,000-day-old accepted image, a 40,000-day-old rollback target and stale
candidates, only the stale untagged push expires. A control run shows that a
count rule over released images evicts both protected images. The staging
foundation test asserts the name and manifest keys, and the disabled
production test asserts no cluster. Both Terraform check scripts copy the module.

**Backend-hop TLS (task 7).** `crates/er-server/src/tls.rs` generates a 2048-bit
RSA key and a minimal self-signed X.509 v3 certificate in memory at startup
(no key file, no sidecar) and serves it through a `TlsListener` for
`axum::serve` (TLS 1.3 and 1.2, ring provider, 10 s handshake timeout, at most
256 handshakes in flight; failed handshakes are dropped without logging peer
input). The certificate is built from already-locked crates (`rsa`, `sha2`,
`rustls`, `tokio-rustls`), so no new crate entered `Cargo.lock`. `API_TLS`
(`config.rs`) defaults to `self-signed` outside dev and rejects `off` there;
dev defaults to plain HTTP and may opt in. **Security-review assumption:** the
ALB does not validate target certificates, so this is encryption only; who may
reach port 8443 is decided by target registration, the task SG and IAM, not mTLS.

**Readiness (task 8).** `/healthz` stays a dependency-free liveness probe.
`/readyz` (`routes.rs`, `AppState::ready` in `state.rs`) does a pool checkout,
`SELECT 1`, and checks every API table exists and is readable by the current
login (`has_any_column_privilege`, so unconverged grants also fail), all within
3 s. It returns static `{"status":"ready"}` / 503 `{"status":"unavailable"}`;
the cause is logged as a static reason only. JWKS/queue/TLS initialization
completes before the port is bound (`main.rs`), so a task that cannot start
never answers a probe. Deepgram/Bedrock are never called.

**Caching.** Every router response, including 401/404, carries
`Cache-Control: no-store`. The 2 MiB body cap and static error shapes are unchanged.

**Tests.** `crates/er-server/tests/edge_gate.rs`: readiness 200 → 503 after the
pool is closed while liveness stays 200; dropping `jobs` fails readiness;
`no-store` on 401/404; a pinned-certificate client (signature still verified)
completes TLS 1.3 and TLS 1.2 handshakes and receives `/readyz`, plaintext HTTP
to the TLS port gets no HTTP response, and two identities differ.
`config_gate.rs` covers `API_TLS` defaults and rejections; `tls.rs` unit tests
cover DER length encoding and UTCTime/GeneralizedTime selection.

**Services and private tasks (tasks 2–5).** `infra/modules/compute/` renders API,
worker and migration JSON from one pinned digest and the versioned manifest.
`services_enabled` defaults false; templates are available for migration before
activation. API desired count 2 rolls at 100/200 with AZ rebalancing; worker count 1
rolls at 0/100 with rebalancing disabled. All tasks are Linux/X86_64 Fargate (services
pin platform 1.4.0), 512 CPU/1024 MiB, non-root, capabilities dropped, read-only root,
no scratch mounts, no public IP, and no ECS Exec. Worker stop timeout 120 exceeds its
100-second drain; API stop 60 exceeds its 30-second request timeout and 45-second
ALB deregistration. Pool caps are 5/5; maximum normal rolling service demand is 25
connections, with 10 more reserved for one-shots/admin. A smaller available budget
is rejected. Previous service task revisions stay active for rollback.

`infra/modules/task/` shares those container restrictions and exact binary paths.
Foundation exposes digest-pinned bootstrap/onboarding JSON and private network
configuration only through operator outputs. Bootstrap has no task role, uses the
master/bootstrap execution boundary, and must pass through `db-secrets.py render`
to pin staged credentials. Migration is a one-shot template, never an implicitly
executed Terraform provisioner or an ECS service. The API gets its own DB secret;
worker gets its own DB plus Deepgram. Role, repository, cluster, target group,
secret and log-group scopes are checked against the environment. Actual S9 log
names are required before rendering a release candidate. Production stays disabled;
its separate production Cognito adoption/wiring remains a prerequisite.

**HTTPS edge (tasks 6, 9–10).** Foundation's optional `edge` creates regional ACM DNS
validation, the managed Route53 alias, public ALB TLS 1.2/1.3, HTTPS IP targets and
regional WAF. Listeners wait for WAF association; the target-group output waits for
the listener so ECS cannot attach prematurely. `edge = null` creates nothing, and
`ingress_enabled = false` blocks all requests at WAF. Foundation rejects client
CIDRs without the explicit edge ingress gate. HTTP is absent unless explicitly
configured to redirect. No runner-IP discovery or automatic allowlist widening.

WAF enforces Common Rule Set and an initial 2,000 requests/300 seconds/IP rule,
with request sampling/raw logs disabled. ALB body inspection is limited to 8 KiB.
Only the managed size rule is replaced: a nonterminating label recognizes exact
JSON POST event batches and PUT snapshots; a later rule blocks other oversize bodies.
Other managed/rate rules still evaluate these sync requests. The application keeps
the 2 MiB cap. The native API client now sends a static versioned User-Agent to
satisfy Common Rule Set's missing-header rule without identifiers. All router
responses, including outer request timeouts, receive `no-store`.

**Additional local verification.** Terraform tests cover edge TLS/readiness/WAF,
closed ingress and explicit opening, immutable task JSON, role/secret scopes,
private placement, rollout settings, disabled activation, pinned manifest versions,
missing telemetry rejection and connection budgets. Runtime tests also exercise
pool exhaustion with a four-second outer deadline (readiness returns in three),
a valid 32 KiB event body and a rejected >2 MiB body, plus the client User-Agent.
Exact final check results are recorded in the linked verification document.

**Limitations.** No live ALB has negotiated with this listener; ALB backend cipher
compatibility with ring's ECDHE-RSA-AES-GCM suites remains unverified. Live ACM/DNS,
WAF false positives, Fargate non-root/read-only memory limits, DB budget, worker
heartbeat/drain, event preservation under replacement and rollback must pass before
S8 live acceptance is complete. S9 owns telemetry; S11 activates reviewed releases.

---

## Current state audit

This is the original **2026-09-11 baseline**, retained to explain the implementation
plan. The S3–S5 records above describe subsequent infrastructure work, and the
[review checklist](../ops/step8-s1-s5-review-checklist.md) records the October 9 fixes
and checks. Rows describing missing infrastructure or old startup behavior are
historical observations, not the current implementation status.

These are repository observations, not claims that cloud resources were inspected. No AWS
account, signed agreement, GitHub setting, live provider quota, or hosted-zone ownership was
verified while drafting this guide.

| What exists now | Consequence for Step 8 |
|---|---|
| Eight workspace crates; `er-api` owns DTOs and the `client`-gated `ServerApi`/`RemoteRepository` | Reuse that client for smoke tests. Do not put another remote repository in `er-cli`. |
| No `infra/`, `.github/workflows/`, or `scripts/demo-remote.sh` in the inspected tree | These are deliverables, not prerequisites that can be invoked today. |
| `AppEnv` exists in `config.rs` but is neither parsed nor stored | `APP_ENV=staging` currently protects nothing. Implement and test it before ECS. |
| Endpoint overrides select `ensure_audio_infra`; absent overrides select `resolve_queue_urls` | Cloud must reject overrides. API and worker need `GetQueueUrl` on the main queue **and its DLQ**, but no create permissions. |
| Worker defaults to direct AI; missing provider keys select mocks with warnings | Warnings are insufficient in staging/prod. Require Bedrock and a configured Deepgram worker. API should not require either provider secret. |
| `STT_QUEUE` carries both `TranscribeChunk` and `GenerateMdm`; DLQ name is derived as `<queue>-dlq` | Provision one standard queue and one DLQ. The original separate `ai-jobs` queue is stale. |
| Both model defaults are Sonnet; `model.rs` accepts logical names and supplies hardcoded Bedrock ids | Preserve the pilot quality default. Add validated Bedrock-id/profile overrides only where live access requires them. Do not assume a bare id is invokable. |
| `state.rs` and `bin/worker.rs` connect with `PgPool::connect`; neither runs migrations | The guide's `er-server migrate` does not exist. Neither does a packaged `sqlx` CLI. Add a dedicated executable. |
| `Dockerfile` copies API and worker but has `ENTRYPOINT ["er-server"]` | ECS `command=["er-worker"]` would pass an argument to the API. Override `entryPoint` explicitly. |
| Builder uses floating `rust:1-slim`, runtime uses `bookworm-slim`; build omits `--locked` and `SQLX_OFFLINE=true` | Pin compatible build/runtime bases and a tested toolchain; verify native libraries and clean offline SQL compilation. |
| `.sqlx` metadata and five timestamped migrations are present | Validate the cache against the actual migrated schema. Preserve applied migration checksums. |
| Compose exposes Postgres on **5433**, MinIO on 9000, ElasticMQ on 9324 | PR CI needs all three services, ready checks, and disposable development signing keys. Postgres alone no longer runs the whole suite. |
| Git tracks development PEM/JWKS files, `er_notes.db` and its WAL/SHM files, and `shift.json` | Treat their provenance as unknown. Exclude them from build contexts/caches and inspect exposure before publication; their presence is not proof of PHI. |
| `.dockerignore` is incomplete; `.gitignore` ignores `docs/planning/*` | Add explicit context safeguards. This guide is local unless deliberately included despite that existing ignore rule. |
| `TraceLayer::new_for_http`, dynamic error-chain logging, and identifier-bearing logs exist | Paths, query strings, malformed input, and database/provider error details can expose sensitive data. Fix logging before cloud collection. |
| `/healthz` always returns 200; API serves plain HTTP | Keep liveness, add bounded readiness, and make the ALB-to-task encryption decision explicit. Public HTTPS alone does not encrypt the backend hop. |
| Audio completion checks only positive object size; worker calls `body.collect()` without an audio-byte cap | Bound memory and reject oversize objects before exposing a billable worker. The API's 2 MiB JSON limit does not limit direct S3 uploads. |
| Queue visibility is 300 s; worker receives one message at a time; STT alone can take 181.5 s | A queue timeout is not a complete job deadline. Include AI, object download, lock waits, and DB writes; handle Fargate shutdown explicitly. |
| Worker waits for a handler to finish before observing shutdown; no visibility heartbeat exists | It cannot promise graceful completion within Fargate's stop window. Add bounded draining and prove redelivery recovery. |
| Cognito validator requires an **access token**, matching issuer/client; resolver reads `clinicians.cognito_sub` | `custom:org_id` is not the tenant authority. Cognito users need DB bridge rows. The current issuer URL is already correct. |
| Resolver caches membership for 300 s; the role column is not a complete role-authorization system | Document the revocation delay and the current same-org access model. Do not advertise per-role/per-patient isolation. |
| Event logs, transcript-bearing chunk rows, snapshots, notes, and usage attribution live in Postgres | “Minimal demographics” does not make this database non-PHI. Opaque ids are linkable metadata, not anonymization. |
| Step 7 reports local tests passing, direct synthetic cache evidence, and outstanding live Bedrock/usage checks | Close those live checks in S12. The reported workspace formatting/Clippy backlog must be resolved for required CI; no checks were rerun while drafting. |

Two original compliance assumptions also need correction:

- **Seven-year retention is not a blanket HIPAA medical-record requirement.** HHS says the
  Privacy Rule does not set medical-record retention periods; other law and contracts can.
  Required Security Rule documentation has its own retention rule. Set separate policies for
  source audio, clinical records, backups, security evidence, and operational logs with counsel.
  [HHS medical-record retention FAQ](https://www.hhs.gov/hipaa/for-professionals/faq/580/does-hipaa-require-covered-entities-to-keep-medical-records-for-any-period/index.html),
  [HHS Security Rule summary](https://www.hhs.gov/hipaa/for-professionals/security/laws-regulations/index.html).
- **Regex scrubbing is minimization, not a legal de-identification determination.** Audio reaches
  Deepgram before text exists; spoken names and other identifiers can survive text scrubbing.
  The current Deepgram adapter already sends `mip_opt_out=true`; preserve that safeguard, but
  verify contractual retention, support access, subprocessors, and BAA coverage separately.
  [Deepgram privacy/BAA documentation](https://developers.deepgram.com/trust-security/data-privacy-compliance),
  [Deepgram model-improvement opt-out](https://developers.deepgram.com/docs/the-deepgram-model-improvement-partnership-program).

---

## Target file layout

Names below are proposed unless identified in the audit as existing. Split a module only when
it has a meaningful owner or reusable boundary; do not introduce a general infrastructure framework.

```text
infra/
├── bootstrap/                         # state buckets/keys; protected independently
├── modules/
│   ├── network/                       # subnets/routes/endpoints/security groups
│   ├── data/                          # RDS, audio S3, SQS/DLQ, data KMS key
│   ├── auth/                          # Cognito pool/client; no user passwords
│   ├── identity/                      # task/execution/migration/CI roles; secret metadata
│   ├── edge/                          # ALB, target group, WAF, ACM, DNS
│   ├── observability/                 # logs, audit storage/trail, SNS, baseline alarms
│   └── compute/                       # ECS service/task definition configuration
└── envs/
    ├── staging/
    │   ├── foundation/                # shared modules, cluster, ECR; infrastructure state
    │   └── release/                   # API/worker task definitions + services; release state
    └── prod/
        ├── foundation/                # same interfaces; unprovisioned until enabled
        └── release/
.github/workflows/
├── ci.yml                             # no real AWS/provider credentials
├── infra.yml                          # separately protected foundation plan/apply
└── deploy.yml                         # build → migrate → release → smoke; gated prod
scripts/
├── ci-services.sh                     # readiness + temporary dev signing fixtures
├── deploy-staging.sh                  # bounded task/migration/release orchestration
└── demo-remote.sh                      # synthetic lifecycle/auth regression wrapper
crates/er-server/src/
├── config.rs                          # authoritative environment and per-process validation
├── db.rs                              # proposed shared TLS/pool configuration
├── telemetry.rs                       # proposed bounded, allowlisted logging
├── main.rs, state.rs, routes.rs        # readiness, TLS startup, safe upload limits
├── aws.rs, worker.rs, bin/worker.rs    # cloud guards, deadlines, visibility/drain behavior
└── bin/migrate.rs                     # new explicit er-migrate binary
crates/er-server/tests/
├── deployment_config.rs               # proposed fail-closed configuration cases
└── existing auth/audio/worker/AI gates # extend where new behavior belongs
crates/er-api/examples/staging_smoke.rs # proposed real HTTP lifecycle driver
dev/fixtures/synthetic-encounter.wav    # approved synthetic speech; explicit build inclusion
docs/ops/
├── deployment-contract.md             # choices, resource identifiers, pre-PHI blockers
├── staging-deploy.md                  # bootstrap, onboarding, release and rollback
└── step8-verification.md               # safe results, costs, evidence references
Dockerfile, .dockerignore, .gitignore   # reproducibility and artifact isolation
rust-toolchain.toml                    # tested toolchain shared with CI
```

The `foundation`/`release` distinction is deliberate (§C2). It lets an ordinary application
deployment update ECS without giving that workflow permission to change bucket policy, IAM,
database protection, or network exposure.

---

## Concepts and decisions

### §C1 — A trust boundary is more than a subnet

The device has a Cognito token and temporary permission to upload one S3 object. The API has
tenant-scoped database access and permission to enqueue work. The worker consumes jobs and
calls providers. ECS's **execution role** pulls images, injects secrets, and sends container
logs; the application's **task role** signs its SDK calls. The migration identity owns schema
changes. These permissions should not collapse into one role merely because all processes run
the same image.

GitHub, Terraform state, build caches, exception logs, and support exports are additional data
destinations. No clinical content belongs in them. Protect linkable identifiers and credentials
even where a source is designed to contain no clinical text.

### §C2 — An image tag names a release; a digest fixes its bytes

Pushing `:<git-sha>` and asking ECS to force a deployment does not change a task definition that
still references an older tag. A release needs new task definition revisions referencing
`repository@sha256:…`, followed by service updates to those revisions and verification of the
actual running image digests.

**Recommendation:** Terraform owns foundation resources in one state and API/worker services
and task definitions in a small release state per environment. CI supplies the approved digest
to the release root. Migration task revisions are short-lived deployment artifacts registered
by CI from a reviewed template. Serialize all mutations for the same environment.

This adds a second state key, but avoids two competing owners for `task_definition`. The
alternative is CI-owned revisions plus Terraform `ignore_changes` on service task definitions;
that can work, but weakens drift visibility and requires a separate check that runtime settings
still match the reviewed template. Do not mix the approaches halfway through implementation.

### §C3 — Rollback changes code; it does not reverse clinical history

A release migrates first, then replaces tasks. Therefore the prior image must work with the new
schema. Add columns/tables first; remove old structures only in a later release after old code
and queued messages no longer depend on them. Existing migration files are immutable.

Rollback redeploys the previous digest and configuration. It does not run a destructive down
migration, restore an old database over accepted events, or purge a queue. Backup restoration
is a separate disaster-recovery operation with an explicit data-loss/reconciliation decision.

### §C4 — Synthetic staging is a controlled environment, not a compliance claim

Use fabricated cases and consented actors speaking fabricated scripts. Never copy a production
database, actual encounter recording, patient export, or “de-identified” clinical sample into
staging without the separately approved data-use process. Do not infer that removing names is
sufficient.

AWS lists HIPAA-eligible services and exceptions under a shared-responsibility model; eligible
services do not automatically make the application compliant. Capture the applicable AWS BAA,
service/model eligibility, and supplier terms before PHI use, and recheck when adding a model,
feature, region, or vendor. [AWS HIPAA eligible services reference](https://aws.amazon.com/compliance/hipaa-eligible-services-reference/).

### §C5 — Queue durability and provider execution have different guarantees

The queue is at-least-once. Database idempotency can prevent duplicate clinical events; it cannot
prove a provider did not finish a billable request immediately before the worker died. Expect
some repeated provider work after failures. Keep clinical facts durable, expose stuck/failed
work, and avoid promising exactly-once billing.

Start with one worker. Queue-based autoscaling becomes useful only after overlapping deliveries,
provider quotas, DB connection demand, and STT claim ownership have been validated. A second
queue is a measured latency decision, not a requirement to deploy Step 7's current code.

---

## S0 — Fix the deployment contract and data boundary

**Objective and scope:** resolve the choices that control provisioning, cost, and privacy before
writing environment-specific Terraform. Create a concrete record of known values and blockers.

**Where:** `docs/ops/deployment-contract.md`; environment example inputs. The
contract contains the current, evidence-labelled S0 values and open gates.

**Depends on:** the audit above and Step 7's implemented behavior. This precedes cloud writes.

**Tasks:**

1. Record the actual repository owner/name/default branch, AWS account ids, SSO profiles, region,
   domain/hosted-zone ownership, contact for alarms, and people allowed to approve production.
   Use placeholders until verified; do not infer an account or hosted zone from local filenames.
2. Recommend separate staging and production AWS accounts. Separate VPCs/state/IAM in one
   account are acceptable for the initial **synthetic** deployment if that is what exists;
   record the weaker boundary and require a production-account decision before production
   provisioning. Use separate environment directories, not CLI workspace selection as the only
   protection against deploying to the wrong account.
3. Set `us-east-2` as the staging source region (operator decision and local CLI profile),
   conditional on verified service/model support.
   Record actual Bedrock model/profile ids, permitted processing regions, quotas, and any abuse
   monitoring/retention terms. Prefer regional inference when available; permit a US geographic
   profile only after reviewing all destination regions. Never silently substitute a global
   profile to fix an access error. [Bedrock geographic inference](https://docs.aws.amazon.com/en_en/bedrock/latest/userguide/geographic-cross-region-inference.html).
4. Establish separate retention decisions: propose staging audio 7 days, application logs
   30 days, and RDS automated backups 7 days as **synthetic-operating defaults**. The owner must
   adopt or change them explicitly. Define how synthetic database records will be aged out or
   the isolated environment rebuilt; S3 expiry alone does not remove transcripts or backups.
   Production audio/event/log retention and legal holds remain expert-review questions.
5. Record a first-pass cost estimate with every enabled resource, using current regional rates:
   API/worker hours, RDS compute/storage/backups, ALB/LCUs, NAT and public IPv4, endpoints per AZ,
   KMS/Secrets, logs/CloudTrail data events, WAF, DNS, image/build storage, STT, and AI tokens.
   Use the repository cost model as a workload assumption source, not as current pricing.
   Save the estimate date and budget threshold. [AWS Pricing Calculator](https://calculator.aws/).
6. Classify tracked database/WAL/SHM/shift artifacts and development keys without dumping them to
   logs. Determine whether repository history or caches contain real clinical data or live
   secrets. If so, stop publication of affected artifacts and follow an incident/remediation
   process; do not rewrite history or delete user data as a routine build step. Known dev keys
   are acceptable solely as untrusted test fixtures and must never authenticate cloud requests.
7. Record the current authorization policy: authenticated, provisioned clinicians can access
   patients within their organization as implemented. Confirm this is sufficient for the
   synthetic gate. Tenant isolation, clinical-role restrictions, emergency access, user
   offboarding, and patient assignment need a product/security decision before patient use.

**Privacy/security:** keep agreements and sensitive account evidence in an access-controlled
location; commit only their status, owner, date, and reference. Explicit pre-PHI blockers:
executed applicable BAAs, vendor retention/residency review, recording consent/notice rules,
organizational risk analysis, retention/hold policy, audit-access design, incident/restore
readiness, and authorization/revocation decisions. Counsel/privacy/security reviewers own
those determinations; this plan does not resolve them by labeling resources “HIPAA.”

**Done when / validation:** the contract distinguishes verified facts from assumptions and lists
an owner and closure condition for each unknown. Account/region identity is checked through SSO;
the budget and synthetic-only restriction are recorded. Engineering can proceed with synthetic
work while procurement continues, but no real-data rollout can bypass the pre-PHI register.

### AWS S0 access and identifiers: step-by-step

For **S0, you do not need to create any AWS access-key pairs**. Your Cognito pool signs in app users; your own access to AWS should use **IAM Identity Center (SSO)**. S3, SQS, Bedrock, and the later ECS tasks will use IAM roles, while GitHub deployment will use OIDC. The [S0 guide](#s0--fix-the-deployment-contract-and-data-boundary) asks you to verify account access and record identifiers, not collect service secrets. [AWS recommends temporary credentials over long-term keys.](https://docs.aws.amazon.com/IAM/latest/UserGuide/securing_access-keys.html)

1. **Secure and identify the account.** In the AWS console, open the account menu at the top right → **Security credentials** and confirm root MFA is enabled. Record the 12-digit account ID from that menu or the IAM dashboard. Use root only for account-owner tasks. [Root MFA instructions](https://docs.aws.amazon.com/IAM/latest/UserGuide/enable-virt-mfa-for-root.html) · [Account ID instructions](https://docs.aws.amazon.com/IAM/latest/UserGuide/console-account-id.html)

2. **Set up your AWS operator login.** Open **IAM Identity Center**. If it is already enabled, use its existing instance. Otherwise, review AWS’s setup page and enable an **organization instance** for AWS account access; an account instance is intended for application assignments. If yours is a standalone Free Tier account, read the billing notice before enabling an organization instance. [Identity Center setup](https://docs.aws.amazon.com/singlesignon/latest/userguide/enable-identity-center.html)

3. **Assign yourself account access.** In Identity Center, go to **AWS accounts** → select your staging account → **Assign users or groups**. Assign your named user an existing administrator permission set, or have the account administrator create one for initial setup. The reported organization instance is `zamos` and the assigned user is `zach-admin`; this is separate from the IAM user `zach` in the screenshot. Later steps define narrower deployment roles. Copy the **AWS access portal URL** from Identity Center’s Dashboard and note the *Identity Center Region*; it may differ from the app’s chosen `us-east-2` Region. [Account assignment](https://docs.aws.amazon.com/en_en/singlesignon/latest/userguide/assignusers.html) · [Portal URL](https://docs.aws.amazon.com/singlesignon/latest/userguide/howtochangeURL.html)

4. **Configure and verify the CLI.** The AWS CLI is installed on this machine. In a shell where you have **not** sourced the project’s `.env`, run:

   ```bash
   unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN
   aws configure sso --profile ernote-staging-admin
   aws sso login --profile ernote-staging-admin
   aws sts get-caller-identity --profile ernote-staging-admin
   ```

   Enter the portal URL and Identity Center Region when prompted, select the intended account and permission set, and set the CLI default Region to `us-east-2`. The local profile reports `us-east-2`; STS could not be checked from this workspace because the AWS endpoint was unreachable. Check that `get-caller-identity` returns the staging account ID (the screenshot suggests `657615279057`, but that alone does not establish that it is staging). The CLI obtains temporary credentials; you do not need to copy the portal’s “Access keys” values into a file. [AWS CLI SSO setup](https://docs.aws.amazon.com/cli/latest/userguide/cli-configure-sso.html)

5. **Record Bedrock access, without creating a key.** In **Amazon Bedrock → Model catalog**, with the console set to `us-east-2`, find the Sonnet model. The operator listed `anthropic.claude-sonnet-4-6` and obtained an `ACTIVE` `us.anthropic.claude-sonnet-4-6` profile with destination ARNs in `us-east-1`, `us-east-2`, and `us-west-2`. Repeat the profile query from `us-east-2` because the earlier suggested command used `us-east-1`. AWS's Sonnet 4.6 model card marks in-Region inference unsupported in Ohio. Complete Anthropic’s first-time-use form if prompted, and review the model terms and payment prerequisites before first invocation. Record the actual invokable ID, destination Regions, and applied Bedrock quotas. You can list candidates with:

   ```bash
   aws bedrock list-foundation-models --by-provider Anthropic \
     --region us-east-2 --profile ernote-staging-admin \
     --query 'modelSummaries[].modelId' --output table

   aws bedrock list-inference-profiles \
     --region us-east-2 --profile ernote-staging-admin \
     --query 'inferenceProfileSummaries[].inferenceProfileId' --output table
   ```

   Listing a model or getting an `ACTIVE` profile does **not** prove this account can invoke it. The October 9 code supports the approved US Sonnet profile and priced usage mapping; Ohio rates and one administrator invocation were verified October 14. Worker-role invocation remains pending; global routing is not an automatic fallback. See the [S0 contract](../ops/deployment-contract.md) for evidence status and blockers. [Bedrock access](https://docs.aws.amazon.com/us_en/bedrock/latest/userguide/model-access.html) · [Sonnet 4.6 Region support](https://docs.aws.amazon.com/bedrock/latest/userguide/model-card-anthropic-claude-sonnet-4-6.html) · [Quotas](https://docs.aws.amazon.com/bedrock/latest/userguide/quotas-runtime.html)

6. **Collect the remaining S0 identifiers and decisions.** Check **Route 53 → Hosted zones** for the domain, hosted-zone ID, and who controls its registrar; check **Billing and Cost Management → Budgets** for a monthly threshold and alert email; and check **AWS Artifact → Agreements** for BAA status without accepting legal terms on your own. Also record the GitHub repository/default branch, staging account ID, alarm contact, staging retention choices, and whether production will have a separate account. These are S0 records, not access keys. [Route 53 guidance](https://docs.aws.amazon.com/Route53/latest/DeveloperGuide/dns-configuring-new-domain.html) · [Budget setup](https://docs.aws.amazon.com/cost-management/latest/userguide/create-cost-budget.html) · [AWS Artifact guidance](https://docs.aws.amazon.com/pdfs/artifact/latest/ug/artifact-ug.pdf)

Later, **S6** creates the ECS roles and stores separate database and Deepgram credentials in Secrets Manager; **S11** gives GitHub a scoped OIDC role. Neither step calls for permanent AWS keys in GitHub or this repository. [AWS OIDC guidance](https://docs.aws.amazon.com/IAM/latest/UserGuide/id_roles_providers_oidc.html)

---

## S1 — Make cloud startup and resource use fail closed

**Objective and scope:** eliminate development fallbacks and unbounded work that would turn a
deployment mistake into data exposure, false clinical output, or runaway resource consumption.

**Where:** `config.rs`, `aws.rs`, `state.rs`, `main.rs`, `bin/worker.rs`, `worker.rs`, `routes.rs`,
`er-ai/src/model.rs`; proposed `db.rs`; existing configuration/audio/worker/AI tests.

**Depends on:** S0's environment/provider decisions. Finish before building the releasable image.

**Tasks:**

1. Parse/store `AppEnv`. Require explicit `APP_ENV` in cloud; use explicit `APP_ENV=dev` in local
   commands and tests too. Validate independently for API, worker, and migration processes so
   the API does not need a Deepgram key and migration does not fetch JWKS or resolve queues.
   Load `.env` only on the explicit development path, after determining the environment.
2. Preserve all-or-neither endpoint validation. Outside `dev`, reject both named emulator
   overrides, dev auth/JWKS configuration, direct AI, and silent mock providers. Audit generic
   AWS endpoint overrides/shared configuration as well; cloud startup must not be redirected
   through `AWS_ENDPOINT_URL` or service-specific equivalents. Do not reject the ECS-provided
   container credential variables that the task-role provider legitimately needs.
3. Set `TRANSPORT=bedrock`, `SUMMARY_MODEL=sonnet`, `MDM_MODEL=sonnet` explicitly for the worker.
   Add optional `BEDROCK_SUMMARY_MODEL_ID` / `BEDROCK_MDM_MODEL_ID` if the verified model/profile
   differs from `model.rs`. Bind overrides to supported model families and reviewed pricing;
   unknown overrides must not silently meter at zero or change model family. Account access
   checks belong in deployment preflight, not in every API health probe.
4. Centralize DB connection options with explicit maximum connections, acquire/connect timeouts,
   application name, and certificate/hostname validation. Cloud connections use
   `sslmode=verify-full` and a trusted RDS CA bundle. Avoid logging URLs, passwords, SQL binds,
   or full connection errors. Local Postgres may use its explicit dev TLS exception.
5. Add an initial configurable **16 MiB maximum audio object size**, a proposed limit to validate
   against supported chunk formats. Enforce it in completion metadata checks and during worker
   streaming, with a hard byte counter even if Content-Length is absent or false. Bound download
   time too; return a static error code and never enqueue/transcribe a known oversized object.
   Verify stored content type against the declared supported type. Do not describe these checks
   as an S3 upload-size quota: a presigned PUT can still consume storage before completion.
6. Set explicit SDK/provider operation deadlines and a complete job time budget. Start with a
   270-second job ceiling, then validate it against STT's 181.5-second retry budget plus AI,
   S3, lock waits, and database work. Give Bedrock a bounded **total operation** deadline,
   including SDK retries; the direct HTTP timeout does not configure Bedrock.
   Route deadline failures through the existing delivery-count/terminal-error handling so a
   final timed-out delivery marks its receipt failed before the message proceeds to the DLQ.
7. Renew SQS visibility while a job is active (for example, every 60 seconds extending to
   300 seconds), stop renewal on completion/exit, and bound total processing. Coordinate lease
   failures with the handler; do not knowingly keep unbounded work alive after losing its lease.
   Never delete a message solely because a timer or shutdown fired. Extend worker tests to cover
   expiry, renewal failure, and recovery from an interrupted durable state.
8. Make SIGTERM stop new receives immediately, allow an in-flight handler up to a bounded drain
   interval (start with 100 seconds), then exit without acknowledging unfinished work. Keep
   queue visibility long enough for the old process to terminate before ordinary retry. Dropped
   transactions must release locks; retries must repair persisted partial progress using the
   existing job/event rules. Fargate allows at most 120 seconds of `stopTimeout`, so “wait until
   the job finishes” is not a shutdown policy. [ECS task timeout parameters](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/task_definition_parameters.html).

**Configuration contract to pin:**

| Setting | API in staging/prod | Worker in staging/prod | Migration |
|---|---|---|---|
| `APP_ENV`, region | Required, verified environment | Same | Same |
| DB connection | API login, verified TLS | Worker login, verified TLS | Migration login, verified TLS |
| `BIND_ADDR` | Explicit listener address | Not required after config separation | Not required |
| `AUTH_MODE`, Cognito region/pool/client | Cognito only | Not needed for job execution | Not needed |
| `AUDIO_BUCKET`, `STT_QUEUE` | Terraform names | Same names | Not needed |
| Endpoint overrides/dev keys/static AWS keys | Rejected or absent by cloud policy | Same | Same |
| `TRANSPORT` / model settings | No provider initialization | Explicit Bedrock / approved families | Not needed |
| `DEEPGRAM_API_KEY` | Absent | Required via ECS secret | Absent |
| `ANTHROPIC_KEY` | Absent | Rejected | Absent |

**Privacy/security:** audio is processed in memory; do not spill it to task storage or attach it
to failure reports. Keep error classifications static. A process environment is not a secret
vault: avoid config `Debug` dumps, core dumps, shell tracing, and diagnostic environment output.

**Done when / validation:** table-driven tests reject unsafe combinations before network I/O;
dev emulator tests still pass. Oversize/incorrect-metadata audio never reaches the provider;
unknown-length streams stop at the cap. Timed tests prove bounded downloads/jobs, lease renewal,
and shutdown redelivery. Startup logs contain environment/provider names but no credentials.

---

## S2 — Package reproducible binaries and a real migration command

**Objective and scope:** produce a small, repeatable image that works without a developer's
filesystem or database, and an explicit schema-migration executable.

**Where:** `Dockerfile`, `.dockerignore`, `.gitignore`, `rust-toolchain.toml`,
`crates/er-server/Cargo.toml`, proposed `src/bin/migrate.rs`, `.sqlx`, CI image checks.

**Depends on:** S1's process-specific configuration and DB helper. S8 later supplies TLS startup.

**Tasks:**

1. Add `[[bin]] name = "er-migrate"` with `path = "src/bin/migrate.rs"`. Use
   `sqlx::migrate!("./migrations")` so the schema travels with the image; it needs neither a
   shell-installed sqlx CLI nor a source mount. Connect with the migration identity, execute
   the migrator with locking enabled, report only version/status, and exit nonzero on failure.
   Keep schema migration out of API/worker startup.
2. Keep role/bootstrap setup distinct from application migrations. A first RDS setup establishes
   the owner/login roles (S6); `er-migrate` creates application tables under the approved owner.
   Give migrations bounded lock/statement/overall execution time, tuned for the actual DDL.
   Do not silently ignore a failed or checksum-mismatched migration.
3. Pin a Rust release compatible with the resolved AWS SDK MSRV and `Cargo.lock`. Pin cargo-chef,
   the builder image, and the runtime image; use compatible Debian generations and immutable
   digests. Decide CPU architecture explicitly: recommend `linux/amd64` for the first deployment
   to match straightforward GitHub builds. The RDS Graviton instance does not force ARM clients.
4. Build only the intended production binaries with `--release --locked`, and set
   `SQLX_OFFLINE=true` for both cargo-chef cook and final compile. Include checked-in `.sqlx`
   metadata in the stage that first compiles application queries. Regenerate metadata locally
   if needed; do not connect a Docker build to staging RDS.
5. Carry API, worker, and migration binaries into the runtime image. Retain non-root execution,
   include CA certificates/RDS trust material and the native libraries actually required by
   `ldd`/runtime testing, and remove build tools from the runtime stage. The TLS server addition
   in S8 must use this same image pipeline.
6. Correct container startup explicitly. Keep the current API default if convenient, but set
   ECS `entryPoint=["/usr/local/bin/er-worker"]` and
   `entryPoint=["/usr/local/bin/er-migrate"]` for the other tasks. Arguments belong in
   `command`. Do not rely on a command override to replace an entrypoint.
7. Prefer an allowlisted build context: Cargo manifests/lock, crates, `.sqlx`, approved prompt
   files and CA assets. Exclude `.git`, `.env*` with a deliberate example-file exception if
   needed, `dev/keys`, DB/WAL/SHM files, shifts, recordings/exports, Terraform state/plans,
   local tool directories, and arbitrary documentation/output folders. Only explicitly approved
   synthetic fixtures may enter a smoke/test artifact.
8. Establish Git tracking rules that prevent new local DB/key/state files from being committed.
   Existing tracked files are unaffected by adding an ignore pattern: follow S0's provenance
   decision and use a reviewed untracking/remediation change if needed. Do not delete a local
   database as a side effect. Never export a shared build cache that has already consumed an
   unreviewed full working tree.

**Skeleton (illustrative, not a complete binary):**

```rust
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

// Parse only migration-relevant settings; build the same verified TLS options.
// Bound lock/statement/run time and emit a safe failure classification.
let pool = build_migration_pool(&migration_config).await?;
MIGRATOR.run(&pool).await?;
pool.close().await;
```

**Privacy/security:** Docker intermediate layers and remote caches matter as much as the final
image. No build arg, secret value, clinical fixture, or full environment dump goes into an image,
attestation, SBOM, or workflow artifact. Signatures/attestations describe code provenance only.

**Done when / validation:** build from a clean tracked-source checkout with no `.env`, private
keys, or live DB; start each binary using its exact intended entrypoint. Inspect runtime dynamic
libraries and image/context contents without printing secrets. Apply migrations to an empty
test DB, rerun as a no-op, and verify a bad migration blocks startup orchestration. Confirm the
previous image can still read/write after each additive migration introduced in Step 8.

---

## S3 — Bootstrap Terraform state and establish ownership

> **Implemented 2026-09-14.** The shipped shape, its deviations from this outline, and what
> was actually verified are in the [S3 implementation record](#implementation-record--s3-2026-09-14).
> The outline below remains the design rationale.

**Objective and scope:** make state durable, access-controlled, and recoverable before it
controls application infrastructure.

**Where:** `infra/bootstrap/`, the four environment roots, provider/backend files and lockfiles.

**Depends on:** S0 account layout and operator SSO. This is the first infrastructure apply.

**Tasks:**

1. Use a tested Terraform version with native S3 locking (at least 1.10; pin the tested release)
   and a pinned AWS provider constraint/lockfile. The original DynamoDB lock-table setup is
   outdated for a new backend: S3 `use_lockfile=true` is available and DynamoDB locking is
   deprecated. [Terraform S3 backend](https://developer.hashicorp.com/terraform/language/backend/s3).
2. Bootstrap dedicated state storage under SSO: block public access, enable versioning and KMS
   encryption, deny insecure transport, and narrowly scope principals. Separate production
   state permissions from staging. Protect the state bucket/key against routine destruction;
   never place them in the lifecycle of a disposable staging stack.
3. Migrate initial local bootstrap state to its protected remote key. Verify the remote copy
   and recovery procedure before securely handling local state/backups. Keep `.terraform/`,
   `*.tfstate*`, binary plans, crash logs, and local backend credential material out of Git and
   build artifacts. Commit `.terraform.lock.hcl` for each root.
4. Use distinct backend keys for bootstrap, staging foundation/release, and production
   foundation/release. Backend credentials come from SSO/OIDC, never `backend.hcl` or tfvars.
   Grant lock-object operations separately from state-object access; normal operators need not
   delete the state object to release a lock.
5. Make each provider/root assert its expected account and region. Use `allowed_account_ids`
   or equivalent provider guards plus validation on environment inputs. Use predictable
   project/environment tags and globally unique bucket names without patient/clinician data.
6. Publish a small **non-secret deployment manifest** from foundation outputs: resource ARNs,
   subnets/SGs, image repository, queue/bucket names, Cognito ids, and approved task-secret ARN
   references. Release CI consumes this contract rather than reading an entire foundation state
   through `terraform_remote_state`. Persist it in a versioned, access-controlled artifact or
   S3 object with an integrity/version reference; no password values.
7. Keep release state limited to ECS services/task definitions. The foundation root owns IAM,
   keys, RDS, ECR, cluster, edge, logs, and network. No `ignore_changes` on security settings to
   silence drift; document any narrowly justified exception.

**Illustrative backend, with real names supplied at setup:**

```hcl
terraform {
  backend "s3" {
    key          = "staging/release/terraform.tfstate"
    region       = "us-east-2"
    encrypt      = true
    use_lockfile = true
    # bucket and kms_key_id supplied as non-secret backend configuration
  }
}
```

**Privacy/security:** `sensitive=true` hides ordinary CLI display; it does not remove a value
from state or a saved plan. Create secret metadata with Terraform and populate secret values
outside Terraform's ordinary resource/state path. State access is privileged even when the
design intentionally avoids secret values.

**Done when / validation:** `fmt`, `init`, and `validate` pass; two simultaneous state operations
cannot both acquire the same lock. Staging credentials cannot read/write production state.
A prior version of a disposable state object can be recovered. Wrong-account execution fails
before resource creation. Plans and outputs reveal no credential values.

---

## S4 — Build the network and prove every required connection

**Objective and scope:** provide private workload/data placement, controlled ingress, and a
complete egress path without buying endpoints that have no justified purpose.

**Where:** `infra/modules/network/`, SG interfaces consumed by data/edge/compute.

**Depends on:** S3 backend/provider guards; S0 availability/cost decisions.

**Tasks:**

1. Create a VPC with DNS support/hostnames, two public subnets for the ALB/NAT, two private task
   subnets, and private DB subnet-group placement across two AZs. A DB subnet group spanning
   AZs does not turn a single-AZ DB into Multi-AZ.
2. Start staging with one NAT gateway and document its AZ outage/cross-AZ transfer tradeoff.
   Tasks have no public IPs; RDS has no public endpoint. NAT is needed for the current Deepgram
   endpoint and public Cognito JWKS access. Do not remove NAT merely because AWS interface
   endpoints were added.
3. Add an S3 gateway endpoint to the task route tables. For the small synthetic deployment,
   recommend HTTPS through NAT for the remaining services unless S0's security requirement or
   measured cost justifies interface endpoints. Preserve module switches for `sqs`,
   `bedrock-runtime`, `secretsmanager`, `ecr.api`, `ecr.dkr`, and `logs`. Include endpoint ENI
   hourly charges **per AZ** in the comparison; “endpoints save NAT money” is workload-dependent.
4. If endpoints are enabled, enable private DNS and allow endpoint-SG ingress on 443 only from
   the required task/execution paths. Scope endpoint policies to actual resources/actions.
   ECR pulls need both ECR endpoints and access to S3 layer storage; CloudWatch and secret
   injection happen before application startup. A healthy app cannot repair a failed image pull.
5. Define the connection matrix below as security-group rules and reviewable intent. Ordinary
   security groups cannot restrict public HTTPS egress by DNS name. For this synthetic stage,
   allow required outbound 443 through NAT with IAM and provider endpoint validation. A domain
   filtering proxy/firewall is a later option if the risk assessment requires it, not an
   imaginary property of the NAT gateway.
6. Give one-shot migration/onboarding tasks private network access through dedicated SGs.
   Hosted GitHub runners do not connect to RDS. Avoid a permanent bastion or inbound SSH.
7. Verify outbound TLS certificate checking for provider/JWKS clients. Keep endpoints fixed or
   administrator-configured; no request body may supply a fetch URL. Never switch to insecure
   TLS verification to diagnose a network or certificate problem.

| Source | Destination | Allowed connection |
|---|---|---|
| Clients | ALB | HTTPS 443; optional 80 only for redirect |
| ALB SG | API SG | HTTPS application port only |
| API SG / worker SG | RDS SG | PostgreSQL 5432 with verified TLS |
| Migration/onboarding SG | RDS SG | 5432, used only by approved one-shot tasks |
| Task SGs | S3 / selected endpoints | HTTPS; IAM/resource policies still apply |
| API | Cognito JWKS | HTTPS via approved egress |
| Worker | Deepgram / Bedrock | HTTPS via approved egress or Bedrock endpoint |
| ECS execution paths | ECR / logs / Secrets | Required AWS service access on HTTPS |
| Internet | API tasks / worker / RDS | No direct inbound path |

**Privacy/security:** public S3 service access for a presigned client PUT must remain possible.
A blanket `aws:SourceVpce` deny on the audio bucket would block the phone/CLI outside the VPC.
Use authenticated signatures, TLS, short expiry, and bucket/IAM policy instead; distinguish
publicly reachable AWS APIs from publicly readable bucket contents.

**Done when / validation:** inspect subnet routes/public-IP settings and SG references. From a
disposable private diagnostic task, verify DNS, TLS, ECR/Secrets/log delivery, queue lookup, and
DB connectivity with scoped permissions. From outside the VPC, direct task/DB access fails.
Record the endpoint/NAT choice and idle cost; do not open DB ingress to a developer IP as a fix.

---

## S5 — Provision encrypted storage, queues, and recoverable data

**Implementation status (2026-10-09):** source and local gates are implemented; see
the [S5 implementation record](#implementation-record--s5-2026-10-09). The tasks below
remain the design and live acceptance contract, not a claim that AWS resources exist.

**Objective and scope:** create the exact durable resources the current application consumes,
with explicit lifecycle and recovery settings.

**Where:** `infra/modules/data/`, environment inputs, the retention section of the deployment
contract; any additive grants/schema changes required by S6.

**Depends on:** S4 network and S0 data-policy defaults. IAM grants are completed in S6.

**Tasks:**

1. Provision RDS PostgreSQL 16 at a currently supported minor version; verify regional class and
   storage combinations before applying. Start with the plan's `db.t4g.small`, single-AZ,
   50 GiB gp3 if supported and within the approved estimate. Cap storage autoscaling, schedule
   maintenance/backups, enable encryption, and require final snapshots for controlled removal.
   Enable deletion protection for persistent staging as well as production; deliberate teardown
   is a separate reviewed operation.
2. Set an explicit automated-backup window/retention (S0 proposes 7 days in synthetic staging)
   and verify `LatestRestorableTime` once backups exist. Resolve the old plan's 7-versus-35-day
   discrepancy here; production recovery objectives decide production retention. Encryption
   keys must remain available for as long as their retained backups are needed.
3. Set `rds.force_ssl=1` and configure clients for CA and hostname verification. Server-side
   encryption enforcement alone does not verify the server's identity. Include CA rotation in
   the image/operations procedure. [RDS PostgreSQL TLS guidance](https://docs.aws.amazon.com/AmazonRDS/latest/UserGuide/PostgreSQL.Concepts.General.SSL.html).
4. Create an audio bucket with all public-access blocks, bucket-owner-enforced ownership,
   versioning, default SSE-KMS using the environment data key, and TLS-only policy. Enable S3
   Bucket Keys if appropriate to the cost/key-policy design. Current PUTs rely on bucket-default
   encryption: allow missing SSE headers but reject an explicitly wrong encryption algorithm or
   key. If requiring encryption headers instead, update the presign contract and client together.
5. Define current and noncurrent-version expiry separately, plus abandoned multipart cleanup.
   Do not configure Deep Archive as a catch-all: the worker has no restore workflow and cannot
   transcribe an archived pending object. No 7-year compliance-mode Object Lock on audio without
   an approved retention/hold decision. Lifecycle expiry is asynchronous, not an exact deletion
   deadline or an end-to-end erasure guarantee.
6. Track the presigned overwrite race explicitly: the same valid URL can overwrite its key, and
   `HeadObject` followed by an unversioned `GetObject` does not pin the approved bytes. Versioning
   alone does not fix that. Before PHI use, add a persisted version/checksum binding at completion
   and worker reads of that version, or a tested conditional write-once contract. Defer that
   protocol extension to Step 9 while staging is synthetic; prohibit claims of immutable audio.
   [S3 presigned upload behavior](https://docs.aws.amazon.com/AmazonS3/latest/userguide/PresignedUrlUploadObject.html).
7. Create one **standard** job queue and `<name>-dlq`. Set main visibility to 300 seconds to
   match S1, long-poll wait to 20 seconds, `maxReceiveCount=3`, and explicit retention—propose
   4 days main/14 days DLQ. Restrict DLQ redrive permission to this source queue and an operator
   role. Use SSE-KMS; deny insecure transport. No transcripts, names, or provider responses in
   message bodies, attributes, queue names, or alarm text.
8. Keep CloudTrail/security log storage separate from audio lifecycle. Use distinct key
   boundaries for state/security evidence versus application data; one application-data key
   for RDS/S3/SQS is adequate initially if grants remain scoped. Record how disabling or deleting
   a key can make data unrecoverable. Avoid per-tenant keys until a contract or isolation need
   justifies their operational cost.
9. Do not provision an application export bucket yet. `note/export` returns the existing HTTP
   export; nothing currently writes exports to S3. Reserve an interface only when a consumer is
   designed. This does not remove the separate audit/log buckets that do have consumers.

**Privacy/security:** backups, S3 historical versions, transcripts in `audio_chunks`, raw
`TranscriptReady` events, snapshots, and exported notes are separate copies. A future deletion
or hold workflow must account for all of them. Ordinary runtime roles must not delete buckets,
purge queues, change retention, disable keys, or read backups.

**Done when / validation:** cloud API inspection confirms private RDS, encryption, backups,
public-access blocks, queue/DLQ settings, and lifecycle rules. A correctly signed client upload
works without extra SSE headers; public/unsigned GET and insecure transport fail. Inspect a
synthetic object's encryption/key metadata. Runtime roles cannot change these controls. Complete
restore rehearsal belongs to Step 9, but a disabled/unavailable backup is already a Step 8 failure.

---

## S6 — Separate IAM roles, database privileges, and secrets

**Objective and scope:** make permissions follow the actual code paths, including startup,
encryption, migrations, and secret injection.

**Where:** `infra/modules/identity/`, data/key policies, DB bootstrap/grant procedure,
`db.rs` and process config; deployment role templates for S11.

**Depends on:** S5 resource ARNs and S4 SGs; may be authored alongside them.

**Tasks:**

1. Create distinct API and worker task roles and distinct execution roles where their secrets
   differ. Scope trust to ECS tasks in the account. Add migration/onboarding roles and scoped
   infrastructure/deployment/synthetic-smoke CI roles; no shared administrator role.
2. Translate these required actions into resource-scoped policies. S3 `HeadObject` uses
   `s3:GetObject` authorization, not an invented `s3:HeadObject` IAM action. Presigning is local
   signing, but S3 later evaluates the signing role's permissions. Test actual AWS operations.
   To preserve `verify_chunk_object`'s missing-upload handling, recommend `s3:ListBucket` on
   this audio bucket for the API: without it, a missing object's HEAD returns 403 rather than
   404. This exposes bucket key metadata to the already-privileged API role, so document the
   tradeoff and expose no listing endpoint. If listing is prohibited, explicitly redesign/test
   that error contract; do not conflate every permission failure with a missing upload.
   [S3 HEAD permissions and missing-object behavior](https://docs.aws.amazon.com/AmazonS3/latest/API/API_HeadObject.html).

   | Identity | Required capability | Explicit boundary |
   |---|---|---|
   | API task | Audio `PutObject` for signed uploads; `GetObject` for metadata verification; `ListBucket` on the audio bucket for missing-object semantics; `SendMessage` on main queue; `GetQueueUrl` on both queues; corresponding KMS use | No Bedrock, provider-secret retrieval, receive/delete/purge, or infrastructure creation |
   | Worker task | Audio `GetObject`; main queue receive/delete/change-visibility; queue lookup for both queues; approved Bedrock invocation; corresponding KMS use | No audio PUT, clinician administration, or infrastructure mutation |
   | API execution | ECR pull, its log stream writes, exact API DB secret retrieval/decryption | No Deepgram secret |
   | Worker execution | ECR pull, its log writes, exact worker DB and Deepgram secrets | No API/migration DB credential |
   | Migration execution/task | Image/log access and exact migration credential; database DDL via that login | No STT/AI calls or queue consumer permissions |
   | Onboarding task | Narrow Cognito admin actions and DB identity setup via approved operator procedure | One-shot privileged use; never an API task capability |
   | Release CI | Its release state/manifest, ECR push as needed, ECS task/service operations, narrowly scoped `PassRole` | No reading clinical buckets, DB passwords, or provider keys; no IAM/network/RDS changes |

3. Add required KMS permissions to **both** IAM and key policy. S3 writers need data-key
   generation; readers need decryption. SQS producers and consumers have service-specific KMS
   requirements, including producer decryption/data-key reuse. Execution roles decrypt only
   the injected secret keys. Scope service use with `kms:ViaService`/encryption context where
   supported, testing context differences when S3 Bucket Keys are enabled. Avoid `kms:*`.
   [SQS producer/consumer KMS requirements](https://docs.aws.amazon.com/AWSSimpleQueueService/latest/SQSDeveloperGuide/sqs-key-management.html).
4. Scope Bedrock to the exact approved foundation model or inference profile **and required
   destination-model ARNs**. Verify applicable SCP restrictions as well. The existing worker
   uses non-streaming invocation; do not grant unrelated batch, agent, or model-management
   permissions. An access failure must not trigger wildcard policy expansion.
5. Create Secrets Manager containers/ARNs with Terraform; populate real values through a secure
   operator or purpose-built bootstrap path that does not persist plaintext in Terraform state.
   Keep Deepgram secrets environment/project-specific. Do not create an Anthropic secret for
   staging/prod. RDS-managed master credentials may bootstrap the DB, but runtime must never
   receive the master secret.
6. Create non-superuser DB logins for API and worker, and a schema-owner/migration role. Runtime
   logins have neither ownership nor `BYPASSRLS`; revoke public schema creation. Grant only
   table/sequence operations needed by each code path, including event `INSERT`/sequence usage,
   snapshot upserts, and their operational ledgers. Runtime cannot update/delete historical
   events, create/alter tables, or manage org/clinician membership. Apply default privileges for
   the role that actually creates future objects, not for an unrelated administrator.
7. Define a reproducible one-shot DB bootstrap operation inside the VPC: retrieve approved
   bootstrap credentials, create/update login roles, write their individual Secrets Manager
   values without logging SQL/passwords, run migrations under the owner, then apply grants.
   Parameterize metadata; bind/escape secret values correctly. This is privileged setup,
   separately reviewed and repeatable; do not put passwords in Terraform `local-exec` or SQL
   files. First-run role creation must precede the first application migration.
8. Preserve `DATABASE_URL` as a dev convenience. For cloud, recommend non-secret host/port/name
   plus username/password injected from JSON secret fields, composed using `PgConnectOptions`
   instead of string interpolation. This avoids URL-escaping errors and a second stored URL
   secret that can go stale when a password changes. Define the exact config names in the
   deployment contract and use them in all three binaries.
9. Write a safe rotation sequence: create/rotate the credential, start replacement tasks using
   the new value, verify new connections, then retire the old value/credential according to the
   chosen DB strategy. An alternating-user strategy can avoid a single-password replacement
   gap; evaluate it during Step 9. For initial synthetic staging, a scheduled maintenance
   rotation with tested restart/recovery is acceptable. ECS-injected secrets do **not** update
   in running tasks. [ECS secret injection and rotation behavior](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/secrets-envvar-secrets-manager.html).

**Privacy/security:** the API's bucket-wide signing role is not tenant isolation; tenancy still
comes from authenticated repository checks and server-derived object keys. Similarly, the worker
has cross-tenant lookup methods by design and is a privileged processor. Keep its external
attack surface small and do not expose those methods through HTTP. `PassRole` plus arbitrary
image deployment is powerful even without `GetSecretValue`; protect deployment code accordingly.

**Done when / validation:** run positive and negative AWS permission probes from disposable
tasks using the actual roles. Verify API cannot invoke Bedrock/read Deepgram secrets; worker
cannot write audio; runtime cannot DDL/delete events; deployment cannot assume production roles
or retrieve DB/provider secrets. Verify queue lookup, encrypted upload/download/send/receive,
logging, and secret injection succeed. Rotate a synthetic credential and prove replacement
tasks reconnect without exposing its value.

---

## S7 — Adopt Cognito and provision synthetic tenants

**Objective and scope:** make cloud authentication and tenant membership reproducible while
preserving Step 4's actual access-token contract.

**Where:** `infra/modules/auth/`, onboarding procedure, proposed smoke fixture setup,
`docs/ops/staging-deploy.md`; existing auth tests.

**Depends on:** S0 identity decisions, S5 migrated schema, S6 operator/DB permissions.

**Tasks:**

1. Inventory the Step 4 sandbox pool/client through read-only AWS inspection. If it is suitable
   and must preserve users, import it and review a no-replacement plan. Otherwise create a
   separate staging pool and re-provision synthetic users deliberately. Do not assume the
   sandbox exists or delete it automatically; pool replacement changes subjects and invalidates
   DB bridge rows/tokens.
2. Disable public self-signup. Create an application client without an embedded client secret
   for CLI/native use; configure supported login/refresh flows, explicit token durations,
   revocation settings, and exact callback/logout URLs when a hosted login flow is used.
   Avoid wildcard callbacks and implicit grant. Phase 5 can add its reviewed native redirect
   without replacing the pool. Do not create `custom:org_id` as the source of authorization.
3. Decide how automated smoke authentication works. Recommendation: dedicated synthetic users
   in the synthetic staging pool, with a supported password authentication flow enabled only
   there. Human AWS/GitHub administrators still use MFA. If the staging pool permits automated
   non-MFA users, label that as a synthetic-only exception and prohibit reuse for patient data.
   Production requires a separate reviewed MFA/session policy and must not inherit this smoke
   exception. If mandatory MFA is already enforced, use a compatible approved test login flow;
   do not downgrade it silently or substitute ID/dev tokens.
4. Seed two synthetic orgs and clinician bridge rows using the privileged onboarding path.
   Fetch each actual Cognito `sub`, then insert/upsert it transactionally with the intended org.
   Make repeat runs idempotent, but refuse an unexpected subject-to-org reassignment instead
   of silently moving a clinician. User passwords belong in scoped Secrets Manager entries,
   not Terraform user resources, SQL migrations, or test fixtures.
5. Define one distinct smoke role that can read **only synthetic staging login credentials**.
   It has no provider/DB credentials or patient-data bucket access. The smoke client authenticates
   through Cognito and uses the public API exactly as another client would. Record that an AWS
   OIDC deployment credential is not an application access token.
6. Pin auth negatives: expired token, wrong pool, wrong client, ID token, dev token, missing
   bearer token, and a valid token whose `sub` has no clinician row. Wrong-org reads/writes and
   job/audio lookups must retain the existing indistinguishable not-found behavior.
   Harden `auth/keys.rs` at the same boundary: its current `Client::new()` fetch has no explicit
   timeout and every unknown `kid` can trigger a refresh. Add bounded HTTPS-only JWKS fetches,
   disable redirects, cap response bytes, and coalesce/rate-limit refreshes with a short bounded
   negative-cache/cooldown policy. Keep last-known-good keys on a fetch failure without accepting
   an unknown key. Test real key rotation, an unavailable JWKS endpoint, and concurrent random-kid
   traffic; a public unauthenticated request must not create unlimited upstream work.
7. Document membership cache lifetime and offboarding limitations. Disabling a Cognito user
   does not make this offline JWT validator perform per-request revocation. Existing tokens
   and cached membership can remain effective until their relevant limits expire. Provide a
   staging emergency sequence (disable issuance, remove membership, replace API tasks to clear
   caches, verify denial); Step 9 must settle the clinical revocation SLA and implementation.

**Privacy/security:** use synthetic usernames/display names and no patient data in Cognito
attributes. Keep access/refresh tokens and passwords out of command output, screenshots,
workflow artifacts, and committed instructions. Use the database as the organization authority;
a successful login alone never provisions clinical access.

**Done when / validation:** both real Cognito access tokens reach `/v1/me` with the expected
distinct orgs; all negative token cases fail. Cross-org patient/event/snapshot/audio/job/export
attempts fail through the real deployed router. Re-running onboarding creates no duplicate
memberships or accidental transfers. Record the precise synthetic MFA exception, if used.

---

## S8 — Define ECS services and the HTTPS edge

**Objective and scope:** run the reviewed binaries behind a narrow encrypted ingress path and
give deployment health checks enough information to reject a broken release.

**Where:** `infra/modules/compute/`, `edge/`, environment release roots; `main.rs`, `state.rs`,
`routes.rs` and server-only TLS dependencies; image pipeline from S2.

**Depends on:** S1–S7 contracts. Define resources now; S11 activates the first release after migration.

**Tasks:**

1. Create an ECS cluster and ECR repository in foundation. Enable immutable image tags,
   vulnerability scanning, and lifecycle rules that retain the current and prior accepted
   releases plus a useful rollback window. Never expire an image still referenced by a running
   or designated rollback task definition.
2. Define separate API and worker task families sharing the digest. Use Fargate `awsvpc`,
   explicit CPU architecture/platform version, non-root user, dropped Linux capabilities,
   no privileged containers, read-only root filesystem, and no ECS Exec by default. Add bounded
   writable scratch space only if a dependency demonstrably needs it; no persistent audio files.
3. Start staging with two API tasks (0.5 vCPU/1 GiB each) across AZs and one worker of the same
   size. Measure actual memory/headroom; these are proposed initial allocations, not benchmark
   results. Keep worker autoscaling disabled initially. Use API rolling replacement with
   healthy capacity maintained; use worker stop-before-start (`minimumHealthyPercent=0`,
   `maximumPercent=100` at desired count 1) to avoid intentional overlapping consumers until
   the concurrency gate is complete. A brief worker pause is acceptable for synthetic staging.
4. Set worker `stopTimeout=120` and use S1's shorter drain deadline. Configure API deregistration
   and shutdown coherently: for the current 30-second request timeout, start with 45-second
   deregistration and 60-second stop timeout, then verify behavior. ECS service stability alone
   does not prove worker progress; include its heartbeat and a real job smoke check.
5. Budget DB connections for the maximum number of tasks **during deployment**, not just steady
   state. Initial explicit caps might be 5 per API and 5 per worker; measure lock-held and query
   connections under the existing advisory-lock design. Reserve migration/onboarding/admin
   capacity. Do not add RDS Proxy until connection pressure justifies its cost/complexity.
6. Create ACM DNS validation in the ALB region, Route 53 alias records, and an HTTPS listener with
   a currently supported policy permitting TLS 1.2/1.3. Port 80 may redirect but never serve
   clinical API responses. Verify certificate hostname/chain without `curl -k`. Clients must
   begin sensitive requests with HTTPS; redirect is not protection for a body already sent over HTTP.
7. Encrypt ALB → API too. Recommend direct server-side Rust TLS in `er-server`, preserving plain
   local HTTP in explicit dev mode. Use an HTTPS `ip` target group on a dedicated port, such as
   8443; allow that port only from the ALB SG. A per-task self-signed certificate generated in
   memory avoids distributing a private key or adding a proxy sidecar. ALB does not validate
   target certificates, so this provides encryption while target authorization relies on
   controlled registration/network/IAM, **not mutual TLS**. Record that assumption for security
   review. Use managed internal certificates/mTLS only if the required trust model calls for it.
   [ALB HTTPS target behavior](https://docs.aws.amazon.com/elasticloadbalancing/latest/application/load-balancer-target-groups.html).
8. Keep `/healthz` as minimal liveness. Add `/readyz` with bounded DB checkout/`SELECT 1`, expected
   schema availability, and completed startup configuration/key initialization. It returns
   static status, never resource ids or errors. Do not call Deepgram/Bedrock on every health
   check. Startup timeout/grace must allow normal JWKS/TLS/DB initialization while remaining
   bounded. Use HTTPS `/readyz` for ALB health; test that DB loss changes readiness to 503.
9. Attach regional WAF before enabling ingress. Begin with managed common protections and an
   explicit rate rule; the old 2,000 requests/5 minutes/IP is a starting hypothesis, not a
   clinician quota. Hospital NAT can put many clinicians behind one address. Test valid event
   batches against body-size/managed rules; use narrow documented exceptions, not a blanket
   `/v1` bypass. Keep authenticated expensive-operation quotas in the Step 9 handoff.
10. Keep ingress closed during foundation provisioning and open it only for the synthetic
    release validation. A public staging endpoint can remain protected by Cognito/WAF with
    self-signup disabled; a tester IP allowlist is an optional extra, provided smoke-runner
    reachability is designed explicitly. Do not automate widening it to the whole internet
    merely because a runner IP changed.

**Privacy/security:** retain the 2 MiB application JSON body cap and static external error shapes.
Use `Cache-Control: no-store` for authenticated clinical/token/upload responses where applicable;
do not add shared HTTP caching. The task SG, WAF, and Cognito controls have different jobs and
none replaces repository tenancy checks.

**Done when / validation:** exact digest/entrypoint/task roles match the release manifest;
API serves encrypted traffic on the backend hop, and the worker has no public listener.
Unhealthy startup does not enter service; DB failure fails readiness; liveness stays minimal.
TLS policy, DNS, certificate renewal path, non-root/read-only behavior, and WAF valid/blocked
requests are exercised. Replacement/restart completes without losing accepted clinical events.

---

## S9 — Install privacy-aware telemetry and deployment alarms

**Objective and scope:** detect a failed or unsafe first deployment without creating a second
copy of clinical content in logs. This is the operational minimum; Step 9 expands it.

**Where:** proposed `telemetry.rs`; `main.rs`, `bin/worker.rs`, `error.rs`, auth middleware,
worker/repository logging sites; `infra/modules/observability/`; verification record.

**Depends on:** S1 failure categories and S5–S8 resource/logging interfaces. Must precede live traffic.

**Tasks:**

1. Replace default HTTP span capture with an allowlist: server-generated request id, method,
   **matched route template**, status, duration, service, and release identifier. Log a fixed
   fallback category for unmatched paths, not the supplied path. Do not record query strings,
   authorization/cookie headers, bodies, presigned URLs, raw user agents, or event payloads.
2. Replace `ApiError::Internal(error=?e)`, free-form auth errors, worker error chains, and queue
   parse-error displays with stable classified codes plus correlation. SQL errors can contain
   row values; serde errors can echo malformed enum/string input; provider errors can include
   bodies. Review the actual logging sites, including `repository::record_usage` failures and
   startup failures. Preserve safe diagnostic categories without dumping raw exceptions.
3. Use structured JSON deterministically in cloud and bounded log verbosity. Limit high-volume
   failures/duplicate warnings so a broken queue cannot generate an unbounded log bill. Clinical
   identifiers are linkable data: omit patient/encounter ids from ordinary logs where correlation
   suffices. If an operational audit requires ids, place that schema in a restricted channel
   with its own purpose/access/retention; do not label it anonymous.
4. Provision API/worker/migration log groups with explicit retention, KMS encryption, and
   role-scoped writes/read access. Restrict live logs/exports to operators. Avoid debug SQL,
   PostgreSQL parameter-value/error-statement logging, SDK wire traces, and Bedrock model
   invocation body logging. Configure database logging to avoid clinical statements/binds and
   verify the result with a synthetic failing query.
5. Enable CloudTrail management-event coverage with log-file validation and protected audit
   storage. Use an account trail if AWS Organizations is not configured; the original “org
   trail” assumes an organization that has not been verified. Add scoped audio-bucket object
   data events when required for access evidence, estimate their volume, and inspect the actual
   resulting fields. They contain keys/principals and need restricted access even without audio.
6. Configure WAF log redaction/data protection for sensitive headers, query strings, and paths;
   disable request sampling unless separately protected and verified. Redacting logs does not
   automatically redact sampled requests. [AWS WAF data-protection guidance](https://docs.aws.amazon.com/waf/latest/developerguide/waf-data-protection-and-logging.html).
   Include rule-match detail/snippet fields and arbitrary client headers in the canary review.
   If the chosen destination/configuration cannot suppress sensitive request details, keep raw
   request logging disabled and use WAF metrics until a reviewed collection path exists.
7. Treat ALB access logs as sensitive metadata: they contain paths, query strings, IPs, and
   timing and cannot use the application's route-template redaction. If enabled, use dedicated
   restricted storage/short retention and prohibit credentials/clinical text in URLs. The
   documented ALB-to-S3 logging path supports SSE-S3, not the blanket SSE-KMS setting in the
   old table; make a scoped encryption exception for that log bucket instead of breaking
   delivery or weakening audio/state encryption. A different supported destination is an
   alternative only after verifying its delivery/encryption behavior.
   [ALB S3 access-log encryption requirements](https://docs.aws.amazon.com/elasticloadbalancing/latest/application/enable-access-logging.html).
8. Wire SNS/email notifications and verify subscription delivery. Minimum alarms: no healthy
   API target, sustained elevated 5xx, missing worker progress/heartbeat, oldest queued message
   age, DLQ visible messages > 0, RDS low storage/connections, and budget/forecast thresholds.
   Define missing-data behavior and evaluation windows; use native metrics where available and
   a small custom heartbeat/metric filter where ECS does not supply the required worker signal.
   A heartbeat must reflect completed polling/progress, not a detached timer that stays healthy
   while the handler is wedged. Alarm payloads contain no patient/job text.
9. Expose aggregate operational counters/timings: jobs completed/failed by kind, safe provider
   error category, provider latency, token/cache counters, usage-write failures, and worker
   poll progress. Avoid patient/job ids as metric dimensions. Document diagnostic queries for
   queued-but-not-enqueued/stale-running jobs and incomplete chunks; SQS depth cannot see a
   receipt that was never sent. The automated reconciliation/janitor remains Step 9 work.

**Privacy/security:** CloudTrail records AWS actions; the clinical event stream records changes.
Neither provides a complete record of who viewed/exported a note. Application read/export audit
and tamper-resistance policy remain explicit **pre-PHI** work. Do not describe mutable tables or
versioned buckets as immutable audit storage merely because normal code appends to them.

**Done when / validation:** inject fabricated canaries into bodies, invalid enum values, query
strings, headers, a fake token, and provider/DB errors. Inspect each configured destination
(including WAF samples and failure artifacts) for leakage. Application logs must omit those
values; raw edge/audit metadata behavior must match its documented restricted-data policy.
Verify log delivery with KMS policies, one real alarm notification, and a bounded worker-stall
signal. Store only pass/fail/category evidence in GitHub and the verification document.

---

## S10 — Make pull-request CI reproduce the local gates

**Objective and scope:** make a clean checkout prove the application and deployment contracts
without access to real AWS or paid AI/STT services.

**Where:** `.github/workflows/ci.yml`, `scripts/ci-services.sh`, temporary key generation,
toolchain pins, Docker checks; existing test suites and proposed focused deployment tests.

**Depends on:** S1–S2 changes; Terraform sources from S3–S9 for static validation.

**Tasks:**

1. Use `pull_request` with read-only contents permission and no OIDC/provider credentials.
   Never run untrusted PR code under `pull_request_target` with secrets or elevated permissions.
   Pin third-party actions to full commit SHAs, tools to tested versions, and container images
   to reviewed references. Cache only reviewed source/dependency artifacts with trust-aware keys.
2. Start Postgres, MinIO, and ElasticMQ with the documented local ports or explicit CI overrides.
   Poll readiness with a timeout instead of sleeping a fixed duration. Supply only dummy dev
   AWS credentials and emulator endpoints; ensure cloud credential resolution cannot be used
   accidentally. Use a disposable DB account with `CREATEDB` for `#[sqlx::test]`; this is a test
   privilege, not the production runtime role.
3. Generate disposable RSA private/public keys and a JWKS with `kid=dev-key-1` in an isolated
   working directory/location that existing test helpers can use. Reuse the dev mint utility
   to format JWKS after key generation. Do not depend on a developer's private key or upload
   generated keys as artifacts. Ensure concurrent test binaries see consistent keys for that run.
4. Run locked builds/tests and preserve the existing feature boundaries. Required commands are
   the target contract below; resolve existing formatting/Clippy failures in a clearly scoped
   prerequisite implementation change. Do not use `continue-on-error`, skip server integration
   targets, or silently reduce the whole-workspace gate to changed files.
5. Test SQLx metadata freshness separately from offline builds: migrate an isolated test DB and
   run the compatible sqlx CLI's prepare check for all relevant targets. Set offline mode off
   for that online metadata check and on for the no-DB compilation/image check. This catches
   stale `.sqlx` entries that would otherwise build while disagreeing with migrations.
6. Add dependency and container vulnerability scans, secret scanning, and `terraform fmt -check`,
   `init -backend=false`, and `validate` for each root. Use a pinned IaC scanner/policy test to
   catch public RDS/buckets, wildcard trust, unencrypted resources, unsafe endpoint/dev config,
   and missing retention/protection. Remote plans require trusted credentials and belong in the
   protected foundation workflow, not an arbitrary fork PR.
7. Add only meaningful new regression tests: cloud config rejection, bounded audio reads,
   safe logs, TLS/readiness, migration behavior, and worker lease/drain recovery. Reuse
   `auth_gate`, `audio_upload_gate`, `audio_complete_gate`, `worker_gate`, `ai_gate`,
   `repository_contract`, and the remote/sync/read/shift/snapshot gates for existing invariants.
8. Build and scan the exact candidate image from the tested commit. Establish a policy for
   vulnerabilities: exploitable critical/high issues block; a necessary exception must name
   impact, mitigation, owner, and expiry rather than becoming a permanent ignored warning.
   Do not upload DB contents, raw container logs, tokens, or clinical fixture outputs on failure.

**Target command set (after prerequisites exist):**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo audit

# Separate feature-boundary check: the non-server build must remain useful on-device.
cargo test -p er-ai --locked
cargo tree -p er-ai -e normal

# From the repository root, against an isolated migrated test database:
SQLX_OFFLINE=false cargo sqlx prepare --workspace --check -- --all-targets
```

The workspace flag targets the repository-root `.sqlx` directory rather than creating a second
cache under `crates/er-server`. [SQLx 0.8.6 CLI offline/cache guidance](https://github.com/transact-rs/sqlx/blob/v0.8.6/sqlx-cli/README.md).

Use job-level environment settings for `DATABASE_URL`, explicit `APP_ENV=dev`, the dummy AWS
credentials, and emulator endpoints. Do not source an arbitrary checked-out `.env` in CI.

**Privacy/security:** PR checks never call paid providers. Allow only synthetic/mock fixtures.
Treat source/artifact publication as a data boundary: S0's tracked-data review and S2's context
exclusions must pass before cache or image export is enabled.

**Done when / validation:** a clean branch/checkout runs the full gate with newly generated
keys and all three services; removing a service or staling query metadata causes a clear failure.
Cloud keys are unnecessary. Fork PRs cannot obtain OIDC/secret access. Required checks are
configured on the actual default branch; backlog/scan exceptions are explicit and reviewable.

---

## S11 — Wire OIDC deployment, migrations, and rollback

**Objective and scope:** make a successful default-branch commit produce a verified staging
release with short-lived AWS credentials and a concrete production approval path.

**Where:** `.github/workflows/infra.yml`, `deploy.yml`, deployment scripts, foundation/release
role policies and manifests, synthetic smoke client, `docs/ops/staging-deploy.md`.

**Depends on:** S0–S10; first-run role/secret/onboarding bootstrap may require an operator before
the automatic deployment path is enabled. Later application releases must need no console edits.

**Tasks:**

1. Manage the GitHub OIDC provider in Terraform, importing an existing account provider if
   necessary. Require `aud=sts.amazonaws.com` and an exact allowed `sub`. Inspect the actual
   repository's subject format: environment jobs use an environment subject rather than a
   branch subject, and newer repository configurations can include immutable repository/owner
   ids. Do not paste the old branch-only policy into an environment-gated workflow.
   [GitHub AWS OIDC configuration](https://docs.github.com/en/actions/how-tos/secure-your-work/security-harden-deployments/oidc-in-aws).
2. Protect staging and production environments and restrict their deployment branches/tags.
   Keep application staging promotion automatic after required checks; require production
   reviewers and prevent self-approval where supported. Verify these controls are available
   for the actual GitHub plan/repository visibility. If they are not, use an equivalent protected
   deployment boundary before enabling production, not a comment claiming “manual approval.”
3. Separate foundation apply from ordinary release. Foundation changes get a reviewed plan and
   the stronger environment-scoped role. Application jobs get only the release-role capabilities
   in S6. Restrict `iam:PassRole` to exact task/execution roles with
   `iam:PassedToService=ecs-tasks.amazonaws.com`; scope `RunTask` to approved families and the
   intended cluster/network. Inventory IAM actions that require `Resource="*"`, such as ECR
   authorization-token retrieval or some registration/list APIs, and justify them individually.
4. Protect workflow/IaC/deployment-template changes with required review/CODEOWNERS as appropriate.
   An actor allowed to deploy arbitrary code into a secret-bearing task can obtain its secrets;
   OIDC removes long-lived keys but does not remove that authority. Never check out and execute
   untrusted PR code after acquiring the deployment role.
5. Use a shared per-environment concurrency group across foundation and release mutation
   workflows. Do not cancel an in-progress migration/deploy when a new commit arrives; queue
   later runs, then skip superseded commits before mutation if policy allows. Use Terraform
   locks as well. Pin the source commit, not a moving branch, throughout build and deploy.
6. Publish the image once under an immutable commit tag, capture the registry digest, scan it,
   and record a non-secret release manifest: commit, digest, architecture, config/template
   revision, schema migration versions, environment, and previous accepted digest/config.
   Promote that digest; do not rebuild for production. If ECR is account-local, copy the exact
   image manifest/layers and verify its digest after transfer.
7. Render a one-shot migration task definition from a reviewed template plus the foundation
   manifest and candidate digest. Register it with `entryPoint=er-migrate`, the migration
   execution/task roles, private subnets/SG, and logging. Its credentials never leave AWS.
   Run it once before changing API/worker services.
8. Treat `RunTask` success as submission only. Check returned `failures`, capture the task ARN,
   wait with a bounded timeout, then inspect stopped reason and the migration container's
   `exitCode==0`. Missing exit code, image-pull failure, secret-injection failure, timeout, or
   nonzero exit blocks deployment. Record sanitized reasons; do not dump task environments or
   migration logs into public artifacts. Check for an already-running migration before retrying
   a workflow whose submission result was lost.
9. After migration succeeds, plan/apply the **release** root using the approved digest/config.
   Terraform creates the first API/worker services on first deployment and new task revisions
   on later releases. The migration task is intentionally ephemeral and outside those service
   resources. A stale saved plan must fail/replan safely; do not apply a plan for another digest
   or foundation-manifest version.
10. Enable the ECS deployment circuit breaker/rollback for supported service failures. Wait for
    stability with a workflow deadline, and verify desired/running counts, target health, task
    definition revisions, and each running container's image digest. Inspect rollout outcomes:
    a service can be stable because it rolled back to the old image. That is a failed new
    deployment, not success.
    [ECS circuit-breaker behavior](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/deployment-circuit-breaker.html).
11. Run a synthetic smoke stage using its separate scoped identity. Fetch only the synthetic
    Cognito credentials, authenticate, and hand tokens to the client in memory or a protected
    short-lived file. Disable shell tracing, mask secrets, avoid tokens in process arguments
    where practical, and delete ephemeral credentials through controlled cleanup. Do not upload
    full responses or generated notes. A `scripts/demo-remote.sh` wrapper plus a small
    `er-api` client example can reuse the actual DTOs and report assertion names/status only.
12. Persist the accepted release manifest only after smoke succeeds. For failures after rollout,
    redeploy the prior accepted digest/config through the same release root, wait for health,
    and run the short rollback smoke. Do not rerun a down migration. If no prior accepted
    release exists, leave workloads stopped or ingress closed and report the failed bootstrap.
    If schema compatibility makes code rollback unsafe, stop and execute a reviewed forward
    repair rather than restoring over accepted records.
13. Define production promotion now, but leave it disabled/unprovisioned until its contract and
    pre-PHI conditions are fulfilled. Approval must identify the exact digest, migration list,
    configuration diff, security/scan exceptions, and staging evidence. Obtain production OIDC
    credentials only after that gate. Production uses its own secrets/data and readiness checks;
    it must not inherit staging test users, password-flow exceptions, or synthetic cleanup jobs.

**Deployment state sequence:**

```text
CI passed → image digest verified → migration submitted → migration exit 0
   → release applied → exact revisions/digests healthy → authenticated smoke passed
   → accepted manifest recorded

migration failure → old services stay in place
rollout/smoke failure → previous compatible image/config → health + smoke again
first-deploy failure → closed/stopped staging + diagnostic evidence
```

**Privacy/security:** do not fetch DB/Deepgram credentials into GitHub just to run migrations or
health tests. The limited synthetic smoke credential is a separate, documented exception with
no production authority. Do not put live Terraform plans/state or raw logs in PR comments.
Protect artifacts with access/retention controls; sanitize even failed-run evidence.

**Done when / validation:** default-branch merge deploys staging without long-lived AWS keys;
an untrusted branch/PR and staging identity cannot assume production. A deliberately failing
test migration leaves services unchanged; a bad candidate image fails and recovers to the
previous accepted release. A completed migration followed by deployment failure can be retried
without duplicate schema changes. Terraform plans converge for foundation and release using
the accepted inputs, including after a rollback. Production approval behavior is verified
without deploying production workloads.

---

## S12 — Perform the staging gate and hand off the remaining work

**Objective and scope:** prove the deployed system's actual behavior and record enough evidence
for the next implementation step to continue without guessing.

**Where:** `scripts/demo-remote.sh`, proposed `er-api` smoke example,
`docs/ops/step8-verification.md`, deployment contract and runbook; follow-up references to Step 9/10.

**Depends on:** a successful S11 release, provisioned synthetic tenants, and approved synthetic
speech fixture. Use real providers here; keep PR CI offline.

**Tasks and gate:**

1. **Infrastructure convergence:** from a clean checkout with the accepted version pins and
   manifest, initialize both staging roots and obtain empty plans. Record safe resource settings,
   account/region, release digest, and verification time. “Clean checkout” still requires
   documented SSO/OIDC authority, backend bootstrap, populated secrets, DNS control, and users;
   it does not mean those external prerequisites magically appear.
2. **Auth and tenancy:** run S7's positive/negative Cognito cases over the staging URL. Exercise
   patient events, snapshots, note/export, shift ownership, audio completion/status, and job
   status with two orgs. A guessed foreign id must disclose neither existence nor contents.
   Exercise a valid but unprovisioned subject as well.
3. **Full lifecycle:** reuse `er-api` DTOs/client and the Step 6/7 ordering contract:
   register synthetic patient/shift → append `AudioStarted` → request audio intent → HTTPS PUT
   the approved WAV using its declared content type → complete → append `AudioStopped` → poll
   for transcript and summary → add synthetic objective data → request MDM → poll job → reload
   events → review/sign → export → end shift. Assert the exported note contains expected
   synthetic clinical facts and server-authored events. Do not put its text in CI output.
4. **Observe asynchronous work correctly:** use a bounded smoke deadline (initially 5 minutes
   per job, adjusted from observed behavior), independent of the CLI's 30-second observation
   window. Report timeout as incomplete work and retain its safe correlation; never resubmit a
   different job id merely because the first poll budget elapsed. Separately exercise the real
   CLI's documented reload-after-timeout behavior.
5. **Idempotency/restart:** retry the same event batch, chunk completion, and MDM job id. Check
   stable accepted ids and no duplicate derived events. Replace the worker during a synthetic
   job and reload from another client/API task. Verify eventual progress or an explicit terminal
   failure/DLQ outcome, with no silently lost accepted events. Provider re-execution after a
   crash can still cost money; record it without calling it a storage-idempotency failure.
6. **Live provider closure:** verify worker task-role Bedrock access to the configured ids,
   actual Deepgram transcription, Sonnet routing, and non-mock generation. Inspect safe
   metering rows/counters in AWS through a restricted diagnostic path: successful summary/MDM
   usage is attributed correctly and nonzero; repeat a cache-eligible synthetic MDM request to
   observe cache reads where supported. Do not pad prompts to force caching or treat a provider
   cache miss as a reason to bypass safety. Record any remaining model/cache discrepancy as a
   Step 7 gate failure to resolve. STT cost backfill/invoice reconciliation remains Step 9/10.
7. **Permission/TLS/privacy evidence:** complete S4–S9's negative IAM, certificate, public-access,
   byte-limit, logging-canary, and alarm checks. In particular, verify a normal presigned PUT
   from outside the VPC works while anonymous reads and changed key/content-type requests fail.
   Test natural presign expiry. Do not claim completion makes a PUT URL immediately unusable;
   the documented overwrite/version-binding gap remains a pre-PHI item.
8. **Deploy/rollback evidence:** run the migration-failure and candidate-image-failure exercises
   against synthetic staging, then restore the accepted release through S11. Record task/image
   revisions and whether any jobs remained queued, failed, or in the DLQ. Redrive only explicitly
   chosen synthetic messages after fixing their cause; never purge the queue to make the gate green.
9. **Operational cost/performance sanity:** record idle resource counts, a short real lifecycle's
   completion/queue timing, peak memory, DB connections, and the dated monthly run-rate estimate.
   Include NAT/endpoints/logging and provider spend. Billing data can lag; distinguish measured
   counters from estimates. This is a smoke-level sanity check, not Step 10's load benchmark or
   a new unsupported performance guarantee.
10. **Handoff:** write a short deployment/rollback/credential-rotation troubleshooting runbook,
    the exact safe commands/roles, alarm owner, known single-AZ/NAT limitations, secret onboarding,
    expected revocation behavior, and the pre-PHI register. Update only Step 8 completion/deviation
    references in implementation work; broader roadmap/OpenAPI cleanup stays in Step 10.

**Privacy/security:** diagnostic access stays inside approved AWS/operator boundaries. Evidence
contains assertions, versions, aggregate counts, and links to restricted records—not tokens,
presigned URLs, audio, transcripts, SQL dumps, or patient-like content. Even synthetic recording
fixtures need provenance and permission to distribute.

**Done when:** all essential Step 8 assertions above pass and evidence is dated/reviewable.
Unperformed live tests remain explicitly unperformed; “healthz is 200” cannot stand in for them.
The production root/path is reviewable but inactive, and remaining pre-PHI work is named with
owners and verifiable closure criteria.

---

## Scope fence

The boundary is **safe synthetic cloud deployment now; clinical deployment only after its
remaining prerequisites**. This table distinguishes optional work from work that is deferred
but still mandatory before patient use.

| Work | Classification / next owner | Trigger or closure condition |
|---|---|---|
| Cloud config guards, bounded audio/jobs, TLS, private RDS, scoped roles/secrets, redacted logs, migration/release recovery | Essential Step 8 | Required before the staging gate can pass |
| RLS with transaction-local tenant context across API and privileged worker paths | Step 9; pre-PHI defense-in-depth gate for this plan | Include snapshots/patients/events/audio/jobs/usage and relevant join tables; test pool reuse/rollback and runtime non-owner roles; do not paste one `SET LOCAL` into an incomplete path |
| Application read/export audit and protected retention of security evidence | Step 9 + security/privacy review; pre-PHI | Verify who accessed what, purpose/access model, integrity and retention; CloudTrail alone does not close this |
| Recording consent, BAAs, vendor retention/residency, risk analysis, records/hold policy | External expert review; pre-PHI | Written applicable determinations and configured controls; no clinical rollout based on this guide alone |
| Revocation SLA, clinical roles, minimum-necessary patient access, MFA policy | Step 9/product/security; pre-PHI | Exercise required deny/revocation cases and remove synthetic auth exceptions |
| Audio version/checksum binding or conditional write-once upload contract | Step 9; pre-PHI | An upload URL replay cannot change the bytes later processed for a completed chunk |
| Stuck-job/outbox reconciliation, incomplete-chunk janitor, safe provider-failure recovery | Step 9; required before unattended clinical operation | A DB-accepted receipt cannot remain invisible merely because enqueue failed and the client disappeared |
| PITR restore drill, recovery objectives, incident response, robust rotation | Step 9; pre-PHI | Restore into an isolated environment, prove replay, document reconciliation; never attach a live consuming worker or provider by accident |
| Per-clinician expensive-operation quotas | Step 9; before wider/unattended use | Bound AI/STT abuse/cost independently of hospital-shared IP rate limits |
| Worker autoscaling and/or separate AI queue | Optional Step 9/10 improvement | Queue age/MDM latency demonstrates need; prove concurrent STT claiming, lease behavior, provider quotas, and DB capacity first |
| Multi-AZ RDS and per-AZ NAT | Production availability decision | Agreed recovery/uptime requirement, not an automatic consequence of a paid-contract label |
| Extra interface endpoints, egress filtering, private CA/mTLS, per-tenant keys | Optional unless risk assessment/contract requires | Demonstrable isolation/compliance/performance benefit exceeds cost and operational burden |
| ARM image, RDS Proxy, Redis, orchestration platform change | Future consideration | Measured compute/connection/cache need; no architecture rewrite for the first deployment |
| S3 export workflow, archive retrieval, immutable clinical archive | Future product/policy work | A consumer and approved retention/access model exist |
| Load tests, cost dashboard, STT cost ledger, invoice reconciliation, OpenAPI/Phase 5 handoff | Step 10 (with Step 9 bookkeeping as already assigned) | Preserve the existing phase acceptance targets; use Step 8 evidence as input |

---

## Decisions and deviations recap

| Decision | Recommendation and reason |
|---|---|
| D8.1 — Environment boundary | Synthetic staging first; separate production account preferred; directory/account/state guards instead of relying on workspace names |
| D8.2 — State locking | Native S3 locking with a tested Terraform version; no new DynamoDB lock table |
| D8.3 — Release ownership | Foundation state plus narrow ECS release state; avoids broad infra permission on every app deploy and conflicting task-definition owners |
| D8.4 — Executables | One digest for API/worker/migration; explicit entrypoint overrides; add the missing migration binary |
| D8.5 — Queue topology | One standard queue/DLQ matching implemented `Job`; fixed worker count until concurrency/latency evidence supports scaling |
| D8.6 — Identity | Cognito access tokens plus DB subject bridge; no tenant authority in a client-writable claim; explicit synthetic automation exception |
| D8.7 — Data retention | Separate synthetic defaults from expert-approved production policy; no invented blanket HIPAA seven-year requirement |
| D8.8 — Encryption | Verified RDS TLS, encrypted backend API hop, service-compatible logging encryption, scoped KMS policies |
| D8.9 — Provider activation | Bedrock/Deepgram mandatory for cloud worker; no direct/mock fallback; verified model/profile ids and processing regions |
| D8.10 — Network cost | Two-AZ placement, one staging NAT, S3 gateway endpoint; paid interface endpoints selected by measured need or security requirement |
| D8.11 — Operations timing | Bring minimal privacy-safe logging, alarms, and rollback into Step 8; leave broader hardening/drills in Step 9 with explicit pre-PHI gates |
| D8.12 — Deployment proof | Exact running digest plus authenticated real lifecycle and failure recovery; health checks alone are insufficient |

---

## External setup and first-deploy walkthrough

This is the order an implementer follows **after creating the files described above**. Commands
are examples; no named script or Terraform root in this section exists merely because the
plan references it. Keep AWS/profile/domain values in the deployment contract, never guessed.

### E1 — Accounts, agreements, and access

Use IAM Identity Center/SSO for operators, root MFA, no root access keys, and no long-lived AWS
keys in GitHub. Record AWS BAA status and Deepgram procurement separately. Confirm service
eligibility, chosen model/profile access, DNS control, GitHub environment protection support,
and alarm ownership. Do not follow the older Cognito guide's suggestion to create a permanent
IAM user access key for routine setup.

```bash
aws sso login --profile ernote-staging-admin
aws sts get-caller-identity --profile ernote-staging-admin
```

Compare the returned account to the recorded expected account before provisioning. Inspect
existing Cognito/OIDC/DNS resources before deciding to import or create them.

### E2 — Local and image gate

Start Compose, wait for all three services, create disposable signing fixtures, and run S10's
checks. Build the candidate image from the reviewed source-only context. Test the API, worker,
and migration entrypoints, including all required native libraries and TLS trust material.
No staging database connection or provider credential is needed for this phase.

### E3 — State, foundation, and privileged bootstrap

Apply bootstrap under SSO and migrate its state as described in S3. Plan/review/apply staging
foundation with workloads disabled and ingress closed. Populate secret values through the
approved secure path. Run the private DB-role bootstrap, migrations, and synthetic identity
onboarding. Save the non-secret foundation manifest for the release workflow.

```bash
terraform -chdir=infra/envs/staging/foundation init
terraform -chdir=infra/envs/staging/foundation validate
terraform -chdir=infra/envs/staging/foundation plan
```

Review replacements, destroy actions, public routes, key/bucket policies, and monthly cost
before applying the concrete plan. Do not use broad `-target` applies as the routine dependency
solver. Missing credentials or model permission is a setup failure, not a reason to enable mocks.

### E4 — First release through GitHub

Run the same reviewed workflow that will handle later default-branch merges: test/build/push,
capture digest, run migration task, apply release, verify exact tasks, and execute synthetic
smoke. The first release has no automatic rollback target; failures leave ingress closed or
workloads stopped while preserving database/queue evidence.

Run a second accepted release so there is a real prior digest, then rehearse migration failure,
task-start failure, and application rollback. Record the result, not just the intended procedure.

### E5 — Close Step 8 honestly

Complete S12's evidence record and the remaining-work register. Keep staging synthetic, keep
production inactive, and pass the concrete runtime/resource/configuration contracts to Step 9.
The result is a reproducible cloud deployment with known limits and an auditable path to the
next gate.
