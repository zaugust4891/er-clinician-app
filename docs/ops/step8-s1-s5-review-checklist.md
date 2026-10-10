# Step 8 S1–S5 review and AWS setup checklist

Reviewed **2026-10-01 (America/New_York)** against `master`, HEAD
`5e0cfe4`, including the uncommitted S5 working tree. This covers **Phase 4,
Step 8**, using the [implementation guide](../planning/phase4-step8-implementation-steps.md).
Checked boxes below mean verified evidence; unchecked boxes are actions still to do.
The file-review checkboxes are left open for your own review.

**Current result: the local automated gates pass; S5 is NOT_READY, and the live
AWS gates are unverified.** Fix the bucket policies before the first apply.
The repository records that nothing has been applied. Today's read-only STS attempt
failed with `Token has expired and refresh failed`, so this review cannot independently
confirm what currently exists in the AWS account. No AWS resources were changed.

## What changed

| Substep | Change boundary | Current assessment |
| --- | --- | --- |
| S1 — startup/resource safeguards | Commit `eb410eb` (2026-09-11): process-specific config, verified DB TLS, audio caps, job deadlines, visibility renewal, bounded shutdown | Local behavior gates passed. Cloud TLS/permissions/provider checks remain open; the chosen Bedrock profile is still rejected by the model allowlist. |
| S2 — image and migrations | Commit `5394122` (2026-09-12): embedded `er-migrate`, pinned build/runtime images, three binaries, RDS CA, source allowlist | Migration tests and existing-image smoke checks passed. A fresh clean-source build and registry digest/provenance are not established by this review. |
| S3 — Terraform ownership/state | Commit `f8844db` (2026-09-14): bootstrap, five roots/keys, state roles, account guards, manifest | Offline checks passed. Live state protection/isolation/recovery gates remain open; bootstrap bucket policies share the S5 condition error described below. |
| S4 — network | Same `f8844db`: two AZs, six subnets, isolated DB placement, one NAT, separate SGs, S3 gateway | Offline checks passed. Private-task connectivity and outside-access denial remain unverified. The 8443 SG rule does not implement server TLS. |
| S5 — data | Uncommitted: `infra/modules/data/`, foundation `data.tf` and tests, data inputs/outputs/manifest keys, audio endpoint scope, config regression, verification doc/script | Configuration tests passed, but upload-policy behavior is incorrect and production audio retention is not gated on an explicit decision. Live acceptance is pending. |

The latest orchestration record is
`.agents/runs/20260918T005346.303255Z-step8-S5/03-gates.summary.md` and
`03-verdict.validated.json`: its five automated gates passed, but the review returned
`NOT_READY`. The earlier September 17 run failed; it is superseded by that later run
and today's reruns. Keep transient run artifacts out of the application change.

## Gates verified in this review

| Done | Gate | Result and practical limit |
| --- | --- | --- |
| [x] | `cargo fmt --all -- --check` | Passed. |
| [x] | `SQLX_OFFLINE=true cargo clippy --workspace --all-targets -- -D warnings` | Passed. |
| [x] | `cargo test --workspace --locked` with explicit dev variables and local Compose services | **302 passed, 0 failed, 0 ignored**. Includes config, audio, worker/AI, migration, tenancy and sync suites. No live AWS/provider inference exercised. |
| [x] | `terraform fmt -check -recursive infra/` | Passed with Terraform **1.10.4**. |
| [x] | Backend-disabled initialization and validation of all five roots in temporary copies | Passed using the committed AWS provider **6.64.0** locks. Does not test remote state or live AWS planning. |
| [x] | `scripts/tf-manifest-guards.sh` | **7/7**: rejects credential values/keys and invalid inputs; accepts a Secrets Manager ARN reference. |
| [x] | `scripts/tf-network-check.sh` | **23 runs**: network module 13, staging foundation 4, production foundation 6. AWS providers mocked. |
| [x] | `scripts/tf-data-check.sh` | **22 runs**: data module 12, staging foundation 4, production foundation 6. Foundation runs overlap the network script; these are not 45 unique tests. |
| [x] | `scripts/tf-lock-check.sh` against local MinIO | Second concurrent operation denied the lock with HTTP **412 PreconditionFailed**. Does not exercise AWS IAM, KMS or bucket policies. |
| [x] | Existing image `er-note-app:s2-verify` smoke inspection | `linux/amd64`, UID/GID `65532:65532`, API entrypoint, three executable binaries, shared libraries resolve, RDS CA checksum matches. Each exact binary entrypoint rejects missing `APP_ENV`/invalid migration config with networking disabled. |
| [ ] | Fresh image build from reviewed tracked sources; registry publication by digest | Not run. Existing image ID is `sha256:d874ced05317b0d32dd2ae7648919a6cce0b1a8b16a67e461d7d6865c43facfd`, created September 12; `RepoDigests` is empty. This local image ID is not an ECR manifest digest. |
| [ ] | AWS identity and all live S3/RDS/SQS/ECS/IAM/provider gates | SSO token expired; no live pass claimed. |

