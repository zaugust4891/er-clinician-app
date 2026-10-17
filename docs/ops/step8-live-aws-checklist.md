# Step 8 — live AWS setup and completion checklist

S8's compute/edge code is now implemented locally. Follow the
[S8 activation and verification guide](compute-edge-verification.md) for its exact
inputs and remaining live checks; no S8 AWS acceptance box is closed by local tests.

Prepared **2026-10-15** for Phase 4 Step 8: Terraform, CI/CD, and the first
**synthetic-only staging deployment**.

Step 8 is complete when a reviewed commit produces one immutable image, the pipeline
migrates the private database and deploys the API/worker, the real-provider lifecycle
passes, and migration failure and application rollback have been rehearsed. Creating
the AWS resources or getting `/healthz` to return 200 is only part of that gate.

This checklist uses the current working tree, including the S7 Cognito/onboarding
work, and the recorded October 14–15 evidence. **No fresh AWS account audit or cloud
changes were performed to prepare it.** Checked boxes below mean previously recorded
completion; unchecked boxes need live evidence or an explicit decision. Reconfirm
account settings before deployment.

Primary references: [Step 8 implementation plan](../planning/phase4-step8-implementation-steps.md),
[deployment contract](deployment-contract.md), [October 14 audit](step8-audit-2026-10-14.md),
and [staging deployment runbook](staging-deploy.md).

## 1. Reuse what is already set up

These are recorded prerequisites, not resources to recreate.

| Item | Recorded value |
| --- | --- |
| Staging account / Region | `657615279057` / `us-east-2` |
| Operator SSO profile | `ernote-staging-admin` |
| Repository / default branch | `zaugust4891/er-clinician-app` / `master` |
| Terraform state bucket | `ernote-tfstate-657615279057-us-east-2` |
| Deployment-artifact bucket | `ernote-deploy-657615279057-us-east-2` |
| State encryption key alias | `alias/ernote-tfstate` |
| Existing public hosted zone | `zamos.co`, `Z0831458W40X3W46HV0S` |
| Existing sandbox Cognito pool | `er-note-dev`, `us-east-2_dxrqqBYoL`; adoption undecided |
| Approved Bedrock profile | `us.anthropic.claude-sonnet-4-6` |

- [x] Operator SSO/account identity, root MFA, and absence of root access keys
  recorded on October 14.
- [x] Terraform bootstrap applied: two protected/versioned S3 buckets, state KMS
  key, and five scoped state-access roles. Bootstrap state migrated to S3;
  all five backends initialized.
- [x] Live bootstrap encryption, public-access denial, version recovery, lock
  contention, cross-root state denial, and wrong-account checks passed.
- [x] Bedrock profile and account access checked; one synthetic **administrator**
  invocation passed. This does not complete the worker-role invocation gate.
- [ ] Before the next apply, refresh SSO, verify the expected account/Region, and
  inspect bootstrap drift. Retain the existing state and scoped roles.

Evidence: [bootstrap and account audit](step8-audit-2026-10-14.md).

## 2. Close the owner setup decisions first — S0

Owner: project/AWS account owner. These can be resolved while the remaining code is
implemented.

- [ ] **Approve the full staging budget and update AWS Budgets.** The last recorded
  limit is **$50/month**; the October 14 always-on estimate is **$166.32/month before
  providers, traffic, and additional operational costs**. Reprice the final resource
  inventory, including the S7 smoke secret/key, Deepgram medical-model usage,
  Bedrock tokens, logs/audit events, and contingency. Record the actual limit,
  scope, alert thresholds, and monitored recipient. The existing budget covers the
  management account's primary billing view, not staging alone. See the
  [dated cost worksheet](staging-cost-review-2026-10-14.md); this is not a new quote.
- [ ] **Register and delegate the domain.** `zamos.co` was unregistered at the last
  check. Register it under the owner's control, or record a different owned domain;
  configure registrar delegation to the intended Route 53 zone and verify public
  resolution. Choose the exact staging API hostname before requesting its certificate.
- [ ] **Choose the application alarm recipient.** Record who monitors incidents,
  the SNS destination/subscription, and who will acknowledge the test notification.
  Existing budget-email evidence does not prove application-alarm delivery.
- [ ] **Accept the availability/cost choices.** Pin two Ohio AZs and review the
  `10.40.0.0/16` CIDR against relevant networks. Confirm one staging NAT and
  single-AZ RDS, including their outage tradeoffs; initial capacity is two API
  tasks and one worker. Change the estimate if those choices change.
