# Phase 4 Step 8 — final AWS live-test setup checklist

Prepared **2026-10-31** against the S11/S12 implementation. This is the current setup
list; the [October checklist](step8-live-aws-checklist.md) remains historical context.
**No AWS resources were applied, credentials retrieved, providers invoked, or hosted
workflows run during this implementation.** Local tests do not close live gates.

Use synthetic data only. Follow this order. A checkbox means the setting has been
verified in the actual account, with restricted evidence recorded in
[step8-verification.md](step8-verification.md).

## 1. Confirm account, cost and external ownership

- [ ] Refresh `aws sso login --profile ernote-staging-admin`; run
  `aws sts get-caller-identity --profile ernote-staging-admin`. Confirm the recorded
  staging account `657615279057` and Region `us-east-2`; do not infer current access
  from the [October 14 audit](step8-audit-2026-10-14.md).
- [ ] Confirm root MFA/no root keys, operator SSO permission sets, service quotas
  and an incident/alarm owner. No permanent AWS keys go into GitHub.
- [ ] Approve a new full-stack budget and alert recipients before foundation apply.
  The old $50 budget did not cover the October estimate of $166.32/month before
  providers/traffic. Reprice NAT, ALB, IPv4 addresses, RDS, tasks, KMS, secrets, logs,
  CloudTrail, S3/SQS and real Deepgram/Bedrock usage. That estimate is historical.
- [ ] Confirm domain registration and public delegation. The existing recorded
  zone is `zamos.co` / `Z0831458W40X3W46HV0S`; ownership/delegation was unresolved.
  Choose an owned staging hostname and keep its ACM validation records.
- [ ] Approve synthetic fixture provenance/distribution, cleanup/retention, one-NAT
  and single-AZ database limitations. Retain the [pre-PHI register](deployment-contract.md);
  this gate does not authorize patient use.

## 2. Reuse bootstrap; establish GitHub trust

- [ ] Inspect existing remote state before any creation. Recorded buckets are
  `ernote-tfstate-657615279057-us-east-2` and
  `ernote-deploy-657615279057-us-east-2`; bootstrap already owns both, their key and
  five state policies/roles. Obtain the actual KMS **key ARN**, not its alias.
- [ ] Inspect `token.actions.githubusercontent.com` in IAM. Bootstrap now owns the
  optional provider via `github_oidc_enabled = true`. If it already exists, import
  it at `aws_iam_openid_connect_provider.github[0]` **in bootstrap** before applying.
  Its URL is `https://token.actions.githubusercontent.com`; audience is
  `sts.amazonaws.com`. Do not duplicate it per environment.
- [ ] Before enabling `ci`, create GitHub environments `staging`, `staging-smoke`,
  and `staging-infrastructure`, restricted to `master`. Require reviewers for
  infrastructure; keep ordinary staging releases automatic after Required CI.
  Verify the actual repository plan supports these protections. Protect workflow,
  IaC, migration and script changes using branch rules and `.github/CODEOWNERS`.
