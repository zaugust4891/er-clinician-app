# Step 8 S0 — staging deployment contract

Updated **2026-10-14**. This is a decision and evidence register for **synthetic-only** staging. Approved bootstrap is applied and verified; application foundation/workloads remain undeployed. It does not approve patient-data processing. Do not put credentials, agreement PDFs, patient data, or full account evidence in this file. Store sensitive evidence in an access-controlled location and add only its owner/date/reference here.

## Confirmed and reported inputs

| Item | Current value | Evidence and status |
| --- | --- | --- |
| Git repository | `zaugust4891/er-clinician-app` | Local `origin` URL; confirmed locally. |
| Default branch | `master` | Operator reports checking GitHub; local checked-out and `origin/master` branches agree. |
| Staging account ID | `657615279057` | Owner-confirmed; independently verified via STS **2026-10-14**. Organization management account, `o-csf6lxxnm8`; resource inventory and scope are in the [audit](step8-audit-2026-10-14.md). |
| Identity Center instance / operator | Organization instance `zamos` (reported); user `zach-admin`, `AdministratorAccess` | Operator-provided STS output on **2026-10-11** confirms assumed role `AWSReservedSSO_AdministratorAccess_e78c48e0012d97ab`, session `zach-admin`. Local SSO profile has `sso_role_name=AdministratorAccess`. The earlier screenshot's IAM user `zach` is separate. |
| Local CLI profile | `ernote-staging-admin` | Operator confirmed default Region `us-east-2` on **2026-10-11**; the profile name is a local label. |
| Identity Center portal URL and Region | `https://ssoins-66849d140aff6f6f.us-east-2.portal.amazonaws.com`; `us-east-2` | Operator report; both values match the local `staging` SSO session configuration. This is separate from the application Region even though both currently have the same value. |
| Staging application and Bedrock **source** Region | `us-east-2` | Owner reconfirmed Ohio on **2026-10-11**. Set cloud `AWS_REGION` and `COGNITO_REGION` to `us-east-2`. |
| Account ownership and root protection | Owner controls root email; root MFA enabled; zero root access keys | Email ownership is owner-confirmed October 11. IAM independently reports `AccountMFAEnabled=1`, `AccountAccessKeysPresent=0` on **2026-10-14**. |
| Current account boundary | `657615279057` for synthetic staging; production disabled | Owner confirmation on **2026-10-11**. No production account ID has been recorded; select and record it before production provisioning. |
| Sonnet foundation model | `anthropic.claude-sonnet-4-6` | Operator's `list-foundation-models` output. Discovery does not prove invocation authorization. |
| Sonnet US geographic profile | `us.anthropic.claude-sonnet-4-6`, **ACTIVE** | Independently verified from **us-east-2, 2026-10-14**; destinations remain the three US Regions below. |
| Profile destination Regions | `us-east-1`, `us-east-2`, `us-west-2` | Operator's returned model ARNs; also listed for a `us-east-2` source in the [AWS Sonnet 4.6 model card](https://docs.aws.amazon.com/bedrock/latest/userguide/model-card-anthropic-claude-sonnet-4-6.html). |
| Bedrock invocation | **One approved synthetic administrator request passed** | 2026-10-14: response `OK.`, 11 input/5 output tokens, estimated **$0.0001188**; agreement subsequently AVAILABLE. Future worker identity remains unverified. |
| Hosted zone | `zamos.co`; `Z0831458W40X3W46HV0S`, public | Zone/type/name servers verified October 14. Owner confirms domain unregistered; authoritative `.co` parent returns NXDOMAIN. Registration/delegation pending. |
| Budget | **$50 USD/month**, `My Monthly Cost Budget` | MONTHLY COST, primary billing view, unblended excluding credits/refunds; actual >85%, actual >100%, forecast >100%. Same one-email recipient recorded privately; owner confirmed control/monitoring/prior delivery October 14. [Current scenario: $166.32 before providers/traffic](staging-cost-review-2026-10-14.md). |
| AWS BAA | Account and organizations BAAs **ACTIVE**, API verified October 14 | Account effective start **2026-08-08T10:57:44.988Z**; organizations **2026-09-14T20:19:43.907Z**. Agreement IDs/scopes in the [audit](step8-audit-2026-10-14.md). Effective start is not separately verified signature date; durable acceptance/vendor evidence pending. |
| Staging retention | Adopt 7-day synthetic audio, 30-day application logs, and 7-day RDS automated backups | Operator decision; implementation and synthetic database-record expiry/rebuild are pending. |
| Production deployment approver | `zach-admin` | Operator report. This records engineering release approval only; privacy, security, legal, and clinical approvals remain separate. |

