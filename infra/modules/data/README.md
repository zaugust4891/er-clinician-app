# S5 data boundary

Shared by both foundation roots. Staging is configured to provision this module;
production calls it only with `enabled = true` and explicit Multi-AZ, backup-retention,
current-audio-retention and noncurrent-audio-retention values. Production approvals
remain pending and its example stays disabled with audio values unset. No cloud apply
is recorded. Run `scripts/tf-data-check.sh`
from the repository root for mocked provider checks.

## What it creates, and who consumes it

| Resource | Application use |
|---|---|
| `aws_db_instance.this` (PostgreSQL 16, private, gp3, encrypted) | Every event, snapshot, chunk row, transcript, note and usage record. API, worker and the one-shot migration connect with `sslmode=verify-full` |
| `aws_db_parameter_group.postgres` | `rds.force_ssl = 1`; statement logging pinned off |
| `aws_s3_bucket.audio` | `AUDIO_BUCKET`. The device PUTs chunks with a presigned URL; the worker downloads them once |
| `aws_sqs_queue.jobs` / `aws_sqs_queue.dlq` | `STT_QUEUE` and its derived `<name>-dlq`. `TranscribeChunk` and `GenerateMdm` jobs, and failures after three attempts |
| `aws_kms_key.data` | Encrypts all three: RDS storage and snapshots, audio objects, queue payloads |

Names are contracts, not labels. The application is given `STT_QUEUE=<name>` and derives
its dead-letter queue as `<name>-dlq` (`Config::stt_dlq`), then resolves both with
`GetQueueUrl`; it never creates anything. `crates/er-server/tests/config_gate.rs`'s
`deployed_queue_names_and_lease_match_the_provisioned_pair` pins that derivation and the
300-second lease this module mirrors.

## Encryption and the key boundary

One customer-managed key covers RDS, S3 and SQS. It is separate from the bootstrap
state/artifact key (`alias/<project>-tfstate`), so application data and Terraform state
never share a grant. S6 scopes who may use it; this module only delegates to account IAM.

**Disabling or scheduling deletion of this key makes the data unrecoverable**, including
snapshots taken while the key was healthy. `kms_deletion_window_days` must cover
`db_backup_retention_days`, up to the 30-day maximum AWS allows — a retention period
longer than 30 days cannot be fully covered, and scheduling deletion then shortens the
recovery horizon below the retention period. Per-tenant keys are deliberately not used;
tenancy is enforced by authenticated repository scoping and server-derived object keys.

S6 must grant, in both IAM **and** the key policy: `GenerateDataKey` for the API role
(a presigned PUT is signed by the API's credentials, so S3 evaluates *its* permissions
when the device uploads), `Decrypt` for the worker, and the SQS producer/consumer
combination. Test those grants against a real upload: S3 Bucket Keys change the
encryption context S3 sends.

## Audio: what versioning does and does not promise

All four public-access blocks are on, ACLs are disabled, SSE-KMS is the bucket default
with bucket keys, and the bucket policy denies plain HTTP, TLS below 1.2, and any upload
that *overrides* the default encryption. Each encryption deny combines `Null = false`
and `StringNotEquals` for the same header: it applies only when the header is present
and wrong. The presigned PUT the API mints sends **no** SSE header, so these denies do
not apply and bucket-default encryption can be used. Correctly signed live uploads
and KMS grants still need verification; mocked tests do not establish that they work.

Three lifecycle rules run on separate clocks — current objects, superseded versions, and
abandoned multipart uploads. There is no storage-class transition: the worker downloads
pending audio with `GetObject` and has no restore workflow, so an archived object would
fail rather than wait. Expiry is asynchronous; it is not a deletion deadline, and it
removes none of the other copies (transcripts in `audio_chunks`, `TranscriptReady`
events, snapshots, exported notes, RDS backups). No Object Lock is configured: a
compliance-mode retention is an approved legal decision, not a default.

**The presigned overwrite race is open and must not be described as immutability.** The
same valid URL can be replayed to overwrite its key, and the completion handler's
`HeadObject` followed by the worker's unversioned `GetObject` does not pin the bytes that
were approved. Versioning makes the overwrite *recoverable*, not impossible. Closing it
means persisting a version id or checksum at completion and having the worker read that
version, or a tested write-once contract — a protocol change deferred to Step 9 while
staging is synthetic.
[Presigned upload behavior](https://docs.aws.amazon.com/AmazonS3/latest/userguide/PresignedUrlUploadObject.html).

`bucket_administration_principal_arns` is empty by default. When set, it adds a deny for
the bucket's own protection settings to every principal outside the list — which must
include the Terraform role that applies this module, because a bucket policy binds every
caller. `aws:PrincipalArn` reports an assumed role's role ARN, not its session ARN.
No `aws:SourceVpce` condition exists anywhere in the policy: the phone and CLI sign
uploads from outside the VPC.

## Queues

One standard queue and its DLQ, because one queue is what the code sends to. Visibility
is 300 seconds to match the worker's lease exactly (a 270-second job budget plus 10
seconds of terminal bookkeeping); long polling is 20 seconds; three deliveries then the
DLQ; retention is 4 days on the main queue and 14 on the DLQ, so a failure outlives the
live work that produced it. `redrive_allow_policy` permits exactly one source queue.
Moving messages back is a separate IAM grant — `dlq_operator_principal_arns` mirrors it
in the queue policy when an operator role exists, and is empty until S6 creates one.

