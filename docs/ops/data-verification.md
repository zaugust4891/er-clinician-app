# Step 8 S5 data verification

Written 2026-09-17. **Authored and tested offline; not applied to AWS.** No database,
bucket, queue or key exists in any account. The live checks below need the applied
foundation, S6's roles and database logins, and an approved diagnostic image; their
status is pending. Staging is synthetic-only — this record does not authorise patient
audio, transcripts, or notes.

## Default choices

| Decision | Staging value | Why, and what it costs |
|---|---|---|
| Engine | PostgreSQL 16, bare major version | RDS selects a currently supported minor at creation; pin an exact minor once `describe-db-engine-versions` confirms it for the region |
| Class and storage | `db.t4g.small`, single-AZ, 50 GiB gp3, autoscaling capped at 100 GiB | The plan's starting point. Single-AZ is a cost choice: an AZ failure is an outage and a restore, not a failover |
| Automated backups | 7 days, 07:00–07:30 UTC, `delete_automated_backups = false` | S0's synthetic default. Zero is rejected by the module — an unavailable backup is already a Step 8 failure |
| Maintenance | Sunday 08:00–09:00 UTC, minor upgrades allowed | Does not overlap the backup window |
| Transport | `rds.force_ssl = 1`, client `sslmode=verify-full` | Server refusal plus client CA and hostname verification. Either alone is insufficient |
| Audio | 7-day current expiry, 7-day noncurrent expiry, 7-day multipart abort, no transitions | S0's synthetic default. The worker cannot transcribe an archived object, so nothing is tiered |
| Queue | Standard, 300 s visibility, 20 s long poll, `maxReceiveCount = 3`, 4-day retention | Visibility matches the worker's lease exactly (270 s job budget + 10 s terminal work) |
| Dead-letter queue | `<queue>-dlq`, 14-day retention, single permitted source | The name is what `Config::stt_dlq` derives; failures outlive the live work |
| Encryption | One customer-managed key for RDS, S3 and SQS, annual rotation, 30-day deletion window | Separate from the bootstrap state key. Per-tenant keys are not justified by any current contract |
| Master credential | RDS-managed in Secrets Manager under the data key | No password in Terraform state. A bootstrap credential only; no running service receives it |

### The 7-versus-35-day question, resolved

The older plan carried both numbers. **Synthetic staging uses 7 days**, matching S0's
adopted retention. **Production sets its own**, from production recovery objectives: the
production root refuses to plan until `db_multi_az` and `db_backup_retention_days` are
stated explicitly, so staging's convenience cannot become a production default by
inheritance. A retention period longer than 30 days cannot be fully covered by the KMS
deletion window AWS allows; record that gap when choosing a longer production value.

Backups, S3 historical versions, transcripts in `audio_chunks`, raw `TranscriptReady`
events, snapshots and exported notes are **separate copies**. Expiring audio removes one
of them. A deletion or legal-hold workflow has to name all of them, and does not exist yet.

## Cost assumptions

These are the on-demand rates used in the earlier cost model, carried forward for
`us-east-2` at 730 hours per month. **They were not re-fetched from AWS pricing pages for
this change**; confirm each line before provisioning, and treat the result as an estimate
rather than billing evidence.

| Component | Assumption | Estimated monthly idle cost |
|---|---|---|
| `db.t4g.small`, single-AZ PostgreSQL | 730 × ~$0.032/hour | ~$23 |
| 50 GiB gp3 database storage | 50 × ~$0.115/GB-month | ~$6 |
| Automated backup storage | Free up to 100% of allocated storage; synthetic data is far below that | ~$0 |
| Customer-managed KMS key | $1/key-month plus per-request charges | ~$1 |
| Audio objects and requests | Days of synthetic audio at ~$0.023/GB-month | <$1 |
| SQS | Well inside the 1M-request free allowance | ~$0 |
| **S5 subtotal** | | **~$31** |
| S4 network (already recorded) | NAT + its IPv4 | $36.50 |