AWS lists Sonnet 4.6 in `us-east-2` for US geographic and global inference, but not in-Region inference. **Use only the US geographic profile as the staging candidate. Do not substitute the global profile.** A request from Ohio may be processed in any of the three US destinations above; AWS says prompts and outputs may be stored in a destination Region for abuse detection. The destination list and that processing/retention behavior require a recorded privacy/security decision before any patient data. [Model card](https://docs.aws.amazon.com/bedrock/latest/userguide/model-card-anthropic-claude-sonnet-4-6.html) · [Geographic inference](https://docs.aws.amazon.com/bedrock/latest/userguide/geographic-cross-region-inference.html).

Code support (2026-10-09): outside dev the worker requires `BEDROCK_SUMMARY_MODEL_ID` and `BEDROCK_MDM_MODEL_ID`, and `set_bedrock_id` accepts `us.anthropic.claude-sonnet-4-6` alongside the in-Region ID; the global profile, other geographies and ARNs stay rejected. Set both variables to `us.anthropic.claude-sonnet-4-6` in staging. The model default is unchanged. `cost_micros` meters both Sonnet Bedrock IDs at Anthropic's published regional-endpoint rate, 10% above global pricing ($3.30 input, $16.50 output, $0.33 cache read and $4.125 cache write per million tokens), because geographic profiles bill at the source Region's regional price. On **2026-10-14**, official AWS Price List Ohio **Regional CRIS** rows verified these four rates, plus $6.60/million for one-hour cache write. The approved synthetic request subsequently passed. See the [cost worksheet](staging-cost-review-2026-10-14.md). IAM and organization SCPs must permit invocation against the profile and all three model destination Regions. [Cross-Region IAM/SCP guidance](https://docs.aws.amazon.com/bedrock/latest/userguide/inference-profiles-support.html).

The operator's model listing also showed `anthropic.claude-haiku-4-5-20251001-v1:0`, while the repo's Haiku Bedrock ID is `anthropic.claude-haiku-4-5`. The pilot's cloud configuration permits Sonnet only. Resolve Haiku's ID, profile, pricing, and quality decision before enabling Haiku; do not treat that catalog result as authorization.

## Decisions and unknowns to close

| Decision / evidence | Current state | Owner and closure condition |
| --- | --- | --- |
| Account and SSO identity | Completed; independently reverified **2026-10-14** | STS identifies account `657615279057`, SSO administrator `zach-admin`. Root MFA and organization scope independently verified; future task-role checks remain separate. |
| Bedrock source-Region profile | Verified **2026-10-14** | Ohio source, ACTIVE US profile, destinations us-east-1/us-east-2/us-west-2. No global substitution. |
| First-time use, license, and payment | Owner terms review and one-request approval recorded **2026-10-14** | First-use form present; after the approved request, agreement AVAILABLE, authorization AUTHORIZED, entitlement/Region AVAILABLE. This is model-access evidence, not a complete payment-account audit. [AWS model access](https://docs.aws.amazon.com/bedrock/latest/userguide/model-access.html). |
| Invocation and quotas | Administrator invocation passed; worker invocation pending | Applied account limits: **50 RPM**, **6,000,000 TPM**, cross-model cross-Region daily **150,000,000 tokens**. S6/S8 must verify the actual worker role and all destination permissions. |
| Domain and DNS | Public zone verified; domain unregistered | Owner must register under their control, delegate to the existing four AWS name servers and choose a staging hostname before ACM/public DNS. No registration or DNS change performed. |
| Cost guardrail | Owner selected $166+ scenario planning; deployment pending | Owner selected the always-on planning direction October 14. The $50 budget remains unchanged; final limit/provider allowances and budget-update approval remain open. [Worksheet](staging-cost-review-2026-10-14.md) estimates $166.32/month before providers/traffic; Deepgram medical pricing/usage remains unverified. Management-account primary billing view has no staging-only filters. Bootstrap alone approved, ~$1/month plus storage/requests. |
| Alarm contact | Budget inbox control/monitoring/prior delivery owner-confirmed October 14; application alarm destination pending | No test message sent. Choose and verify the S9 application alarm destination before deployment. |
| Staging retention | Synthetic defaults adopted; controls not yet configured | Engineering owner: implement the 7/30/7-day settings and define database-record expiry or environment rebuild. S3 expiration alone does not remove transcripts, events, or backups. |
| Production account | Separate AWS account recommended; no production account ID recorded. Owner reconfirmed synthetic staging and disabled production on **2026-10-11** | Project owner: select and record the production account before production provisioning. The current account is for synthetic staging. |
| Production approval and retention | Engineering deployment approver reported as `zach-admin`; remaining approvals unknown | Project owner and privacy/security reviewers: name separate privacy, security, legal, and clinical approvers; set production audio, clinical records, logs, backup, legal-hold, and access policies. Do not infer a blanket seven-year medical-record rule from HIPAA. |
| AWS BAA / other agreements | AWS API ACTIVE/effective-start/scope/IDs verified; signature evidence and other agreements pending | Store acceptance/signature evidence durably under access control; October 14 raw API metadata is temporary local evidence. Deepgram and applicable vendor/customer agreements remain separate owner decisions. |
| Tracked artifact provenance | Completed within the recorded scope on 2026-10-10: owner confirmed synthetic-only database/shift history and exclusively development/test signing-key usage. Bounded filename inventory; no independent content or complete cache scan claimed | Four runtime database/shift files are untracked and ignored, with preserved local copies and four matching checksum checks. Signing keys remain tracked as auth-test fixtures. Evidence and inventory limits: [artifact provenance record](repository-artifact-provenance.md). The RDS CA bundle is a public trust bundle. |
| Current app authorization | Authenticated and provisioned clinicians are resolved to a database organization | Product/security owner: approve this for synthetic testing only. Role limits, patient assignment, emergency access, revocation delay, and audit-access design remain pre-PHI decisions. |
| Production audio retention and legal holds | Pending; production remains disabled | Record approved current/noncurrent retention days, legal-hold requirements, approver, date, and evidence reference before enabling production. Mocked test values are not approved production settings. |

## Terraform state and ownership (S3, implemented)

Implemented 2026-09-14 in `infra/`. Runbook: [`staging-deploy.md`](staging-deploy.md). Approved bootstrap applied **2026-10-14**; account-specific outputs and live verification are recorded below.

| Item | Value | Notes |
| --- | --- | --- |
| Terraform version floor | `>= 1.10.0, < 2.0.0` | 1.10 is the first release whose S3 backend locks with a `.tflock` object instead of a DynamoDB table. Tested release pinned in `.terraform-version` (`1.10.4`). Upgrading to 1.11+ is encouraged: native S3 locking is generally available there rather than experimental. |
| AWS provider | `~> 6.0`, resolved to `6.64.0` | `.terraform.lock.hcl` committed for all five roots, locked for `linux_amd64`, `linux_arm64`, `darwin_arm64`. |
| State locking | S3 `use_lockfile = true` | Real Terraform S3 collision returned **412 PreconditionFailed**, encrypted lock released normally; all five scoped roles passed lock Get/Put/Delete checks on October 14. MinIO gate also passed. |
| State bucket | `ernote-tfstate-657615279057-us-east-2` | Applied and verified October 14: versioning, approved SSE-KMS/Bucket Keys, public blocks, ACL disabling, 365-day superseded retention, transport/encryption denies. Terraform `prevent_destroy` remains in source. |
| Deployment-artifact bucket | `ernote-deploy-657615279057-us-east-2` | Applied with equivalent protections and 180-day superseded retention; no foundation manifests published yet. |
| Encryption key | `alias/ernote-tfstate` | ARN `arn:aws:kms:us-east-2:657615279057:key/e7c67376-5b46-472c-a016-59a3e104cd25`; rotation enabled, shared by the two bootstrap buckets. Application-data key remains unapplied. |
| State keys | `bootstrap/`, `staging/foundation/`, `staging/release/`, `prod/foundation/`, `prod/release/` (each `.../terraform.tfstate`) | Backend `key` and `region` are committed; `bucket` and `kms_key_id` are passed with `-backend-config` at init. No credentials in `backend.hcl` or tfvars. |
| State policies and roles | `ernote-tfstate-<scope>` policy, `ernote-tfstate-<scope>-operator` role, one pair per root | Each allows only its own state object, grants the lock object separately from the state object, and **explicitly denies** every other root's objects. No policy grants `s3:DeleteObject` on a state key. |
| State-role trust | Exact verified SSO permission-set role only | All five deployed roles trust `arn:aws:iam::657615279057:role/aws-reserved/sso.amazonaws.com/us-east-2/AWSReservedSSO_AdministratorAccess_e78c48e0012d97ab`; no account-wide fallback used. |
| Account guard | `allowed_account_ids` plus an STS `precondition` per root | Wrong-account execution fails during `plan`, before any resource is created. |
| Release/foundation split | Foundation state owns everything; release state owns ECS services and task definitions only | Release reads a published manifest, not `terraform_remote_state`. CI supplies the image digest as an input; Terraform stays the owner of the task definition. |
| Production | Both examples `enabled = false`; state backends initialized only | October 14 backend initialization uses scoped prod-state roles in the current shared bootstrap bucket. It creates no production application resources and does not select a production deployment account. Neither prod root applied. |
| MFA delete on the state bucket | Not enabled | Requires the account root user and the CLI; cannot be set by Terraform or an assumed role. Decide before production. |

### S3 values to record after the first bootstrap apply

| Value | Source | Recorded |
| --- | --- | --- |
| State bucket name | `terraform -chdir=infra/bootstrap output -raw state_bucket` | `ernote-tfstate-657615279057-us-east-2`, verified October 14 |
| State KMS key ARN | `terraform -chdir=infra/bootstrap output -raw state_kms_key_arn` | `arn:aws:kms:us-east-2:657615279057:key/e7c67376-5b46-472c-a016-59a3e104cd25` |
| Deployment-artifact bucket | `terraform -chdir=infra/bootstrap output -raw deploy_artifacts_bucket` | `ernote-deploy-657615279057-us-east-2` |
| Bootstrap state migrated and local copies destroyed | Runbook step 2 | Completed October 14: original resources/outputs match remote state, two no-change plans pass; redundant local state/backups removed after verification. State version/checksum in dated audit/private metadata. |
| Bucket protections verified (HTTP denied, versioning on, wrong-key upload denied, prior version recovered) | Runbook step 3 | Passed on both buckets October 14, with signed requests and a real alternate AWS-managed key. Also omitted-key/SSE-C/unsigned-read checks. Scratch versions removed. |
| Live lock collision observed against the real bucket | Runbook step 5 | Passed October 14 on disposable backend: **412 PreconditionFailed**, approved-key encryption, normal release and scratch cleanup. |
| Staging role denied production state and manifest, and the reverse | Runbook step 6 | Existing cross-root probes denied live in both directions. Deployed-policy simulation explicitly denies actual other state/manifest keys; manifests are unpublished, so live published-manifest read denial remains pending. |
| Wrong-account execution refused | Runbook step 7 | Passed October 14: `allowed_account_ids` refused a wrong-account plan; no apply. |
| MFA delete decision | Account owner | Pending |

Checks that already pass with no AWS account, on 2026-09-14: `terraform fmt -check -recursive infra/`; `terraform validate` on all five roots; `scripts/tf-manifest-guards.sh` (7 cases — a manifest cannot carry a secret value, a non-Secrets-Manager ARN, or a credential-named resource key); `scripts/tf-lock-check.sh` (a second concurrent operation is refused the lock with HTTP 412).

## Network placement and egress (S4, implemented offline)

Implementation and checks: [`network-verification.md`](network-verification.md).
Network/data have not been applied; bootstrap alone is applied and verified. The live
application connectivity gate remains pending. Current prices supersede historical
estimates in the [cost worksheet](staging-cost-review-2026-10-14.md).

| Item | Current choice | Remaining closure |
|---|---|---|
| Placement | Two public, two private task, two isolated DB subnets; staging `10.40.0.0/16`, dormant prod `10.50.0.0/16` | Ohio a/b/c are available; proposed 10.40.0.0/16 does not overlap the inspected default 172.31.0.0/16 VPC. Other-network overlap and final plan/availability decision remain open |
| NAT | One staging NAT in the first AZ; module supports per-AZ NAT | Record accepted AZ outage/cross-AZ cost tradeoff; choose production availability before enabling |
| Endpoints | S3 gateway on both task route tables; six interface switches off | S5 supplies the audio bucket ARN; selected interface services require actual reviewed resource ARNs |
| Ingress | Closed ALB client CIDRs; ALB SG alone can reach API port 8443 | S8 configures real HTTPS/WAF before opening ingress |
| Database access | Four distinct task SGs, TCP 5432 to the database SG; isolated subnet group | S5 enforces non-public RDS and verified TLS; S6 supplies runtime/one-shot privileges |
| Public IPs | Subnet auto-assignment disabled | S6/S8 must also disable ECS public IP assignment and inspect task ENIs |
| Network idle estimate | $36.50/month for NAT + its IPv4, 730 hours, Ohio, 2026-09-14 | Dated source/rate table in network verification; resolve full-stack budget before provisioning |
| Live gate | Pending | Per-AZ private diagnostic startup/DNS/TLS/service checks and external denial; record sanitized evidence |

## Database connection settings (S6)

Implemented in `crates/er-server/src/config.rs` and `db.rs` and used by `er-server`,
`er-worker` and `er-migrate`. Outside dev `DATABASE_URL` is rejected, and combining it with
any setting below is rejected everywhere. The parts are composed with `PgConnectOptions`, so
no password is URL-escaped or stored a second time inside a URL.

| Setting | Value source | Secret |
| --- | --- | --- |
| `DB_HOST` | Manifest `db_endpoint`: hostname only, verified against the server certificate | No |
| `DB_PORT` | Manifest `db_port` | No |
| `DB_NAME` | Manifest `db_name` | No |
| `DB_USERNAME` | ECS task secret, `valueFrom = "<login secret ARN>:username::"` | Injected |
| `DB_PASSWORD` | ECS task secret, `valueFrom = "<login secret ARN>:password::"` | Injected |
| `DB_SSL_ROOT_CERT` | `/etc/ssl/certs/rds-global-bundle.pem`, packaged in the image | No |

Each login secret is JSON shaped like the RDS-managed master secret,
`{"username": "...", "password": "..."}`. Each process receives only its own login: the API
its API login, the worker its worker login and `er-migrate` the schema-owner login. No running
service receives the master secret. ECS resolves secrets when a task starts, so a rotated
value reaches a process only through a replacement task.

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

## S6 identity implementation (2026-10-15)

S6 is implemented locally; its AWS/RDS/ECS acceptance is unperformed. The exact
role matrix, DB grant decisions, secret staging/pinning/promotion and maintenance
rotation procedure are in [identity-verification.md](identity-verification.md).
The image adds `er-db-bootstrap`; only that one-shot execution role receives the
RDS master. Four separate logins cover schema ownership, API, worker and privileged
identity onboarding. No secret values enter Terraform or the release manifest.

CI role templates default to disabled. S11 must configure GitHub environment branch
protections, adopt the OIDC provider and review the final infrastructure provisioning
policy before enabling them. Application resources remain unapplied pending the
budget decision and S7–S9 dependencies; production remains disabled.