- [ ] **Decide Cognito import versus a new pool.** Inventory the existing sandbox
  pool/client/users. Import only a compatible pool with a reviewed no-replacement
  plan; otherwise create a separate synthetic staging pool and leave the sandbox
  intact. Use the [S7 procedure](auth-onboarding.md).
- [ ] **Prepare Deepgram access.** Create/select the staging project and scoped API
  key; confirm access to the application's `nova-3-medical` model and its actual
  plan/rate. Record permission/provenance for the synthetic audio fixture.
- [ ] **Confirm the Bedrock deployment inputs.** Use the approved US profile for
  summary and MDM from Ohio. Recheck its destination Regions and account quotas;
  the last record lists `us-east-1`, `us-east-2`, and `us-west-2`. Do not substitute
  another profile to work around access failure. Actual worker invocation is below.
- [ ] **Record synthetic-data cleanup and evidence ownership.** Adopt the existing
  7-day audio, 30-day application-log, and 7-day RDS-backup settings in the deployed
  configuration. Define and exercise a controlled synthetic DB expiry/rebuild
  procedure covering transcripts/events/snapshots as well as audio. Choose durable,
  restricted storage for operational evidence; the old `/tmp` audit is temporary.

## 3. Finish the code needed to make AWS setup deployable — S7–S11

