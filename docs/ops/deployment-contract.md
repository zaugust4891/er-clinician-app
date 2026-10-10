# Step 8 S0 — staging deployment contract

Updated 2026-09-14. This is a decision and evidence register for **synthetic-only** staging, not a record of completed cloud deployment or approval to process patient data. Do not put credentials, agreement PDFs, patient data, or full account evidence in this file. Store sensitive evidence in an access-controlled location and add only its owner/date/reference here.

## Confirmed and reported inputs

| Item | Current value | Evidence and status |
| --- | --- | --- |
| Git repository | `zaugust4891/er-clinician-app` | Local `origin` URL; confirmed locally. |
| Default branch | `master` | Operator reports checking GitHub; local checked-out and `origin/master` branches agree. |
| Staging account ID | `657615279057` candidate | IAM user ARN in operator-provided screenshot and local SSO profile `sso_account_id` agree; **not STS-verified**. Confirm this is the intended staging account. |
| Identity Center instance / operator | Organization instance `zamos`; user `zach-admin`, reported assigned `AdministratorAccess` | Operator report; local SSO profile has `sso_role_name=AdministratorAccess`. The IAM user `zach` in the screenshot is a separate identity. |
| Local CLI profile | `ernote-staging-admin` | Local profile default Region was checked as `us-east-2`; the profile name is a local label. |
| Identity Center portal URL and Region | `https://ssoins-66849d140aff6f6f.us-east-2.portal.amazonaws.com`; `us-east-2` | Operator report; both values match the local `staging` SSO session configuration. This is separate from the application Region even though both currently have the same value. |
| Staging application and Bedrock **source** Region | `us-east-2` | Operator decision; local CLI profile Region checked. Set cloud `AWS_REGION` and `COGNITO_REGION` to `us-east-2`. |
| Sonnet foundation model | `anthropic.claude-sonnet-4-6` | Operator's `list-foundation-models` output. Discovery does not prove invocation authorization. |
| Sonnet US geographic profile | `us.anthropic.claude-sonnet-4-6`, reported `ACTIVE` | Operator's `get-inference-profile` response. The earlier suggested command used `--region us-east-1`; repeat with `--region us-east-2` to retain account-specific source-Region evidence. |
| Profile destination Regions | `us-east-1`, `us-east-2`, `us-west-2` | Operator's returned model ARNs; also listed for a `us-east-2` source in the [AWS Sonnet 4.6 model card](https://docs.aws.amazon.com/bedrock/latest/userguide/model-card-anthropic-claude-sonnet-4-6.html). |
| Bedrock invocation | **Pending** | An active profile and model catalog listing are read-only metadata, not a successful invocation. |
| Hosted zone | `zamos.co`; `Z0831458W40X3W46HV0S` | Operator report; confirm public-zone type, registrar owner, and name-server delegation. |
| Budget | `$50` for testing/pilot | Operator reports it is set; budget period, scope, alert thresholds, recipient, and adequacy for pilot workloads are not yet recorded. |
| AWS BAA | Account and organization agreements reported **Active** | Operator reports both signed and active; acceptance dates, scope, and access-controlled evidence references remain to be recorded. This is not a claim about other vendor or customer agreements. |
| Staging retention | Adopt 7-day synthetic audio, 30-day application logs, and 7-day RDS automated backups | Operator decision; implementation and synthetic database-record expiry/rebuild are pending. |
| Production deployment approver | `zach-admin` | Operator report. This records engineering release approval only; privacy, security, legal, and clinical approvals remain separate. |