- [ ] Inspect each environment's actual OIDC **subject only**, without logging its
  JWT. Default subjects are `repo:zaugust4891/er-clinician-app:environment:<name>`.
  If immutable IDs are enabled, supply exact `ci.subjects` values; wildcard trust is
  rejected. [GitHub documents both subject formats](https://docs.github.com/en/actions/how-tos/secure-your-work/security-harden-deployments/oidc-in-aws).
- [ ] Plan/review/apply `ci` in staging foundation under SSO after the smoke
  secret/key exist. Set repository, `environment = "staging"`, smoke secret/key
  ARNs and any subject overrides. The root supplies artifact bucket/key and private
  subnets automatically. Record `identity.ci_role_arns`. Roles allow two-hour sessions.
- [ ] **Foundation provisioning policy: separate operator/security review required.**
  The infrastructure role deliberately ships with state/manifest/plan-artifact
  permissions only. Author/review and attach the account's approved provisioning
  policy before enabling the infrastructure workflow, or keep foundation applies
  under SSO. Scope it to the selected staging network/resources, DNS zone and
  infrastructure services; account for Terraform reads, creation/tagging, RDS KMS
  grants and service-linked-role creation. Review IAM/key-policy administration and
  destructive operations explicitly. Do not attach AdministratorAccess as a shortcut.
  Automatic approval review rejected the proposed broad regional provisioning
  policy during implementation; that policy was **not added or applied**.
- [ ] Prove fork/PR/other-repository subjects cannot assume any role, staging
  identities cannot read production state or assume production roles, release cannot
  read DB/Deepgram credentials, and smoke can read only its synthetic credential
  and approved fixture. IAM policies are testable locally; these are live probes.

## 3. Apply the durable staging foundation under SSO

Create local ignored `backend.hcl`/`terraform.tfvars` from the example files. Review
the complete plan and cost, then apply the foundation. **Do not hand-create every
Terraform-owned service in the console.** Keep `services_enabled=false` in release
and application ingress closed during initial setup.

- [ ] Select two Ohio AZs and nonconflicting VPC CIDR. Apply public, private task,
  isolated DB subnets, NAT/S3 endpoint, routes and separate security groups.
- [ ] Apply encrypted private RDS and verify the selected PostgreSQL minor/class
  are currently orderable. Configure verified TLS, deletion protection, final
  snapshots, backup retention and the privacy-safe parameter group.
- [ ] Apply audio S3/KMS/lifecycle/public-access protections and encrypted main
  queue/DLQ. Preserve external presigned HTTPS PUT support.
- [ ] Apply separate API/worker/migration/onboarding task and execution roles,
  bootstrap execution role, and empty DB/Deepgram secret containers.
- [ ] Inventory the old sandbox Cognito pool before choosing new creation or
  import. Configure the staging-only password-auth exception, no self-signup,
  secretless app client, and smoke credential secret/key. Do not reuse the pool for PHI.
- [ ] Apply ECR immutable tags and ECS cluster. Keep tagged running/rollback
  images; do not add an indiscriminate lifecycle expiry rule.
- [ ] Apply ACM in Ohio, public DNS alias, ALB, HTTPS target group, WAF and private
  API 8443 rules. Complete certificate validation. Record the API HTTPS origin.
- [ ] Apply the five encrypted process log groups, audit destinations, metric
  filters, alarms and SNS subscriptions. Confirm subscriptions and real delivery.
  `telemetry.alarms_enabled` must be true before services can be enabled.
- [ ] Inspect the published versioned foundation manifest for complete auth,
  edge, cluster, roles and logs. It contains secret ARNs only. Release pins its S3
  version at the start of each attempt.

Detailed probes: [network](network-verification.md), [data](data-verification.md),
[identity](identity-verification.md), [Cognito](auth-onboarding.md),
[edge](compute-edge-verification.md), [telemetry](telemetry-verification.md).

## 4. Populate secrets and perform private bootstrap

- [ ] Obtain a scoped Deepgram staging key and confirm `nova-3-medical` access.
  Put `{"api_key":"..."}` into the existing Deepgram secret through a secure
  operator path. Never place its value in Terraform or GitHub.
- [ ] Confirm Bedrock access, quotas, approved `us.anthropic.claude-sonnet-4-6`
  profile and destination Regions. API/worker generation uses task-role Bedrock
  credentials; no Anthropic key or mock fallback belongs in staging.
- [ ] Publish a reviewed, tested single-platform `linux/amd64` bootstrap image
  under SSO; record its ECR digest. The initial DB bootstrap needs an image before
  the full deployment workflow can succeed. See [identity verification](identity-verification.md).
- [ ] Stage the four distinct DB login secrets using `scripts/db-secrets.py stage`.
  Set `operator_image_digest` in foundation to render private operator templates.
  Pin AWSPENDING version IDs with `render`, run `er-db-bootstrap`, inspect its exact
  task/container exit code, then `promote`. Preserve the rotation record on failure.
- [ ] Run the private owner `er-migrate` task before clinician onboarding. Keep
  master/DB credentials inside AWS; inspect process-specific grants/TLS there.
- [ ] Create **three** synthetic Cognito users with permanent passwords, no
  invitations: clinician A, clinician B, and an unprovisioned negative-test user.
  Add the third entry to the S7 smoke secret:

  ```json
  {
    "clinician_a": {"username": "synthetic-clinician-a", "password": "<securely generated>"},
    "clinician_b": {"username": "synthetic-clinician-b", "password": "<securely generated>"},
    "unprovisioned": {"username": "synthetic-unprovisioned", "password": "<securely generated>"}
  }
  ```

- [ ] Map only A and B's actual Cognito `sub` values to **different** organizations
  with private `er-onboard`. Verify rerun idempotency. The third user must have no
  clinician row. Keep the default 60-minute token lifetime for this bounded gate.
- [ ] Upload one consented, synthetic spoken WAV (44 bytes–10 MiB) to
  `s3://<deploy bucket>/envs/staging/fixtures/smoke.wav`, using default SSE-KMS.
  Record its SHA256 and provenance. Select a short JSON array of reliable clinical
  terms actually spoken and expected in the generated note, for example
  `["ankle"]` **only if that is what the fixture says**. No audio is bundled by this
  implementation. Smoke checks the exact bytes and transcript/export terms.

## 5. Set GitHub variables and activate

All listed variables are non-secret references/configuration. Passwords remain in
Secrets Manager. Set `STAGING_RELEASE_CONFIG` as a repository variable shared across
the deployment jobs, using real verified values:

```json
{
  "aws_account_id": "657615279057",
  "aws_region": "us-east-2",
  "project": "ernote",
  "deploy_artifacts_bucket": "ernote-deploy-657615279057-us-east-2",
  "state_bucket": "ernote-tfstate-657615279057-us-east-2",
  "state_kms_key_arn": "<actual bootstrap key ARN>"
}
```

| Scope | Variable | Value/action |
|---|---|---|
| Repository | `STAGING_DEPLOY_ENABLED` | Leave unset/false until all prerequisites pass; then `true` |
| Repository | `STAGING_INFRA_ENABLED` | Enable only after separately reviewed provisioning permissions |
| `staging` | `AWS_RELEASE_ROLE_ARN` | Exact `identity.ci_role_arns.release` |
| `staging-smoke` | `AWS_SMOKE_ROLE_ARN` | Exact `identity.ci_role_arns.smoke` |
| `staging-smoke` | `SMOKE_SECRET_ARN` | Auth output's synthetic secret ARN |
| `staging-smoke` | `DEPLOY_ARTIFACTS_BUCKET` | Bootstrap deployment bucket |
| `staging-smoke` | `SMOKE_WAV_SHA256` | SHA256 of reviewed fixture bytes |
| `staging-smoke` | `SMOKE_EXPECTED_TERMS` | JSON array described above |
| `staging-infrastructure` | `AWS_INFRA_ROLE_ARN` | Exact infrastructure role ARN |
| `staging-infrastructure` | `STAGING_FOUNDATION_TFVARS` | Reviewed JSON equivalent of foundation tfvars, references only |

- [ ] Require `Required CI` on `master`; run CI successfully on the reviewed
  commit. Deploy accepts only successful push/manual CI for this repository's
  current `master`, never PR runs. A superseded commit fails before AWS authority.
- [ ] Configure approved smoke-runner access and enable reviewed edge ingress
  with the existing WAF/telemetry guard. Hosted runner IPs vary; do not invent a
  fixed CIDR that silently blocks smoke. This is a foundation change.
- [ ] Turn on staging deploy, then push a reviewed commit or dispatch CI on
  current `master`. The workflow pins the source SHA, publishes/scans the image,
  migrates, applies release, verifies tasks/target health and runs smoke.
- [ ] Confirm `envs/staging/accepted-release.json` advances only after smoke.
  Inspect per-attempt records at `envs/staging/releases/<run>-<attempt>.json`.
- [ ] For foundation changes, dispatch **Staging foundation / plan** with current
  source SHA. Download the exact encrypted S3 plan version under SSO, review it
  privately with `terraform show`, then dispatch **apply** with its plan ID, S3
  version and SHA256. Reviewers approve that concrete version. Stale plans fail;
  regenerate/review them. Never put plan/state contents in GitHub artifacts/comments.

## 6. Run live acceptance, failure drills and handoff

- [ ] Complete every row of [the verification record](step8-verification.md),
  including private connectivity, negative IAM/TLS, provider usage, privacy canary,
  notifications, restart and timeout/reload behavior, cost and convergence.
- [ ] Accept two releases, then rehearse migration failure and candidate rollout
  failure with synthetic staging. Use the procedures in
  [staging-deploy.md](staging-deploy.md); do not purge queues or run down migrations.
- [ ] Verify rollback restores the previous commit's config and digest, and the
  rollback smoke passes. Schema-changing rollback requires explicit migration-set
  compatibility in `ci/release-policy.json`; otherwise services stop for forward repair.
- [ ] Exercise failed first deployment and interrupted-run recovery. Check active
  migrations before retrying. A cancelled/timed-out workflow requires operator
  inspection; GitHub cancellation is not a reliable recovery trigger.
- [ ] Verify both staging Terraform roots converge using accepted inputs. Record
  accepted source/digest, manifest version, migration/task definitions, alarm owner,
  timing, resource counts, estimated versus observed spend, and restricted evidence links.
- [ ] Keep production disabled. Its workflow deliberately refuses promotion and
  has no OIDC permission. Review [production-promotion.md](production-promotion.md)
  before any later activation. Complete the pre-PHI gates separately.
