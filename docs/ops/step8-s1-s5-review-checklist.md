# Step 8 S1–S5 review and AWS setup checklist

Updated **2026-10-14 (America/New_York)** against application HEAD `329f4ed` and the
current Terraform working tree, after the owner delegated verification. Earlier
October 1/9/11 evidence is historical. Checked boxes record completed evidence;
unchecked boxes identify remaining actions. File-reading rows at the end remain
available for the owner's manual review; automated checks do not certify every
review focus as a live pass.

**Current result: local gates pass; approved bootstrap is applied and protected remote
state is verified. Application network/data/workloads remain undeployed.** The current
full local suite is 304 passing tests; Sonnet has one approved synthetic administrator
invocation. The $50 budget is unchanged; the reviewed always-on scenario is $166.32/month
before provider/traffic charges. Domain registration and S6–S9 implementation remain
open. Production stays disabled. Details and limits:
[audit evidence](step8-audit-2026-10-14.md),
[cost worksheet](staging-cost-review-2026-10-14.md),
[implementation guide](../planning/phase4-step8-implementation-steps.md).

## What changed

| Substep | Change boundary | Current assessment |
| --- | --- | --- |
| S1 — startup/resource safeguards | Commit `eb410eb` (2026-09-11), Sonnet routing/pricing/config correction `6845878` (2026-10-09) | Local behavior gates passed; US-profile support and explicit cloud model IDs now pass targeted tests. Ohio rates and one administrator invocation verified October 14; cloud task TLS/runtime permissions remain open. |
| S2 — image and migrations | Commit `5394122` (2026-09-12): embedded `er-migrate`, pinned build/runtime images, three binaries, RDS CA, source allowlist | Migration tests and existing-image smoke checks passed. Fresh clean-source image build and six startup probes passed October 14; ECR publication/digest remains open. |
| S3 — Terraform ownership/state | Commit `f8844db` (2026-09-14), bootstrap encryption correction `91dce5e` (2026-10-09) | Offline checks passed; header-presence conditions corrected and bootstrap validation passed. Approved bootstrap apply, remote-state migration, bucket probes, real locking/recovery and existing cross-root-object denials passed October 14. Published-manifest denial remains open. |
| S4 — network | Same `f8844db`: two AZs, six subnets, isolated DB placement, one NAT, separate SGs, S3 gateway | Offline checks passed. Private-task connectivity and outside-access denial remain unverified. The 8443 SG rule does not implement server TLS. |
| S5 — data | Source committed in `3698415`; October 9 follow-ups correct audio encryption conditions, tests and production-retention enablement | Latest audit data gate: 25 runs passed, including the shared encryption-policy module. Production requires explicit current/noncurrent retention and remains disabled. Actual approvals, upload/IAM/backup checks and live acceptance are pending. |

The historical September orchestration record is
`.agents/runs/20260918T005346.303255Z-step8-S5/03-gates.summary.md` and
`03-verdict.validated.json`: its five automated gates passed, but the review returned
`NOT_READY` before the October 9 corrections. The earlier September 17 run failed.
The fixes and current results below supersede those local defect descriptions.
The formal orchestration review was not rerun; bootstrap live acceptance is now
recorded separately, and application live acceptance remains pending. These transient
records are retained locally and excluded from tracking by the cleanup below.

## Current local and bootstrap gates — 2026-10-14