AWS lists Sonnet 4.6 in `us-east-2` for US geographic and global inference, but not in-Region inference. **Use only the US geographic profile as the staging candidate. Do not substitute the global profile.** A request from Ohio may be processed in any of the three US destinations above; AWS says prompts and outputs may be stored in a destination Region for abuse detection. The destination list and that processing/retention behavior require a recorded privacy/security decision before any patient data. [Model card](https://docs.aws.amazon.com/bedrock/latest/userguide/model-card-anthropic-claude-sonnet-4-6.html) · [Geographic inference](https://docs.aws.amazon.com/bedrock/latest/userguide/geographic-cross-region-inference.html).

The current code does **not** yet support this deployment decision: `ModelId::sonnet_4_6()` supplies the bare ID, and `set_bedrock_id` rejects profile IDs. The worker must require an explicit, reviewed US profile choice for Sonnet in cloud configuration; its allowlist and `cost_micros` price mapping must be updated together before a release. Do not silently change the model default or accept arbitrary profile IDs. AWS says geographic cross-Region inference uses source-Region standard pricing without an extra routing charge, but recheck Sonnet 4.6 token and cache prices for `us-east-2` in [AWS pricing](https://aws.amazon.com/bedrock/pricing/) before changing the meter. IAM and organization SCPs must permit invocation against the profile and all three model destination Regions. [Cross-Region IAM/SCP guidance](https://docs.aws.amazon.com/bedrock/latest/userguide/inference-profiles-support.html).

The operator's model listing also showed `anthropic.claude-haiku-4-5-20251001-v1:0`, while the repo's Haiku Bedrock ID is `anthropic.claude-haiku-4-5`. The pilot's cloud configuration permits Sonnet only. Resolve Haiku's ID, profile, pricing, and quality decision before enabling Haiku; do not treat that catalog result as authorization.

## Decisions and unknowns to close

| Decision / evidence | Current state | Owner and closure condition |
| --- | --- | --- |
| Account and SSO identity | Pending verification | Operator: run `aws sts get-caller-identity --profile ernote-staging-admin`; confirm account ID and an Identity Center assumed-role ARN. Record the account ID and check date, not temporary credentials. |
| Bedrock source-Region profile | Pending `us-east-2` recheck | Operator: run `get-inference-profile` in `us-east-2`; retain the ID, status, destination Regions, and date. |
| First-time use, license, and payment | Unknown | Account owner/procurement: review Anthropic first-time-use requirement, model terms, Marketplace payment method, and model subscription status. First invocation may constitute acceptance of third-party model terms. [AWS model access](https://docs.aws.amazon.com/bedrock/latest/userguide/model-access.html). |
| Invocation and quotas | Unknown | After terms review, operator: invoke with a synthetic prompt from `us-east-2` using the intended profile, then verify the eventual ECS task role separately. Record applied cross-Region tokens/minute, daily token limit, and any requests/minute quota from Service Quotas; model catalog values are not account quotas. |
| Domain and DNS | Hosted zone reported; type and delegation unverified | Domain owner: confirm the zone is public, identify the registrar owner, and compare registrar name servers with the `zamos.co` zone. |
| Cost guardrail | `$50` testing budget reported; details and estimate pending | Account owner: record budget period, scope, alert threshold(s), recipient email, and date; calculate a `us-east-2` staging estimate before provisioning, including API/worker hours, RDS/storage/backups, ALB/LCUs, NAT/public IPv4/endpoints, KMS/Secrets, logs/CloudTrail, WAF, DNS, ECR/build storage, Deepgram, and Bedrock tokens. Revisit the `$50` amount before pilot workloads. Older cost-model amounts and `us-east-1` rates are only workload assumptions. |
| Alarm contact | Unknown | Operator: identify a monitored email or paging destination and verify delivery before deployment. |
| Staging retention | Synthetic defaults adopted; controls not yet configured | Engineering owner: implement the 7/30/7-day settings and define database-record expiry or environment rebuild. S3 expiration alone does not remove transcripts, events, or backups. |
| Production account | Separate AWS account recommended; not selected | Project owner: decide account boundary before any production provisioning. One account with separate state/VPC/IAM is acceptable only for the initial synthetic staging gate. |
| Production approval and retention | Engineering deployment approver reported as `zach-admin`; remaining approvals unknown | Project owner and privacy/security reviewers: name separate privacy, security, legal, and clinical approvers; set production audio, clinical records, logs, backup, legal-hold, and access policies. Do not infer a blanket seven-year medical-record rule from HIPAA. |
| AWS BAA / other agreements | AWS account and organization BAAs reported active; evidence and other agreements pending | Authorized legal/privacy owner: record the AWS agreement scope, acceptance date, and access-controlled evidence reference. Confirm Deepgram and other applicable vendor/customer agreements separately. Do not treat an AWS BAA as covering every provider. |
| Tracked artifact provenance | Unknown; filenames inventoried only | Security/project owner: classify tracked `er_notes.db` and WAL/SHM, `shift.json`, and `dev/keys/dev_{private,public}.pem` and inspect relevant history/caches without publishing their contents. The RDS CA bundle is a public trust bundle. Escalate actual PHI/live-secret exposure; do not casually rewrite history or delete user data. |
| Current app authorization | Authenticated and provisioned clinicians are resolved to a database organization | Product/security owner: approve this for synthetic testing only. Role limits, patient assignment, emergency access, revocation delay, and audit-access design remain pre-PHI decisions. |
| Production audio retention and legal holds | Pending; production remains disabled | Record approved current/noncurrent retention days, legal-hold requirements, approver, date, and evidence reference before enabling production. Mocked test values are not approved production settings. |

## Terraform state and ownership (S3, implemented)

Implemented 2026-09-14 in `infra/`. Runbook: [`staging-deploy.md`](staging-deploy.md). These are the choices; the account-specific values are filled in by the operator after the first bootstrap apply.

| Item | Value | Notes |
| --- | --- | --- |
| Terraform version floor | `>= 1.10.0, < 2.0.0` | 1.10 is the first release whose S3 backend locks with a `.tflock` object instead of a DynamoDB table. Tested release pinned in `.terraform-version` (`1.10.4`). Upgrading to 1.11+ is encouraged: native S3 locking is generally available there rather than experimental. |
| AWS provider | `~> 6.0`, resolved to `6.64.0` | `.terraform.lock.hcl` committed for all five roots, locked for `linux_amd64`, `linux_arm64`, `darwin_arm64`. |
| State locking | S3 `use_lockfile = true` | No DynamoDB lock table is created anywhere. Proven locally against MinIO by `scripts/tf-lock-check.sh`. |
| State bucket | `ernote-tfstate-<account>-<region>` | Versioned, SSE-KMS with a customer-managed key, public access blocked, ACLs disabled, `prevent_destroy`, superseded versions kept 365 days. Bucket policy denies plain HTTP, TLS < 1.2, and any upload overriding the KMS default. |
| Deployment-artifact bucket | `ernote-deploy-<account>-<region>` | Same protections. Holds one non-secret foundation manifest per environment; superseded versions kept 180 days. |
| Encryption key | `alias/ernote-tfstate` | One customer-managed key with annual rotation, used by both buckets. Separation between staging and production is enforced per object in IAM, not per key; split the key later if artefact consumers multiply. |
| State keys | `bootstrap/`, `staging/foundation/`, `staging/release/`, `prod/foundation/`, `prod/release/` (each `.../terraform.tfstate`) | Backend `key` and `region` are committed; `bucket` and `kms_key_id` are passed with `-backend-config` at init. No credentials in `backend.hcl` or tfvars. |
| State policies and roles | `ernote-tfstate-<scope>` policy, `ernote-tfstate-<scope>-operator` role, one pair per root | Each allows only its own state object, grants the lock object separately from the state object, and **explicitly denies** every other root's objects. No policy grants `s3:DeleteObject` on a state key. |
| State-role trust | Account delegation by default | Narrow to exact Identity Center permission-set role ARNs via `state_operator_principals` once they are stable. |
| Account guard | `allowed_account_ids` plus an STS `precondition` per root | Wrong-account execution fails during `plan`, before any resource is created. |
| Release/foundation split | Foundation state owns everything; release state owns ECS services and task definitions only | Release reads a published manifest, not `terraform_remote_state`. CI supplies the image digest as an input; Terraform stays the owner of the task definition. |
| Production | Authored, `enabled = false` in both prod roots | The account boundary decision is still open (see the table above). Prod roots have never been initialised or applied. |
| MFA delete on the state bucket | Not enabled | Requires the account root user and the CLI; cannot be set by Terraform or an assumed role. Decide before production. |

### S3 values to record after the first bootstrap apply

| Value | Source | Recorded |
| --- | --- | --- |
| State bucket name | `terraform -chdir=infra/bootstrap output -raw state_bucket` | Pending |
| State KMS key ARN | `terraform -chdir=infra/bootstrap output -raw state_kms_key_arn` | Pending |
| Deployment-artifact bucket | `terraform -chdir=infra/bootstrap output -raw deploy_artifacts_bucket` | Pending |
| Bootstrap state migrated and local copies destroyed | Runbook step 2 | Pending |
| Bucket protections verified (HTTP denied, versioning on, wrong-key upload denied, prior version recovered) | Runbook step 3 | Pending |
| Live lock collision observed against the real bucket | Runbook step 5 | Pending — mechanism verified locally against MinIO on 2026-09-14 |
| Staging role denied production state and manifest, and the reverse | Runbook step 6 | Pending |
| Wrong-account execution refused | Runbook step 7 | Pending |
| MFA delete decision | Account owner | Pending |

Checks that already pass with no AWS account, on 2026-09-14: `terraform fmt -check -recursive infra/`; `terraform validate` on all five roots; `scripts/tf-manifest-guards.sh` (7 cases — a manifest cannot carry a secret value, a non-Secrets-Manager ARN, or a credential-named resource key); `scripts/tf-lock-check.sh` (a second concurrent operation is refused the lock with HTTP 412).

## Network placement and egress (S4, implemented offline)

Implementation and checks: [`network-verification.md`](network-verification.md).
No cloud apply has been performed; the live connectivity gate remains pending.

| Item | Current choice | Remaining closure |
|---|---|---|
| Placement | Two public, two private task, two isolated DB subnets; staging `10.40.0.0/16`, dormant prod `10.50.0.0/16` | Operator checks CIDR overlap and pins the ordered AZ pair from the reviewed plan |
| NAT | One staging NAT in the first AZ; module supports per-AZ NAT | Record accepted AZ outage/cross-AZ cost tradeoff; choose production availability before enabling |
| Endpoints | S3 gateway on both task route tables; six interface switches off | S5 supplies the audio bucket ARN; selected interface services require actual reviewed resource ARNs |
| Ingress | Closed ALB client CIDRs; ALB SG alone can reach API port 8443 | S8 configures real HTTPS/WAF before opening ingress |
| Database access | Four distinct task SGs, TCP 5432 to the database SG; isolated subnet group | S5 enforces non-public RDS and verified TLS; S6 supplies runtime/one-shot privileges |
| Public IPs | Subnet auto-assignment disabled | S6/S8 must also disable ECS public IP assignment and inspect task ENIs |
| Network idle estimate | $36.50/month for NAT + its IPv4, 730 hours, Ohio, 2026-09-14 | Dated source/rate table in network verification; resolve full-stack budget before provisioning |
| Live gate | Pending | Per-AZ private diagnostic startup/DNS/TLS/service checks and external denial; record sanitized evidence |

## Pre-PHI gate

No real patient audio, transcript, or note enters staging until the appropriate owners document executed BAAs and other agreements; provider retention and three-Region processing approval; recording consent/notice; organizational risk analysis; record retention and legal holds; audit-access design; incident and restore readiness; and authorization/revocation rules. S0 records these gates. It does not certify HIPAA compliance.

## Next verification commands

Run these in a shell that has **not** sourced `.env`. The SSO portal Region remains whatever Identity Center reports; `--region` here is the Bedrock source Region.

```bash
unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN
aws sts get-caller-identity --profile ernote-staging-admin
aws bedrock get-inference-profile \
  --inference-profile-identifier us.anthropic.claude-sonnet-4-6 \
  --region us-east-2 --profile ernote-staging-admin \
  --query '{id:inferenceProfileId,status:status,models:models[].modelArn}' \
  --output json
```