Migration coverage includes an empty database, a successful no-op rerun, rejection
of a changed applied checksum, and assertions that migration locking remains enabled.
It does not establish the later CI rule that failed migrations prevent service updates.

Rerun logs are local at `/tmp/er-note-review-cargo-test.log`,
`/tmp/er-note-review-tf-network.log`, `/tmp/er-note-review-tf-data.log` and
`/tmp/er-note-review-tf-lock.log`. They are temporary evidence, not committed artifacts.

## Fixes and decisions before provisioning or release

- [x] **Fix S5's audio encryption denies.** Completed **2026-10-09**;
  operator confirmed and saved code reviewed. In
  [audio.tf](../../infra/modules/data/audio.tf), both encryption denies now combine
  `Null = false` with `StringNotEquals` for the same header key, so missing SSE headers
  do not trigger these denies and explicit wrong algorithms/keys still do.
  Corrected a typo in the KMS header's `Null` condition during review.
  This records the code correction; matching mocked tests passed and live upload evidence remains pending.
  [AWS condition-operator documentation](https://docs.aws.amazon.com/IAM/latest/UserGuide/reference_policies_elements_condition_operators.html#Conditions_IfExists).
- [x] **Correct the matching S3 test (local assertions).** Completed **2026-10-09**;
  saved assertions reviewed in
  [data.tftest.hcl](../../infra/modules/data/tests/data.tftest.hcl). Both denies now
  require `Null = false` and `StringNotEquals` for their corresponding header keys;
  the test also rejects `StringNotEqualsIfExists`. Operator-provided output from
  `scripts/tf-data-check.sh` reports **22 passed, 0 failed**: data module 12,
  staging foundation 4, production foundation 6. Providers were mocked.
- [ ] **Verify actual audio upload allow/deny behavior.** Retained separately from
  the completed local test correction: a green mocked configuration assertion cannot
  prove uploads succeed. After provisioning and runtime IAM prerequisites, prove a
  normal headerless presigned upload succeeds under bucket-default KMS encryption.
  For wrong algorithm/key probes, use correctly signed requests carrying those headers
  so a signature error does not masquerade as proof of the encryption policy.
- [x] **Review/fix S3 bootstrap policies (local configuration).** Completed
  **2026-10-09**; saved code reviewed in
  [bucket_policies.tf](../../infra/bootstrap/bucket_policies.tf). The shared state and
  deployment-artifact guards now use `Null = false` with `StringNotEquals` for both
  encryption headers, and the comments are corrected. Operator-provided temporary-copy
  initialization/validation passed with locked AWS provider **6.64.0**.
- [ ] **Verify live bootstrap state and upload behavior.** After the reviewed bootstrap
  apply, verify actual backend state and `.tflock` writes as well as headerless uploads
  encrypted with the bucket-default KMS key. Explicit encryption headers on some clients
  do not validate the documented contract. Confirm signed wrong-algorithm/key uploads
  are denied. No live result is claimed by the local configuration correction above.
- [ ] **Require explicit production audio decisions.**
  [production variables](../../infra/envs/prod/foundation/variables.tf) defaults current
  and noncurrent audio retention to seven days. Only backup retention and Multi-AZ
  have enablement guards. Require approved current/noncurrent retention values before
  `enabled = true`, record the legal-hold decision, update the example and add rejection
  tests for omitted decisions. Keep production disabled until its contract is settled.
- [ ] **Resolve the Sonnet deployment mismatch.**
  [model.rs](../../crates/er-ai/src/model.rs) explicitly rejects
  `us.anthropic.claude-sonnet-4-6`, and its price mapping does not recognize that ID.
  The deployment contract chooses that US geographic profile for `us-east-2`.
  Extend the reviewed allowlist, pricing and config tests together after account-access
  verification. AWS currently lists Sonnet 4.6 in Ohio with geographic/global inference
  and no in-Region inference. Use the contract's approved US profile; record its
  destination Regions. [AWS model card](https://docs.aws.amazon.com/bedrock/latest/userguide/model-card-anthropic-claude-sonnet-4-6.html).
- [ ] **Reprice the full stack and resolve the budget.** Existing docs estimate S4 at
  $36.50/month and S5 at about $31/month, totaling about $67.50 before ECS, ALB, WAF,
  logs and providers. These are historical planning estimates, not current verified
  prices or bills. The reported $50 budget needs a full-stack decision before apply.
- [ ] **Keep the proposed commit reviewable.** Exclude transient `.agents/runs/`
  prompts/transcripts; consider an ignore rule. Review the still-tracked local DB,
  WAL/SHM, shift and dev-key artifacts under the existing S0 provenance task without
  publishing contents or deleting user data.
- [ ] **Update stale completion records after fixes.** The planning guide still calls
  S5 a plan; `infra/README.md` and `staging-deploy.md` still describe data provisioning
  as future work. Record authored code, offline passes and pending live evidence
  separately. Do not mark S5 complete merely because tests pass.

## AWS setup checklist, in execution order

### 1. Refresh and verify the existing operator setup — S0

Identity Center, an administrator assignment, a hosted zone, a budget and active AWS
BAAs were previously reported. Verify and record them; they are not established as
missing just because this workspace's token expired. See the
[deployment contract](deployment-contract.md) for the evidence register.

- [ ] Refresh `ernote-staging-admin` SSO and verify STS account/assumed-role identity.
  The recorded **candidate** staging account is `657615279057`, not a newly verified value.
- [ ] Confirm application Region `us-east-2`, account-owner/root MFA, and intended
  staging/production account boundary. Production account selection remains open.
- [ ] Recheck Bedrock's US Sonnet profile in **us-east-2**; record ID, status,
  destination Regions, model terms/first-use prerequisites and applied account quotas.
- [ ] After the account owner has reviewed model terms/payment, perform a synthetic
  invocation; later repeat using the actual worker task role. An `ACTIVE` profile is
  discovery evidence, not an invocation pass. Review IAM/SCP coverage for all destinations.
- [ ] Verify `zamos.co` zone `Z0831458W40X3W46HV0S` is public and properly delegated;
  identify registrar control and choose the staging API hostname for S8's ACM/DNS work.
- [ ] Record budget period/scope/thresholds/recipient, reprice the full stack, and choose
  a monitored alarm destination with verified delivery.
- [ ] Finish the existing synthetic retention decisions: 7-day audio, 30-day app logs,
  7-day RDS backups, plus a DB-record expiry/rebuild procedure. S3 expiry alone does
  not remove transcripts, events, snapshots or backups.
- [ ] Record AWS BAA acceptance dates/scope/evidence references and resolve the
  existing Deepgram/vendor agreement tasks. Production approvals and data policies
  remain separate from synthetic staging readiness.

Run AWS commands in a fresh shell that has not sourced the emulator `.env`:

```bash
unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN
aws sso login --profile ernote-staging-admin
aws sts get-caller-identity --profile ernote-staging-admin --region us-east-2
export AWS_PROFILE=ernote-staging-admin
export AWS_REGION=us-east-2

aws bedrock get-inference-profile \
  --inference-profile-identifier us.anthropic.claude-sonnet-4-6 \
  --profile ernote-staging-admin --region us-east-2
```

### 2. Establish protected Terraform state — S3

No operator `terraform.tfvars`, `backend.hcl` or local state files were found in the
five roots during this review. Fill verified inputs locally from the committed examples.
Follow [staging-deploy.md](staging-deploy.md) after correcting the policies above.

- [ ] Fill bootstrap account/Region/project inputs and narrow state-role trust to the
  intended stable Identity Center role ARNs where practical.
- [ ] Review and apply **bootstrap only**: state/artifact buckets, rotating KMS key,
  five state-access policies and five operator roles.
- [ ] Record bucket names and key ARN, enable bootstrap's commented S3 backend, and
  migrate bootstrap state to its remote key. Supply the **KMS key ARN**, not its alias.
- [ ] Verify the remote copy before handling local state/backups; keep state and plans
  outside Git and images.
- [ ] Initialize the four environment backends at their distinct committed keys;
  fill staging foundation inputs from bootstrap outputs. Keep both prod roots disabled.
- [ ] Prove real S3 lock contention, version recovery on a disposable object, transport/
  encryption/public-access controls, staging-to-prod denial and the reverse direction,
  and a wrong-account refusal. Record assertion results and dates.
- [ ] Verify a versioned foundation manifest contains identifiers/secret ARNs only;
  record its version/checksum and pin the manifest version for a later release.

Native S3 locking needs separate Get/Put/Delete permissions on the `.tflock` object;
normal state access does not require deleting the state object.
[HashiCorp S3 backend documentation](https://developer.hashicorp.com/terraform/language/backend/s3).

### 3. Review foundation inputs and finish its dependencies — S4/S5

- [ ] Pin the reviewed AZ pair, check CIDR overlap, retain closed client ingress and
  choose the one-NAT/endpoint availability and cost tradeoff explicitly.
- [ ] Query PostgreSQL 16 minors and orderable `db.t4g.small`/gp3 options in `us-east-2`;
  select a supported minor and record 50 GiB allocation/100 GiB cap and backup windows.
- [ ] Review synthetic audio current/noncurrent expiry and multipart cleanup separately;
  review 300-second queue visibility, 20-second polling, three deliveries and 4/14-day
  queue/DLQ retention. Match queue names and lease settings to application configuration.
- [ ] Finish S6–S9 foundation components before expecting the complete live gate:
  scoped roles/secrets/DB bootstrap, Cognito, ECR/cluster/edge, and telemetry.
  The current foundation creates network and data; release roots still have no ECS services.
- [ ] Apply the corrected, reviewed synthetic foundation with workloads disabled and
  application ingress closed until image, credentials, migrations and telemetry are ready.
  Do not manually create competing resources outside Terraform ownership.

Read-only RDS discovery commands after SSO verification:

```bash
aws rds describe-db-engine-versions --engine postgres --region us-east-2 \
  --query 'DBEngineVersions[?starts_with(EngineVersion, `16.`)].EngineVersion'
aws rds describe-orderable-db-instance-options --engine postgres \
  --db-instance-class db.t4g.small --region us-east-2 \
  --query 'OrderableDBInstanceOptions[?StorageType==`gp3`].{version:EngineVersion,storage:StorageType,min:MinStorageSize,max:MaxStorageSize}'
```

### 4. Collect the live acceptance evidence — S1/S3/S4/S5

- [ ] Inspect actual routes, SG references, task ENIs without public IPs and RDS
  `PubliclyAccessible = false`. Require failure of outside-VPC direct task/DB access.
- [ ] Use a disposable approved diagnostic task in **each private task subnet** to
  prove DNS, ECR pulls, secret injection, safe log delivery, queue lookup and verified
  TLS to RDS/JWKS/providers. Check wrong-CA and wrong-hostname rejection. This needs
  S6 roles and S8 compute; use [network-verification.md](network-verification.md).
- [ ] Inspect RDS encryption/key, `rds.force_ssl = 1`, backup window/retention, deletion
  protection and final-snapshot settings; observe `LatestRestorableTime` advancing.
- [ ] Inspect audio public-access blocks, ownership, versioning, KMS/Bucket Key metadata,
  lifecycle rules, TLS restrictions and corrected allow/deny upload behavior.
- [ ] Prove a normal externally presigned PUT succeeds with Content-Type only; inspect
  its stored KMS metadata. Verify signed wrong-algorithm/key requests, unsigned reads,
  insecure transport and expired URLs fail. Keep URLs and audio out of evidence logs.
- [ ] Inspect both queues' key/redrive/retention settings; test the real API/worker KMS
  grants, including the encryption context used with S3 Bucket Keys.
- [ ] Prove API/worker roles cannot change bucket protection/lifecycle, purge queues,
  disable/delete keys, modify RDS, or access snapshots/backups. Worker cannot upload audio.
- [ ] Add non-secret assertion results/date/Region/image digest to
  [data-verification.md](data-verification.md) and the network/deployment records.

Use the exported `db_parameter_group_name` when querying `rds.force_ssl`.
The existing data runbook exports `DATA_PG_GROUP` but its example command reconstructs
the group name; using the actual output avoids assumptions about naming changes.

### 5. Remaining AWS work after S5

These are still implementation dependencies, not additional S1–S5 local failures.

| Done | Later step | Still needed |
| --- | --- | --- |
| [ ] | S6 | Separate API/worker/migration/onboarding task and execution roles; scoped S3/SQS/KMS/Bedrock grants; Secrets Manager metadata and out-of-state secret population; separate DB owner/runtime logins and grants. Bedrock uses IAM; Deepgram needs its provider credential in Secrets Manager. |
| [ ] | S7 | Adopt/provision Cognito pool/client configuration; provision synthetic organizations/clinicians; prove real access-token and tenant behavior. |
| [ ] | S8 | ECR and immutable candidate digest, ECS cluster/task definitions/services, HTTPS server listener/ALB targets, ACM certificate/DNS/WAF, private placement and explicit worker/migration entrypoints. |
| [ ] | S9 | Retained log groups, security evidence destinations, monitored alarms and privacy-aware telemetry. |
| [ ] | S10 | PR CI reproducing application, SQLx, image and Terraform checks. No `.github/` workflow directory exists in the current tree. |
| [ ] | S11 | GitHub OIDC/deploy role and environment controls; manifest/digest-pinned release; one-shot private migrations; failed-migration deployment stop; rehearsed application rollback. |
| [ ] | S12 | Full synthetic remote lifecycle using Cognito, S3, SQS, Deepgram and Bedrock; task replacement/rollback evidence and handoff. |

Keep the existing Step 9 deferrals visible: a restore rehearsal and audio version/checksum
binding are required before patient use. S3 versioning does not make a presigned upload
immutable. Today's local passes do not close those deferred requirements.

## Files to review for each S1–S5 substep

Paths below are existing files, relative to the repository root. Read each row alongside
the matching numbered task in the implementation guide. For paired environment files,
check both staging and production; production enablement is especially important.

### S1 — cloud startup and bounded resource use

| Done | Task | Files | Review focus |
| --- | --- | --- | --- |
| [ ] | S1.1 environment/process split | `crates/er-server/src/config.rs`, `crates/er-server/src/main.rs`, `crates/er-server/src/bin/worker.rs`, `crates/er-server/src/bin/migrate.rs`, `crates/er-server/tests/config_gate.rs`, `.env.example` | Explicit `APP_ENV`; dev-only dotenv; each process requires only its own dependencies. `crates/er-server/src/` and `crates/er-server/tests/` here are under `crates/er-server/`. |
| [ ] | S1.2 endpoint/credential guards | `crates/er-server/src/config.rs`, `crates/er-server/src/aws.rs`, `crates/er-server/tests/config_gate.rs` | Reject dev endpoints/auth/static credentials in cloud; use ECS credentials and preserve legitimate container credential settings. |
| [ ] | S1.3 model routing/pricing | `crates/er-ai/src/model.rs`, `crates/er-ai/src/transport.rs`, `crates/er-server/src/config.rs`, `crates/er-server/tests/config_gate.rs` | Approved IDs/family and cost mapping; US profile support is still a deployment blocker. |
| [ ] | S1.4 DB TLS/pools | `crates/er-server/src/db.rs`, `crates/er-server/src/config.rs`, `certs/rds-global-bundle.pem` | `verify-full`, CA/hostname checking, limits/timeouts and safe errors; live negative TLS probes pending. |
| [ ] | S1.5 audio bounds | `crates/er-server/src/audio.rs`, `crates/er-server/src/routes.rs`, `crates/er-server/src/state.rs`, `crates/er-server/tests/audio_limits.rs`, `crates/er-server/tests/audio_complete_gate.rs` | 16 MiB cap, content-type agreement, unknown-length streaming cap and download deadline. |
| [ ] | S1.6 operation/job deadlines | `crates/er-server/src/config.rs`, `crates/er-server/src/worker.rs`, `crates/er-server/src/aws.rs`, `crates/er-server/tests/worker_gate.rs`, `crates/er-server/tests/ai_gate.rs`, `crates/er-ai/src/transport.rs` | Total retry-inclusive deadlines; terminal failures preserve durable state and reach DLQ correctly. |
| [ ] | S1.7 visibility renewal | `crates/er-server/src/worker.rs`, `crates/er-server/src/config.rs`, `crates/er-server/tests/worker_gate.rs` | 60-second renewal/300-second visibility, bounded work, lease-loss handling and no premature acknowledgement. |
| [ ] | S1.8 shutdown | `crates/er-server/src/bin/worker.rs`, `crates/er-server/src/worker.rs`, `crates/er-server/tests/worker_gate.rs` | Stop receives on SIGTERM, 100-second drain, leave unfinished work retryable; future ECS stopTimeout must agree. |

Reference: [server startup README](../../crates/er-server/README.md).

### S2 — reproducible package and migrations

| Done | Task | Files | Review focus |
| --- | --- | --- | --- |
| [ ] | S2.1 migration executable | `crates/er-server/Cargo.toml`, `crates/er-server/src/bin/migrate.rs`, `crates/er-server/migrations/*.sql` | Embedded locked migrations, nonzero failures, no migrations in API/worker startup. |
| [ ] | S2.2 privileges/time bounds | `crates/er-server/src/bin/migrate.rs`, `crates/er-server/src/db.rs`, `crates/er-server/README.md` | 15-second lock/300-second statement/600-second run limits; DB login/bootstrap privilege work belongs to S6. |
| [ ] | S2.3 pins/architecture | `Dockerfile`, `rust-toolchain.toml`, `Cargo.lock` | Rust 1.97.1, cargo-chef/base digests, amd64 release path; package-manager downloads are not frozen merely by base-image pins. |
| [ ] | S2.4 offline compilation | `Dockerfile`, `.sqlx/*.json`, `crates/er-server/Cargo.toml` | `SQLX_OFFLINE=true`, `--locked`, required query metadata in compilation stages. |
| [ ] | S2.5 runtime contents | `Dockerfile`, `certs/rds-global-bundle.pem`, `crates/er-server/README.md` | Three binaries, non-root user, shared libraries/CA and trust-bundle rotation. |
| [ ] | S2.6 entrypoints | `Dockerfile`, `crates/er-server/README.md` | API default; worker/migrate absolute `entryPoint` overrides in future ECS definitions. |
| [ ] | S2.7 source context | `.dockerignore`, `Dockerfile` | Allowlist excludes keys/env/DB/state/audio/docs and intermediate layers use the approved context. |
| [ ] | S2.8 artifact tracking | `.gitignore`, `docs/ops/deployment-contract.md` | Existing tracked DB/dev-key artifacts require provenance/remediation; ignores do not untrack them. |

### S3 — state and ownership

| Done | Task | Files | Review focus |
| --- | --- | --- | --- |
| [ ] | S3.1 versions/locking | `.terraform-version`, `infra/bootstrap/versions.tf`, each environment root's `versions.tf`, `.terraform.lock.hcl`, `backend.tf`, `scripts/tf-lock-check.sh` | Tested versions, committed locks, native `use_lockfile`, emulator versus real-S3 proof. |
| [ ] | S3.2 protected storage/principals | `infra/bootstrap/state.tf`, `infra/bootstrap/bucket_policies.tf`, `infra/bootstrap/iam.tf`, `infra/bootstrap/variables.tf`, `infra/bootstrap/locals.tf` | Rotation/versioning/public-access/retention/destroy protection, trust and per-root isolation; fix the header condition error. |
| [ ] | S3.3 backend migration/recovery | `infra/bootstrap/backend.tf`, `infra/bootstrap/outputs.tf`, `.gitignore`, `docs/ops/staging-deploy.md` | Local-to-remote migration, key ARN input, secure state handling and version recovery. |
| [ ] | S3.4 root separation | `infra/bootstrap/iam.tf`, `infra/bootstrap/locals.tf`, `infra/envs/{staging,prod}/{foundation,release}/backend.tf` | Five keys/policies; separate lock operations; explicit cross-environment denial. |
| [ ] | S3.5 account/Region guards | `infra/bootstrap/{providers,guard,variables}.tf`, `infra/envs/{staging,prod}/{foundation,release}/{providers,guard,variables}.tf` | Expected account, allowed Regions, wrong-account rejection and disabled production. |
| [ ] | S3.6 manifest | `infra/modules/manifest/{main,variables,outputs}.tf`, both foundation roots' `manifest.tf`/`locals.tf`, both release roots' `manifest.tf`/`locals.tf`, `scripts/tf-manifest-guards.sh` | Non-secret allowlist, version/checksum, required keys and environment/account/schema guards. |
| [ ] | S3.7 ownership boundary | `infra/README.md`, `infra/envs/{staging,prod}/{foundation,release}/`, `docs/ops/deployment-contract.md` | Durable resources foundation-owned; release services/task definitions only; no credential-bearing remote-state consumption. |

### S4 — network and required connections

| Done | Task | Files | Review focus |
| --- | --- | --- | --- |
| [ ] | S4.1 VPC/subnets | `infra/modules/network/main.tf`, `infra/modules/network/variables.tf`, `infra/modules/network/outputs.tf`, both foundation roots' `network.tf` | DNS, six subnets/two AZs, isolated DB routes/subnet group; AZ order and CIDRs. |
| [ ] | S4.2 NAT/private placement | `infra/modules/network/main.tf`, `infra/modules/network/variables.tf`, `docs/ops/network-verification.md`, `infra/modules/data/database.tf` | One-NAT tradeoff; RDS actually private; ECS no-public-IP setting still belongs to S8. |
| [ ] | S4.3 S3/optional interfaces | `infra/modules/network/endpoints.tf`, `infra/modules/network/variables.tf`, `infra/modules/network/outputs.tf`, both foundation roots' `network.tf`/`locals.tf` | Free S3 gateway, audio bucket scope added by S5; optional paid interfaces and cost. |
| [ ] | S4.4 endpoint policies/startup | `infra/modules/network/endpoints.tf`, `infra/modules/network/tests/network.tftest.hcl`, `infra/modules/network/README.md` | Private DNS/task SG ingress, actual resource scopes, paired ECR endpoints and S3 layer access. |
| [ ] | S4.5 connection matrix | `infra/modules/network/security_groups.tf`, `infra/modules/network/README.md`, `docs/ops/network-verification.md` | Closed client ingress, ALB-to-API HTTPS port, task-to-DB 5432 and required outbound 443. |
| [ ] | S4.6 one-shot access | `infra/modules/network/security_groups.tf`, `infra/modules/network/outputs.tf`, both foundation roots' `locals.tf`, `docs/ops/network-verification.md` | Dedicated migration/onboarding SGs; approved per-subnet diagnostic tasks still need S6/S8. |
| [ ] | S4.7 verified outbound TLS | `crates/er-server/src/auth/keys.rs`, `crates/er-server/src/state.rs`, `crates/er-server/src/config.rs`, `crates/er-pipeline/src/deepgram.rs`, `crates/er-ai/src/transport.rs` | Fixed/admin-controlled endpoints, normal certificate checking, live negative TLS tests pending. |

Gate files: `scripts/tf-network-check.sh`,
`infra/modules/network/tests/network.tftest.hcl`,
`infra/envs/{staging,prod}/foundation/tests/network.tftest.hcl`.

### S5 — encrypted and recoverable data

| Done | Task | Files | Review focus |
| --- | --- | --- | --- |
| [ ] | S5.1 RDS configuration | `infra/modules/data/database.tf`, `infra/modules/data/variables.tf`, both foundation roots' `data.tf`/`infra/modules/data/variables.tf`/`terraform.tfvars.example` | Supported PG16 minor/class/gp3, capped autoscaling, private encrypted instance, deletion protection/final snapshot. |
| [ ] | S5.2 backups/key lifetime | `infra/modules/data/database.tf`, `infra/modules/data/main.tf`, `infra/modules/data/variables.tf`, production foundation `infra/modules/data/variables.tf`, `docs/ops/data-verification.md` | Explicit staging/prod backup decisions, advancing restore time, retained backups' key availability; 35-day retention exceeds a 30-day deletion window. |
| [ ] | S5.3 DB TLS/rotation | `infra/modules/data/database.tf`, `crates/er-server/src/db.rs`, `certs/rds-global-bundle.pem`, `crates/er-server/README.md` | `rds.force_ssl=1` plus verified client TLS and CA rotation; inspect the applied parameter group. |
| [ ] | S5.4 audio security | `infra/modules/data/audio.tf`, `infra/modules/data/tests/data.tftest.hcl`, `crates/er-server/src/audio.rs`, `crates/er-server/src/routes.rs`, `docs/ops/data-verification.md` | **Blocking:** missing-SSE-header uploads must work with default KMS; wrong overrides denied; signed external uploads remain possible. Server `crates/er-server/src/` paths are under `crates/er-server/`. |
| [ ] | S5.5 lifecycle/production decisions | `infra/modules/data/audio.tf`, `infra/modules/data/variables.tf`, `infra/envs/prod/foundation/variables.tf`, `infra/envs/prod/foundation/terraform.tfvars.example`, `infra/envs/prod/foundation/tests/data.tftest.hcl`, `docs/ops/deployment-contract.md` | Separate current/noncurrent/multipart clocks, no archive transitions; **blocking:** gate production on explicit audio retention decisions. |
| [ ] | S5.6 overwrite race | `infra/modules/data/README.md`, `infra/modules/data/audio.tf`, `docs/ops/data-verification.md`, `crates/er-server/src/routes.rs`, `crates/er-server/src/worker.rs` | Versioning enables recovery; completion/worker still do not bind a version/checksum. Document Step 9 deferral accurately. |
| [ ] | S5.7 queue/DLQ | `infra/modules/data/queues.tf`, `infra/modules/data/variables.tf`, `crates/er-server/src/config.rs`, `crates/er-server/src/aws.rs`, `crates/er-server/tests/config_gate.rs` | Standard queue and derived `-dlq`, 300/20-second settings, three receives, 4/14-day retention, source-restricted redrive, KMS/TLS. |
| [ ] | S5.8 key/copy boundaries | `infra/modules/data/main.tf`, `infra/modules/data/outputs.tf`, `infra/modules/data/README.md`, `infra/bootstrap/state.tf`, `docs/ops/data-verification.md` | Data key separate from state/security evidence; S6 grants still missing; audio, transcripts, events and backups are distinct copies. |
| [ ] | S5.9 exports | `infra/modules/data/README.md`, `infra/modules/data/audio.tf`, `crates/er-server/src/routes.rs` | No unsupported export bucket; note export remains HTTP. |

Integration/gate files: `scripts/tf-data-check.sh`,
`infra/modules/data/tests/data.tftest.hcl`,
`infra/envs/{staging,prod}/foundation/tests/data.tftest.hcl`, both foundation roots'
`locals.tf`/`outputs.tf`/`network.tf`, both release roots' `locals.tf`, and
`infra/modules/network/outputs.tf`. Review the data manifest and S3 gateway wiring
alongside the resources; neither manifest references nor endpoint policy scope grant
the runtime IAM permissions that S6 must provide.