| Done | Gate | Result and practical limit |
| --- | --- | --- |
| [x] | Rust formatting and locked workspace Clippy with warnings denied | Passed. |
| [x] | Locked workspace tests with explicit dev variables and isolated emulators | **304 passed, 0 failed, 0 ignored**. Synthetic local integrations. |
| [x] | `er-ai --no-default-features` | **18 passed**; overlaps workspace coverage. |
| [x] | Terraform formatting and all five backend-disabled root validations | Passed: Terraform **1.10.4**, committed provider **6.64.0**. |
| [x] | Manifest guard script | **7/7**, plus provider-failure injection correctly fails. |
| [x] | Network gate | **25 passed**: network 13, staging 4, prod 8. Mocked AWS. |
| [x] | Data gate | **25 passed**: shared policy 1, data 12, staging 4, prod 8. Foundation cases overlap network gate. |
| [x] | Bootstrap policy test | **1 mocked plan-only test passed**. |
| [x] | Local and actual AWS lock collisions | Both returned **412 PreconditionFailed**; AWS lock encrypted and normally released. |
| [x] | Fresh image from tracked HEAD and runtime probes | `er-note-app:checklist-20261014-329f4ed`; Linux/amd64, non-root, three binaries/CA verified, six startup rejections passed. |
| [ ] | Registry publication and immutable ECR manifest digest | Local image ID is not an ECR digest; S8 remains pending. |
| [x] | AWS identity, root MFA, Sonnet metadata/quotas/pricing | Independently verified through AWS APIs. |
| [x] | One approved synthetic Sonnet request | Response `OK.`, 11 input/5 output tokens, estimated **$0.0001188**. Administrator identity only. |
| [x] | Approved bootstrap and remote state | **32 additions**, remote migration and no-change plans; five scoped backends initialized. |
| [ ] | Application runtime, network, RDS, audio, queues and worker provider gates | Foundation/workloads and S6–S9 prerequisites remain undeployed. |

Migration tests include empty/no-op databases, checksum mismatch rejection and enabled
migration locking. Logs and exact image ID are in the [dated audit](step8-audit-2026-10-14.md).
These local results do not establish CI deployment or application cloud acceptance.

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
- [x] **Verify live bootstrap state and upload behavior.** Completed **2026-10-14**;
  see the [audit](step8-audit-2026-10-14.md). Actual encrypted remote state, scoped lock
  writes, headerless/correct-key uploads, signed wrong-algorithm/real-wrong-key/omitted-key/
  SSE-C denials, signed HTTP/unsigned-read denials and disposable recovery passed.
- [x] **Require explicit production audio decisions (implementation and documentation).**
  Completed **2026-10-09**; saved guards/tests/documentation reviewed. Seven-day
  current/noncurrent defaults removed from
  [production variables](../../infra/envs/prod/foundation/variables.tf); all four enablement
  guards are present: backup retention, Multi-AZ, current audio, and noncurrent audio.
  Separate audio-omission tests added and existing fixtures updated. Latest operator-provided
  `scripts/tf-data-check.sh` run reports **24 passed, 0 failed**: data module 12,
  staging foundation 4, production foundation 8, with mocked providers. This successful
  run supersedes the prior failed attempts. Descriptions/comments corrected; the production
  example keeps audio values `null` and `enabled = false`. The deployment contract records
  the pending retention/legal-hold decision and distinguishes test values from approved inputs.
- [ ] **Approve production audio retention and legal-hold requirements.** Record approved
  current/noncurrent retention values, legal-hold requirements, approver, date and evidence
  reference in the deployment contract before enabling production. The completed guards
  enforce explicit values; they do not establish approval or implement legal holds.
  Keep production disabled until its contract is settled.