Both queues use SSE-KMS with the data key. Naming a customer-managed key replaces
SQS-managed encryption, and the provider rejects setting both, so `sqs_managed_sse_enabled`
is absent rather than `false`. Message bodies carry opaque identifiers only; no
transcript, name, or provider response appears in a body, attribute, queue name, or
alarm text. [SQS KMS requirements](https://docs.aws.amazon.com/AWSSimpleQueueService/latest/SQSDeveloperGuide/sqs-key-management.html).

## Database

Single-AZ `db.t4g.small` on 50 GiB gp3 with autoscaling capped at 100 GiB, private, with
no public endpoint and no route to NAT or the internet gateway. Verify the class and
storage combination is offered in the region before applying; `postgres_version` defaults
to the bare major so RDS selects a currently supported minor, and an exact minor can be
pinned once verified.

Automated backups are 7 days in synthetic staging, taken in an explicit window, with
`delete_automated_backups = false`. They cannot be disabled: the variable rejects zero.
Production retention is set from production recovery objectives, and the production root
refuses to plan until someone states it. After the first backup exists, confirm
`LatestRestorableTime` advances; a restore rehearsal belongs to Step 9, but an
unavailable backup is already a Step 8 failure.

Removal takes two deliberate changes: deletion protection is on, and
`skip_final_snapshot = false` forces a named final snapshot. `db_final_snapshot_suffix`
exists because AWS still holds the name of a previous teardown's snapshot.

`rds.force_ssl = 1` makes the server refuse plaintext, which is half the guarantee. The
other half is the client: `db.rs` pins `sslmode=verify-full` against the CA bundle baked
into the image at `/etc/ssl/certs/rds-global-bundle.pem` (`DB_SSL_ROOT_CERT`). Rotating
either side is an operations procedure — pin `db_ca_cert_identifier` after confirming the
CA is available in the region, and refresh the image's bundle and its recorded SHA-256
before the current CA expires. Never relax verification to diagnose a TLS failure.
[RDS PostgreSQL TLS](https://docs.aws.amazon.com/AmazonRDS/latest/UserGuide/PostgreSQL.Concepts.General.SSL.html).

The master credential is RDS-managed in Secrets Manager under the data key, so no
password passes through Terraform or its state. It is a bootstrap credential: S6's
one-shot task uses it to create the schema-owner and runtime logins. Its ARN is a module
output and is deliberately **not** in the foundation manifest — no release needs it.

## Deliberately not created

- **No application export bucket.** `note/export` returns text over HTTP; nothing writes
  exports to object storage. Reserve that interface when a consumer is designed.
- **No CloudTrail or log bucket.** Security evidence keeps its own key boundary and
  retention, and belongs to S9, not to audio's lifecycle.
- **No engine log export, Performance Insights, or enhanced monitoring.** Each creates a
  destination with its own retention; S9 owns that decision.

## Interfaces and later-step obligations

| Consumer | Interface | Required integration |
|---|---|---|
| S4 network | `audio_bucket_name` composed in the root | Already wired: the same string scopes the S3 gateway endpoint policy |
| S6 identity | `audio_bucket_arn`, `stt_queue_arn`, `stt_dlq_arn`, `db_arn`, `kms_key_arn`, `db_master_user_secret_arn` | Resource-scoped task/execution policies plus matching key-policy grants; `s3:ListBucket` for the API so a missing upload answers 404 rather than 403 |
| S6 database | `db_host`, `db_port`, `db_name`, `db_master_user_secret_arn` | Non-superuser logins, revoked public schema creation, migrations under the owner |
| S8 release | manifest `audio_bucket`, `stt_queue_name`, `db_endpoint`, `db_port`, `db_name` | Task-definition environment; credentials injected from Secrets Manager, never from the manifest |
| S9 telemetry | `stt_queue_url`, `stt_dlq_url`, `db_identifier` | Queue depth/age and DLQ alarms, and log destinations with their own retention |
| Step 9 protocol | — | Version/checksum binding for audio before any PHI use |

Live checks, costs and pending evidence: [data verification](../../../docs/ops/data-verification.md).