Owner: engineering. **The current foundation/release roots do not yet constitute a
complete deployable application.** S4–S6 are authored but lack live acceptance. S7
pool/client/onboarding code and local validation are complete as of October 15,
disabled by default, and have no live acceptance. See the
[S7 completion record](../planning/phase4-step8-implementation-steps.md#implementation-record--s7).
The following are implementation prerequisites:

- [ ] **S8 compute and edge:** implement the ECR/ECS/ALB/ACM/DNS/WAF Terraform,
  release task definitions/services, private one-shot task templates, API HTTPS
  listener on 8443, and bounded `/readyz` checks. The current tree has no compute
  or edge modules and no release ECS resources; the API still needs backend TLS.
- [ ] **S9 telemetry:** implement safe structured logging, worker progress signals,
  log groups, audit configuration, alarms, and privacy-canary checks before live
  provider traffic. Create log groups for bootstrap/onboarding as well as services.
- [ ] **S10 CI:** add required PR checks for formatting, Clippy, workspace/local
  integration tests, feature boundaries, SQLx cache, Terraform, and image/runtime
  packaging. The `.github/workflows/` directory is absent in this working tree.
- [ ] **S11 deployment:** implement OIDC workflows, reviewed infrastructure policy,
  digest/manifest pinning, migration completion checks, release, smoke, rollback,
  and the inactive production promotion path. Existing CI role templates are
  disabled and the infrastructure role lacks the final provisioning policy.
- [ ] **S12 evidence tooling:** implement the synthetic remote smoke client and
  failure exercises. `scripts/demo-remote.sh` and `docs/ops/step8-verification.md`
  are planned deliverables, not existing proof of completion.

See [S8–S12 in the implementation plan](../planning/phase4-step8-implementation-steps.md#s8--define-ecs-services-and-the-https-edge).

## 4. Apply the reviewed staging foundation — S4–S9

Owner: infrastructure operator/engineering. Apply the complete reviewed foundation
after the budget decision and implementation dependencies are closed. Keep
application workloads off and ALB client ingress closed during provisioning.
Use the foundation root for durable infrastructure; use the release root for ECS
application task definitions/services. Do not routinely use targeted applies to
work around missing dependencies.

### Network

- [ ] Create the two-AZ VPC layout: two public subnets, two private task subnets,
  two isolated DB subnets, internet gateway, selected NAT topology, route tables,
  and S3 gateway endpoint on both task route tables. Paid interface endpoints
  remain off unless separately selected and costed.
- [ ] Apply separate ALB, API, worker, migration, onboarding, and DB security
  groups. Only ALB may reach API 8443; only the intended task groups may reach
  RDS 5432. Disable task public IP assignment. Check actual ENIs and routes.

### RDS, audio, queues, and encryption

- [ ] Create private, encrypted PostgreSQL with the RDS-managed master secret.
  Reconfirm orderability of the pinned `16.15`, `db.t4g.small`, and gp3 settings;
  start at 50 GiB with a 100 GiB cap, deletion protection, final-snapshot behavior,
  7-day backups, and the reviewed maintenance window. Set `rds.force_ssl=1`.
- [ ] Create `ernote-staging-audio-657615279057-us-east-2` with versioning,
  public-access blocks, ACLs disabled, approved SSE-KMS encryption, and separate
  7-day current/noncurrent/multipart lifecycle rules. Preserve the external
  presigned-upload path; do not require every bucket request to originate in the VPC.
- [ ] Create `ernote-staging-jobs` and `ernote-staging-jobs-dlq`, encrypted with
  the data key. Verify 300-second visibility, 20-second long polling, three
  receives before DLQ, four-day source retention, 14-day DLQ retention, and the
  DLQ's single allowed source queue.
- [ ] Create the rotating application-data KMS key, separately from the state
  key. Verify exact service/role encryption-context grants for RDS, S3 Bucket
  Keys, SQS, and Secrets Manager. Runtime roles must not administer the key.

### IAM, secret containers, and Cognito

- [ ] Apply separate task and execution roles for API, worker, migration, and
  onboarding, plus the DB-bootstrap execution role. Bootstrap has no task role;
  only its execution role receives the RDS master secret. Check that repository,
  log-group, cluster, and task-family names match the authored IAM policy scopes.
- [ ] Create the five S6 encrypted secret containers and S7's separate smoke
  credential container/key. Terraform manages containers and policies; populate
  values out of band in section 5. Publish secret ARNs only in manifests.
- [ ] Apply/import the chosen Cognito pool and secretless client, with self-signup
  disabled, reviewed token/session settings, and deletion protection. Record the
  staging-only optional-MFA/password-auth exception for synthetic smoke users.
  Publish the pool ID, client ID, and Region; grant onboarding `AdminGetUser`
  only on this pool. Hosted login is unnecessary unless explicitly configured.

### Registry, cluster, and public edge

- [ ] Create the ECR repository and ECS cluster. Enable immutable image tags,
  scanning, and a lifecycle policy that retains running and rollback images.
  Push a tested candidate and record its digest/architecture for private setup tasks.
- [ ] Request/validate ACM for the chosen hostname **in `us-east-2`**, create the
  ALB and Route 53 alias, and retain certificate-validation DNS records for renewal.
  Configure HTTPS with TLS 1.2/1.3; port 80 may redirect but must not serve API data.
- [ ] Configure an HTTPS `ip` target group on API port 8443 and HTTPS `/readyz`
  health checks. ALB encrypts the target connection but does not validate its
  certificate; document the network/IAM trust boundary. See
  [AWS target-group TLS behavior](https://docs.aws.amazon.com/elasticloadbalancing/latest/application/load-balancer-target-groups.html).
- [ ] Attach a regional WAF ACL with managed protections and a reviewed rate rule.
  Validate legitimate event batches and the 2 MiB application body limit before
  opening staging ingress; document any narrow rule exceptions and smoke-runner access.

### Logs, audit, and alerts

- [ ] Create encrypted, access-restricted, 30-day log groups matching
  `/ecs/ernote-staging/{api,worker,migration,db-bootstrap,onboarding}`. Verify
  each execution role can write only its intended streams and inject its own secrets.
- [ ] Configure CloudTrail management-event coverage, log validation, protected
  storage, and operator access. Inspect existing account/organization trails first.
  Decide and cost scoped audio-object data events if needed for access evidence.
- [ ] Configure WAF data protection/redaction and sampling deliberately. If
  sensitive fields cannot be suppressed and verified, keep raw request logging
  disabled and use metrics. Decide whether to enable ALB access logs; if enabled,
  use a dedicated restricted bucket with short retention and the supported SSE-S3
  encryption, not the audio/state bucket's KMS-only policy. See
  [AWS ALB logging requirements](https://docs.aws.amazon.com/elasticloadbalancing/latest/application/enable-access-logging.html).
- [ ] Create SNS notifications and alarms for no healthy API targets, sustained
  5xx, missing worker progress, oldest queued-message age, visible DLQ messages,
  RDS low storage/high connections, and spend/forecast thresholds. Set evaluation
  windows and missing-data behavior; confirm the subscription and one real delivery.
- [ ] Publish the complete non-secret foundation manifest to
  `s3://ernote-deploy-657615279057-us-east-2/envs/staging/foundation-manifest.json`.
  Record its version ID; make release guards require S7–S9 interfaces too.
  Check allowed reads and cross-environment denial against existing objects;
  a missing-object 404 is not permission evidence.

Detailed inspections: [network](network-verification.md), [data](data-verification.md),
[identity](identity-verification.md), and [Cognito](auth-onboarding.md).

## 5. Populate secrets, bootstrap the database, and onboard users — S6–S7

Owner: privileged operator. Requires the foundation, reviewed image, private task
templates, and working logs. Use the [credential bootstrap/rotation procedure](identity-verification.md).

| Credential | Population / consumer |
| --- | --- |
| RDS master | RDS-managed; DB-bootstrap execution role only |
| Schema-owner DB login | Generated via `scripts/db-secrets.py`; `er-migrate` |
| API DB login | Generated via the same helper; API only |
| Worker DB login | Generated via the same helper; worker only |
| Onboarding DB login | Generated via the same helper; `er-onboard` only |
| Deepgram | Staging project key, JSON `api_key`; worker only |
| Synthetic smoke users | Two Cognito credentials; separate smoke role/secret/key |

- [ ] Populate the Deepgram secret through the secure operator path. Do not add
  an Anthropic API-key secret; cloud generation uses Bedrock through the worker role.
- [ ] Stage four distinct DB logins with `scripts/db-secrets.py stage`. Keep the
  non-secret rotation/version record in protected durable storage; do not put
  passwords in Terraform, manifests, shell output, or GitHub artifacts.
- [ ] Run the digest-pinned private `er-db-bootstrap` task with exact secret
  version IDs. Require the expected task definition and container **exit code 0**
  before promoting those versions to `AWSCURRENT` with the helper. Preserve the
  same rotation record for a failed/interrupted retry.
- [ ] Run `er-migrate` as the schema owner and verify migrations plus runtime
  grants. API, worker, and onboarding logins must have their distinct restricted
  privileges; services must never connect as the owner or master.
- [ ] Create two synthetic Cognito users without sending invitations; populate
  only the smoke secret with their passwords. Obtain their actual Cognito `sub`
  values and seed distinct organizations/clinician mappings with private `er-onboard`.
  Require `created, created`, then `unchanged, unchanged` on rerun.
- [ ] Exercise maintenance rotation after service acceptance: stop/drain workloads,
  bootstrap/promote the next pinned credentials, replace tasks, and verify old
  passwords cannot establish new DB sessions. Injected ECS secrets are loaded at
  task start; rotation requires replacement tasks. See
  [AWS ECS secret-injection behavior](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/secrets-envvar-secrets-manager.html).

## 6. Prove private connectivity and permissions — S4–S7

Owner: engineering/operator. Use reviewed disposable diagnostic tasks, with scoped
roles, in **each private task subnet**. The application image is not a general
diagnostic toolbox; prepare the separate diagnostic image/template before this gate.

- [ ] Prove DNS and verified TLS connectivity to ECR/S3 layers, Secrets Manager,
  CloudWatch, SQS, RDS, Cognito JWKS, Deepgram, and Bedrock from each AZ. Verify
  secret injection and log delivery; wrong CA/hostname fixtures must fail.
- [ ] Prove RDS connections use `verify-full` and encrypted sessions with the
  packaged CA bundle. Corroborate external task/RDS access denial with actual
  security groups, ENIs, routes, and `PubliclyAccessible=false`.
- [ ] With the **actual API role**, prove encrypted audio signing/PUT/HEAD and queue
  send; with the **actual worker role**, prove audio GET, queue receive/delete/
  visibility change, and a bounded approved-profile Bedrock invocation. A prior
  administrator success is insufficient. Run real Deepgram transcription in S12.
- [ ] Exercise negative IAM/DB checks: API cannot invoke Bedrock/read Deepgram;
  worker cannot PUT audio; runtime roles cannot perform DDL, delete event history,
  purge queues, change retention, or administer keys/backups. Execution roles
  cannot read unrelated secrets; master access remains bootstrap-only.
- [ ] Prove an outside-VPC presigned PUT works without SSE headers and stores the
  approved KMS key. Test anonymous/insecure reads, changed key/content type,
  natural expiry, and separately signed forbidden-encryption requests. Distinguish
  signature failure from bucket-policy denial; inspect safe object metadata.
- [ ] Inspect deployed RDS backups and observe `LatestRestorableTime` advancing.
  Inspect S3 lifecycle/encryption/versioning and SQS/DLQ settings against section 4.
  Backup creation is required here; a full restore rehearsal is Step 9.

## 7. Connect GitHub to AWS and protect deployments — S10–S11

Owner: repository administrator/infrastructure operator. GitHub configuration is an
external dependency of Step 8 even though it is outside the AWS console.

- [ ] Protect the default branch and required CI checks; review workflow/IaC
  changes. Configure `staging-infrastructure`, `staging`, and `staging-smoke`
  environments with intended deployment-branch restrictions before enabling trust.
- [ ] Import/create the GitHub OIDC provider and enable separate infrastructure,
  release, and smoke roles. Match `aud=sts.amazonaws.com` and the repository's
  **actual exact environment subject**, including immutable IDs if used; do not
  assume a branch-only subject for an environment job. See
  [GitHub's AWS OIDC guidance](https://docs.github.com/en/actions/how-tos/secure-your-work/security-harden-deployments/oidc-in-aws).
- [ ] Attach the reviewed foundation provisioning policy. Scope release ECR/ECS
  permissions, migration `RunTask`, and `iam:PassRole` to the intended resources;
  exclude DB-bootstrap/onboarding authority. Smoke may retrieve only its synthetic
  credential. No DB/Deepgram secrets or long-lived AWS keys belong in GitHub.
- [ ] Supply non-secret workflow configuration: account/Region, role ARNs, backend
  identifiers, manifest location/version, ECR repository, cluster, service/task
  families, smoke secret ARN, and staging URL. Derive resource identifiers from
  reviewed outputs instead of maintaining conflicting console copies.
- [ ] Verify authorized OIDC assumption succeeds and untrusted branch/PR,
  wrong-environment, and cross-environment access fail. Test release denial of
  DB/provider secret reads and bootstrap `PassRole`.
- [ ] Serialize foundation/release mutations with the same environment concurrency
  group and Terraform locks; never cancel an in-progress migration for a newer
  commit. Record the exact commit, image digest, config, schema versions, and
  foundation-manifest version throughout the workflow.
- [ ] Make production promotion reviewable with explicit approval requirements,
  while production roots/workloads/trust remain inactive until their own contract
  is ready. Verify the approval boundary without provisioning production resources.

## 8. Deploy the API and worker through the pipeline — S8/S11

Owner: engineering/operator. Bootstrap may use an operator, but the first accepted
application release and later default-branch releases use the same pipeline.

- [ ] Build/push once from the reviewed source, scan, and pin the ECR digest.
  Verify the image includes `er-server`, `er-worker`, `er-migrate`,
  `er-db-bootstrap`, `er-onboard`, native dependencies, and TLS trust material.
- [ ] Run the private migration task using the candidate digest and owner secret.
  Check submission failures, bounded completion, and container exit 0 **before**
  changing services. Image/secret/startup failures and missing exit codes fail deployment.
- [ ] Apply the release root with the pinned digest/config/manifest version.
  Start two API tasks and one worker at the reviewed initial 0.5 vCPU/1 GiB each,
  with explicit architecture/Fargate version, private networking, non-root user,
  dropped capabilities, read-only root, and no default ECS Exec. Choose a platform
  supporting JSON-key/version secret injection; Linux Fargate requires 1.4.0+ for
  those features. [AWS requirements](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/secrets-envvar-secrets-manager.html).
- [ ] Use API rolling replacement and worker stop-before-start at desired count 1
  (`minimumHealthyPercent=0`, `maximumPercent=100`). Configure worker stop timeout
  120 seconds with a shorter drain deadline; start API deregistration/stop at
  45/60 seconds. Budget DB pools for deployment overlap and one-shot/admin capacity.
- [ ] Enable deployment failure detection/rollback; verify exact running task
  revisions, image digests, desired/running counts, target health, and worker
  progress. Stability after an automatic rollback is a failed candidate release.
- [ ] Open the intended staging ingress only after TLS, WAF, auth, and telemetry
  are ready. Verify public DNS/certificate chain without verification bypass,
  encrypted backend traffic, readiness failure on DB loss, and graceful replacement.
- [ ] Run authenticated smoke, then persist the accepted release manifest. On a
  first-deploy failure with no prior accepted image, close ingress/stop workloads
  and preserve evidence. Subsequent failures use the prior compatible digest/config.

## 9. Collect the live acceptance evidence — S7/S9/S11/S12

Owner: engineering, reviewed by the project owner. Resource existence alone does
not check these boxes.

- [ ] **Auth/tenancy:** both real access tokens reach `/v1/me` with their distinct
  orgs. Expired, wrong-pool, wrong-client, ID, dev, missing, and unprovisioned-subject
  tokens fail with 401. Cross-org patients/events/snapshots/notes/exports/shifts/
  audio/jobs disclose neither existence nor content. Exercise synthetic offboarding
  and document the membership-cache/token revocation limits.
- [ ] **Real lifecycle:** register shift/patient → start audio → presign/upload the
  approved WAV → complete → stop audio → await real Deepgram transcript and Bedrock
  summary → add objective data → request/await MDM → reload → review/sign/export →
  end shift. Assert expected synthetic facts without printing note contents.
- [ ] **Provider/metering:** confirm non-mock execution and correctly attributed,
  nonzero summary/MDM usage. Exercise a cache-eligible repeat and inspect safe
  counters; resolve remaining Step 7 model/cache discrepancies. Keep bounded job
  polling independent of the CLI's shorter observation window.
- [ ] **Retries/restarts:** resend the same event batch, chunk completion, and MDM
  job ID; verify stable IDs and no duplicate derived events. Replace the worker
  during a job and reload from another client/API task; require progress or an
  explicit terminal/DLQ outcome with no silently lost accepted events.
- [ ] **Privacy/operations:** run canaries through request bodies, paths/queries,
  headers/tokens, invalid values, and provider/DB errors. Inspect every enabled
  app/edge/audit/failure-artifact destination. Verify no content leakage beyond the
  documented restricted metadata policy, actual alarm delivery, worker-stall
  detection, and diagnostic queries for stale DB jobs not visible in SQS.
- [ ] **Migration failure:** deliberately fail a synthetic migration and show
  running service revisions stay unchanged. Verify a completed migration can be
  retried after deployment failure without duplicate schema changes.
- [ ] **Rollback:** establish a second accepted release so a genuine prior digest
  exists; exercise a bad candidate/task-start or smoke failure, restore the prior
  compatible digest/config, and rerun smoke. Do not reverse migrations or purge
  queues to make the exercise pass. Record any remaining queued/failed/DLQ jobs.
- [ ] **Convergence:** from a clean checkout with documented access and accepted
  inputs, both staging roots produce empty plans, including after rollback.
- [ ] **Cost/capacity:** record actual resource counts, lifecycle/queue timing,
  peak memory, DB connections, and an updated monthly estimate. Distinguish
  measured counters from lagging bills and estimated provider/traffic charges.
- [ ] **Sign-off:** create `docs/ops/step8-verification.md` with date, reviewer,
  account/Region, source commit, image/task revisions, manifest version, assertion
  results, and restricted evidence links. Update the deployment/rollback/rotation
  runbook and contract. List owners and closure criteria for remaining pre-PHI work.

Evidence should contain identifiers, versions, counts, and pass/fail results—not
passwords, tokens, presigned URLs, audio, transcripts, generated notes, or SQL dumps.

## 10. Keep the completion boundary explicit

Step 8 closes when the required unchecked items above have evidence and the
synthetic staging gate passes. Conditional features such as hosted Cognito login,
paid interface endpoints, and ALB raw access logging can remain disabled with the
choice recorded.

The following remain outside the Step 8 staging setup; carry them forward with
owners instead of treating them as reasons to deploy production now:

| Follow-up | Boundary |
| --- | --- |
| Production account selection/provisioning and real users/data | Production path is defined in Step 8 but inactive; its contract and approvals are separate |
| RDS restore rehearsal / recovery objectives | Step 9; Step 8 still requires working backups and an advancing restore time |
| Audio version/checksum binding | Step 9/pre-PHI; a valid presigned URL can still overwrite its key |
| Tenant RLS, clinical read/export audit, revocation SLA, expensive-operation quotas | Step 9/pre-PHI |
| Job reconciliation/outbox recovery and incomplete-chunk janitor | Step 9; Step 8 supplies visibility and manual diagnostics |
| Alternating-user DB rotation without maintenance | Step 9; Step 8 rehearses the implemented maintenance procedure |
| Load benchmarks and full provider invoice reconciliation | Step 10; Step 8 includes smoke-level cost/capacity checks |
| Agreements, consent, retention/legal holds, risk/incident readiness | Pre-PHI gate in the [deployment contract](deployment-contract.md#pre-phi-gate); AWS agreement metadata alone does not close it |