Multi-AZ roughly doubles the instance line. So does an idle second environment. The two
subtotals together already exceed the reported $50 testing budget before ECS, the ALB,
logs, WAF, Deepgram or Bedrock; the full-stack estimate and the budget decision remain
open in the [deployment contract](deployment-contract.md).
[RDS pricing](https://aws.amazon.com/rds/postgresql/pricing/),
[KMS pricing](https://aws.amazon.com/kms/pricing/), [S3 pricing](https://aws.amazon.com/s3/pricing/).

## Local gates

```bash
terraform fmt -check -recursive infra/
./scripts/tf-data-check.sh
```

The script copies only Terraform source, tests and root lockfiles into a temporary
directory, then runs mocked-provider tests for `infra/modules/data/` and both foundation
roots. It excludes operator tfvars, state and backend settings, needs no AWS credentials,
and creates nothing. Set `TF_DATA_PLUGIN_DIR` to a provider filesystem mirror for offline
installation (`scripts/tf-network-check.sh` uses `TF_NETWORK_PLUGIN_DIR` the same way).

Verified locally on 2026-09-17 with Terraform 1.10.4 and AWS provider 6.64.0:
**22 test runs passed** — 12 in the module (defaults, explicit retention and operator
grants, and ten rejected inputs), 4 in the staging foundation, 6 in production
(disabled, both missing-decision refusals, and explicitly enabled). `tf-network-check.sh`
now runs 23 (its 13 module runs plus the foundation roots' network *and* data files, up
from 17), `tf-manifest-guards.sh` runs its 7 cases, and all five roots validate. `cargo test -p er-server --test config_gate` passes, including the new
`deployed_queue_names_and_lease_match_the_provisioned_pair`, which pins the queue naming
and 300-second lease this module mirrors.

## Inspect the applied resources

Follow the [staging runbook](staging-deploy.md) for SSO and backend setup. After the
complete foundation applies, collect non-secret configuration only.

```bash
terraform -chdir=infra/envs/staging/foundation output -json data
# Set DATA_DB_ID, DATA_PG_GROUP, DATA_BUCKET, DATA_QUEUE_URL, DATA_DLQ_URL and
# DATA_KEY_ARN from its db_identifier, db_parameter_group_name, audio_bucket_name,
# stt_queue_url, stt_dlq_url and kms_key_arn fields.

aws rds describe-db-instances --db-instance-identifier "$DATA_DB_ID" \
  --query 'DBInstances[].{public:PubliclyAccessible,encrypted:StorageEncrypted,key:KmsKeyId,
    multiAz:MultiAZ,backupDays:BackupRetentionPeriod,window:PreferredBackupWindow,
    protection:DeletionProtection,ca:CACertificateIdentifier,subnets:DBSubnetGroup.DBSubnetGroupName,
    groups:VpcSecurityGroups[].VpcSecurityGroupId,latestRestorable:LatestRestorableTime}'

aws rds describe-db-parameters --db-parameter-group-name "${DATA_DB_ID%-pg}-pg-postgres16" \
  --query 'Parameters[?ParameterName==`rds.force_ssl`].[ParameterValue,ApplyType]'

aws s3api get-public-access-block --bucket "$DATA_BUCKET"
aws s3api get-bucket-versioning --bucket "$DATA_BUCKET"
aws s3api get-bucket-encryption --bucket "$DATA_BUCKET"
aws s3api get-bucket-lifecycle-configuration --bucket "$DATA_BUCKET"
aws s3api get-bucket-policy --bucket "$DATA_BUCKET" --query Policy --output text

aws sqs get-queue-attributes --queue-url "$DATA_QUEUE_URL" --attribute-names All \
  --query 'Attributes.{visibility:VisibilityTimeout,wait:ReceiveMessageWaitTimeSeconds,
    retention:MessageRetentionPeriod,redrive:RedrivePolicy,key:KmsMasterKeyId}'
aws sqs get-queue-attributes --queue-url "$DATA_DLQ_URL" --attribute-names All \
  --query 'Attributes.{retention:MessageRetentionPeriod,allow:RedriveAllowPolicy,key:KmsMasterKeyId}'

aws kms describe-key --key-id "$DATA_KEY_ARN" --query 'KeyMetadata.{state:KeyState,manager:KeyManager}'
aws kms get-key-rotation-status --key-id "$DATA_KEY_ARN"
```

Expected: `PubliclyAccessible = false`, encryption on with the data key, backups
non-zero with the chosen window, deletion protection on, `rds.force_ssl = 1`, all four
public-access blocks true, versioning `Enabled`, SSE-KMS with the data key, three
lifecycle rules with **no** transition, visibility 300, wait 20, `maxReceiveCount = 3`,
the DLQ permitting exactly one source queue, both queues KMS-encrypted, and key rotation
enabled. Confirm `LatestRestorableTime` advances once the first backup exists; a
`LatestRestorableTime` that never moves is a Step 8 failure, not a Step 9 one.

## Prove the upload contract, positively and negatively

The point of the bucket policy is that a **correctly signed upload with no encryption
headers succeeds** while everything else fails. Test all five with synthetic bytes:

| Case | Expected |
|---|---|
| Presigned PUT from outside the VPC, `Content-Type` only, no SSE headers | 200; the stored object shows `ServerSideEncryption=aws:kms` and the data key |
| Same URL with `x-amz-server-side-encryption: AES256` | 403 |
| Same URL with a different KMS key id | 403 |
| Unsigned/public `GET` of the stored key, and any `http://` request | 403 |
| Presigned PUT replayed after `expires_in_secs` | 403 |

```bash
aws s3api head-object --bucket "$DATA_BUCKET" --key "$SYNTHETIC_KEY" \
  --query '{sse:ServerSideEncryption,key:SSEKMSKeyId,bucketKey:BucketKeyEnabled,type:ContentType,len:ContentLength}'
```

Record the assertion names and status codes. Do not paste presigned URLs, object bodies
or credentials into this file.

**Known open behavior, not a defect to be hidden:** replaying a still-valid presigned URL
overwrites its key, and the completion handler's `HeadObject` followed by the worker's
unversioned `GetObject` does not pin the approved bytes. Versioning makes that
recoverable, not impossible. Do not describe staging audio as immutable. Closing it
(persisted version/checksum binding, or a tested write-once contract) is Step 9 work and
is required before any PHI.

## What S6 must still prove

S5 creates the resources and their resource policies; it grants no identity anything.
These belong to S6's permission probes and are listed here so they are not forgotten:

- API role: `PutObject`, `GetObject`, `ListBucket` on this bucket, `SendMessage` and
  `GetQueueUrl`, and the KMS `GenerateDataKey`/`Decrypt` that a presigned PUT and an
  encrypted queue message actually require.
- Worker role: `GetObject`, receive/delete/change-visibility, `GetQueueUrl` on both
  queues, `Decrypt` — and **no** audio `PutObject`.
- Neither runtime role may delete the bucket, purge a queue, change retention or
  lifecycle, disable or schedule deletion of the key, modify the database, or read
  backups and snapshots. Probe each of those negatively with the real role.
- Test the KMS grants against a real upload: S3 Bucket Keys change the encryption context
  S3 sends, so an untested `kms:EncryptionContext` condition can pass review and fail in
  production.

## Evidence

| Evidence | Status / owner |
|---|---|
| Module, foundation wiring, manifest keys, offline gates | Implemented locally; engineering |
| Verified account, region class/storage availability, pinned engine minor | Pending; operator |
| Applied resource inspection (the commands above) | Pending apply; operator |
| `LatestRestorableTime` observed advancing | Pending first backup; operator |
| Signed upload succeeds without SSE headers; wrong algorithm/key/public/insecure denied | Pending apply and a synthetic client; engineering |
| Object encryption metadata inspected | Pending apply; engineering |
| Runtime roles cannot change these controls | Pending S6; engineering |
| Restore rehearsal | Step 9; engineering |
| Audio version/checksum binding | Step 9 protocol change; engineering |
| Actual RDS/KMS/S3/SQS charges | Pending billing; operator |