- [x] **Resolve the Sonnet deployment mismatch (local code and tests).** Verified
  **2026-10-09** against committed HEAD `6845878`. The exact US Sonnet profile is
  accepted and priced in [model.rs](../../crates/er-ai/src/model.rs); unreviewed
  global/geographic profiles, ARNs and family switches remain rejected. Cloud workers
  require both Bedrock model IDs explicitly, while dev defaults remain intact.
  Passed: `cargo fmt --all -- --check`; `SQLX_OFFLINE=true cargo test --locked -p er-ai
  --features bedrock` (**19 tests**); the same crate with `--no-default-features`
  (**18 tests**, overlapping coverage under a different feature configuration);
  `SQLX_OFFLINE=true cargo test --locked -p er-server --test config_gate --bin er-worker`
  (**6 config tests + 2 worker tests**); `SQLX_OFFLINE=true cargo clippy --locked
  --workspace --all-targets -- -D warnings`. All tests passed; no live provider invocation
  or database integration suite was run in this verification. Regional price constants
  match [Anthropic's documented base prices and 10% regional premium](https://platform.claude.com/docs/en/about-claude/pricing).
- [x] **Verify Sonnet administrator access, Ohio prices and one synthetic invocation.**
  Completed **2026-10-14**: profile/destinations, first-use form, applied quotas,
  source-Region rates and one owner-approved request verified. Agreement subsequently
  reports AVAILABLE. See [audit evidence](step8-audit-2026-10-14.md).
- [ ] **Repeat provider access with the actual worker role.** S6/S8 must implement the
  role and verify destination-Region IAM/SCP permissions; administrator success does
  not prove worker access.
- [x] **Reprice the reviewed full-stack scenario.** Official AWS price dimensions and
  assumptions recorded in the [cost worksheet](staging-cost-review-2026-10-14.md):
  **$166.32/month** before providers/traffic/additional operational choices.
- [x] **Choose the cost planning direction.** Owner selected planning for the **$166+**
  always-on scenario on **2026-10-14**, with deployment pending.
- [ ] **Resolve the full-stack budget and variable provider costs.** Existing monthly
  budget is **$50** and unchanged. Final limit/provider/traffic allowances and approval
  of the actual budget update remain open; this is separate from the chosen direction.
- [x] **Keep generated runs out of the proposed commit.** Updated **2026-10-09**:
  `/.agents/runs/` is ignored and its 65 previously tracked files are removed from
  the index while preserving local copies. The implementation guide is explicitly
  allowed through its existing ignore rule so the durable S5 record can be reviewed.
  Source/history references and remaining artifact decisions are recorded in
  [repository-artifact-provenance.md](repository-artifact-provenance.md).
- [x] **Classify the current local database and shift file.** Owner confirmed
  **2026-10-10** that current `er_notes.db` records and `shift.json` are synthetic only.
  This is owner-provided evidence; the assistant did not inspect their contents.
  Historical provenance and development-key usage are recorded separately below.
- [x] **Establish historical database/shift data provenance.** Owner confirmed
  **2026-10-10** that database/shift data were synthetic throughout the project's
  history. This records owner-provided origin evidence for the database, WAL/SHM
  companions and shift file; no independent content or residual-page scan is claimed.
- [x] **Establish development-key intent and usage.** Owner confirmed **2026-10-10**
  that the private/public signing keys and JWKS were intended solely for development
  and testing and have only ever been used that way. Retain these files as fixtures
  because existing authentication tests depend on them.
- [x] **Finish the existing S0 artifact provenance/remediation decision.** Completed
  **2026-10-10**. Owner confirmed synthetic-only database/shift history and exclusively
  development/test key usage. The four runtime database/shift paths are untracked and
  ignored; local copies remain present and all four pre-cleanup checksum checks pass.
  The three development signing files remain tracked as authentication-test fixtures.
  A bounded filename inventory found only the four known database/shift paths;
  cache directories and outside locations were not inspected. Evidence and scope:
  [artifact provenance record](repository-artifact-provenance.md). Untracking does
  not erase previously committed versions from history.
- [x] **Update stale completion records after fixes.** Updated **2026-10-09**:
  the implementation guide, infrastructure README, staging/data runbooks, data module
  README and this checklist distinguish authored S5 code, dated local passes and
  pending live evidence. Stale encryption-condition explanations were corrected.
  Full S5 acceptance, actual production approvals and the budget remain open.

## AWS setup checklist, in execution order

### 1. Refresh and verify the existing operator setup — S0

Identity Center, an administrator assignment, a hosted zone, a budget and active AWS
BAAs were previously reported. Verify and record them; they are not established as
missing just because this workspace's token expired. See the
[deployment contract](deployment-contract.md) for the evidence register.

- [x] Refresh `ernote-staging-admin` SSO and verify STS account/assumed-role identity.
  Completed **2026-10-11** using operator-provided STS output. Account
  `657615279057` matches the owner's intended staging account; ARN:
  `arn:aws:sts::657615279057:assumed-role/AWSReservedSSO_AdministratorAccess_e78c48e0012d97ab/zach-admin`.
  This confirms the active identity; resource inventory and runtime-role permissions
  remain separate checks.
- [x] Confirm application Region `us-east-2`, account-owner/root MFA, and intended
  staging/production account boundary. Completed **2026-10-11** via owner confirmation:
  Ohio is the intended application Region, root MFA is enabled, and the owner controls
  the account-owner/root email. Account `657615279057` is for synthetic staging;
  production remains disabled and no production account ID has been recorded.
  AWS IAM independently verified root MFA enabled and zero root access keys on
  **2026-10-14**; this account is also the organization management account.
- [x] Recheck Ohio Sonnet profile/destination Regions, first-use form, agreement and
  applied quotas. Verified **2026-10-14**, details in the dated audit.
- [x] Perform the one owner-approved synthetic administrator invocation after terms
  review. Agreement subsequently AVAILABLE; no further paid request made.
- [ ] Repeat using the eventual worker task role and verify all destination permissions.
- [x] Verify hosted zone `Z0831458W40X3W46HV0S` is public and inspect its name servers.
- [ ] Register `zamos.co`, delegate to the existing zone and choose the staging API
  hostname. Owner confirms it is unregistered; authoritative parent returns NXDOMAIN.
- [x] Record the $50 MONTHLY COST budget, primary billing-view scope, three thresholds
  and configured recipient reference; reprice the stack. Private recipient details
  remain outside Git. No notification sent.
- [x] Confirm the budget recipient is controlled/monitored and delivery works.
  Owner confirmed prior delivery on **2026-10-14**; assistant sent no test message.
- [x] Choose the $166+ scenario for planning, leaving application deployment pending.
- [ ] Choose the application alarm destination and approve the final budget limit/
  provider allowances and actual budget update before foundation apply.
- [ ] Finish synthetic retention: 7-day audio/backups source settings are explicit;
  30-day app logs and a DB-record expiry/rebuild procedure still need implementation.
- [x] Verify AWS Artifact account and organizations BAAs are ACTIVE; record effective
  dates/scope/agreement IDs in the [audit](step8-audit-2026-10-14.md).
- [ ] Retain acceptance/signature evidence in a durable controlled location and resolve
  Deepgram/other vendor agreements. API effective-start dates do not prove signature dates.

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

Bootstrap and staging foundation inputs and all five backend configurations are now
prepared as ignored local files. Approved bootstrap is applied and migrated; see
[the dated audit](step8-audit-2026-10-14.md). No application foundation/release was applied.

- [x] Fill bootstrap account/Region/project inputs and narrow state-role trust to the
  intended stable Identity Center role ARNs where practical.
- [x] Review and apply **bootstrap only**: state/artifact buckets, rotating KMS key,
  five state-access policies and five operator roles.
- [x] Record bucket names and key ARN, enable bootstrap's commented S3 backend, and
  migrate bootstrap state to its remote key. Supply the **KMS key ARN**, not its alias.
- [x] Verify the remote copy before handling local state/backups; keep state and plans
  outside Git and images.
- [x] Initialize the four environment backends at their distinct committed keys;
  fill staging foundation inputs from bootstrap outputs. Keep both prod roots disabled.
- [x] Prove real S3 lock contention, disposable version recovery, live encryption/public/
  HTTP controls, both directions of cross-root denial on existing probes, and wrong-account
  refusal. Each role passed its own lock Get/Put/Delete checks. TLS-floor condition
  simulation passed; live TLS 1.1 policy evaluation was not performed.
- [x] Simulate deployed-policy explicit denies on actual other-root state paths and the
  opposite environment's manifest. No published manifests exist yet.
- [ ] Verify live denial of an existing published opposite-environment manifest after
  foundation publication. Missing-key 404 responses are not permission proof.
- [ ] Verify a versioned foundation manifest contains identifiers/secret ARNs only;
  record its version/checksum and pin the manifest version for a later release.

Native S3 locking needs separate Get/Put/Delete permissions on the `.tflock` object;
normal state access does not require deleting the state object.
[HashiCorp S3 backend documentation](https://developer.hashicorp.com/terraform/language/backend/s3).

### 3. Review foundation inputs and finish its dependencies — S4/S5

- [ ] Pin the reviewed AZ pair, check CIDR overlap, retain closed client ingress and
  choose the one-NAT/endpoint availability and cost tradeoff explicitly.
- [x] Query PostgreSQL 16 minors and orderable `db.t4g.small`/gp3 options in `us-east-2`;
  **16.15** selected in staging inputs on **2026-10-14**; 50 GiB/100 GiB cap, backup
  07:00–07:30 UTC, maintenance Sunday 08:00–09:00 UTC recorded.
- [x] Review synthetic audio current/noncurrent expiry and multipart cleanup separately;
  review 300-second queue visibility, 20-second polling, three deliveries and 4/14-day
  queue/DLQ retention. Source settings and application queue/lease tests match; live
  inspection and actual expiry remain pending foundation deployment.
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
| [ ] | S1.3 model routing/pricing | `crates/er-ai/src/model.rs`, `crates/er-ai/src/transport.rs`, `crates/er-server/src/config.rs`, `crates/er-server/tests/config_gate.rs` | Approved IDs/family and cost mapping; US-profile code/tests pass, administrator access and Ohio rates verified October 14; worker-role access pending. |
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
| [ ] | S2.8 artifact tracking | `.gitignore`, `docs/ops/deployment-contract.md` | Runtime DB/shift paths are untracked/ignored with owner-confirmed synthetic history; tracked dev keys are test fixtures. |

### S3 — state and ownership

| Done | Task | Files | Review focus |
| --- | --- | --- | --- |
| [ ] | S3.1 versions/locking | `.terraform-version`, `infra/bootstrap/versions.tf`, each environment root's `versions.tf`, `.terraform.lock.hcl`, `backend.tf`, `scripts/tf-lock-check.sh` | Tested versions, committed locks, native `use_lockfile`, emulator versus real-S3 proof. |
| [ ] | S3.2 protected storage/principals | `infra/bootstrap/state.tf`, `infra/bootstrap/bucket_policies.tf`, `infra/bootstrap/iam.tf`, `infra/bootstrap/variables.tf`, `infra/bootstrap/locals.tf` | Rotation/versioning/public-access/retention/destroy protection, trust and per-root isolation; shared four encryption guards; actual bootstrap state/lock writes passed October 14. |
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
| [ ] | S5.1 RDS configuration | `infra/modules/data/database.tf`, `infra/modules/data/variables.tf`, both foundation roots' `data.tf`/`variables.tf`/`terraform.tfvars.example` | Supported PG16 minor/class/gp3, capped autoscaling, private encrypted instance, deletion protection/final snapshot. |
| [ ] | S5.2 backups/key lifetime | `infra/modules/data/database.tf`, `infra/modules/data/main.tf`, `infra/modules/data/variables.tf`, production foundation `variables.tf`, `docs/ops/data-verification.md` | Explicit staging/prod backup decisions, advancing restore time, retained backups' key availability; 35-day retention exceeds a 30-day deletion window. |
| [ ] | S5.3 DB TLS/rotation | `infra/modules/data/database.tf`, `crates/er-server/src/db.rs`, `certs/rds-global-bundle.pem`, `crates/er-server/README.md` | `rds.force_ssl=1` plus verified client TLS and CA rotation; inspect the applied parameter group. |
| [ ] | S5.4 audio security | `infra/modules/data/audio.tf`, `infra/modules/data/tests/data.tftest.hcl`, `crates/er-server/src/audio.rs`, `crates/er-server/src/routes.rs`, `docs/ops/data-verification.md` | Header-presence conditions and assertions corrected. Live evidence must prove headerless default-KMS uploads and signed wrong-override denial; external presigned uploads remain part of the contract. |
| [ ] | S5.5 lifecycle/production decisions | `infra/modules/data/audio.tf`, `infra/modules/data/variables.tf`, `infra/envs/prod/foundation/variables.tf`, `infra/envs/prod/foundation/terraform.tfvars.example`, `infra/envs/prod/foundation/tests/data.tftest.hcl`, `docs/ops/deployment-contract.md` | Separate current/noncurrent/multipart clocks, no archive transitions; enablement guards/tests implemented. Actual production retention/legal-hold approvals and live lifecycle inspection remain pending. |
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
