# Phase 4 Step 9 operations runbook

Implementation date: 2026-11-03. This is an executable procedure, **not evidence
that a live recovery, rotation, or alarm delivery drill has passed**. Use synthetic
staging only until the remaining gates in [Step 9](../planning/phase4-step9-implementation.md)
are accepted. Production remains disabled.

## Incident ownership and first response

Before enabling unattended operation, record named primary/backup contacts and
pager/email addresses in the restricted operator directory for: deployment owner,
database/recovery owner, security/privacy lead, and clinical escalation owner.
No names or response commitments have been supplied to this repository.

1. Acknowledge the SNS notification; record UTC start time, release digest, alarm
   name and environment. Do not put tokens, audio URLs, transcripts or SQL row
   contents in a ticket or ordinary logs.
2. Open `<project>-<environment>-operations`. Check target health, request rate,
   p95/5xx, queue age/depth/DLQ, worker progress, RDS capacity and outbox failures.
3. For access concerns, disable issuance in Cognito and revoke database membership
   using [onboarding](auth-onboarding.md). The next auth check on every task denies
   the existing token. Already admitted requests may finish within the 30-second
   HTTP timeout; already accepted background jobs are not cancelled.
4. For wrong output, unavailable dependencies or runaway cost, close ingress and
   stop the worker through the existing release procedure. Preserve durable rows,
   outbox and queues. Never purge a queue or delete clinical events to clear an alarm.
5. Validate recovery with synthetic authenticated reads/writes and a synthetic
   audio-to-note flow before reopening ingress. Record timestamps and evidence.

Proposed staging targets, requiring an owner to accept and measure: RPO at most
5 minutes and RTO at most 60 minutes. These are objectives, not observed results.

## Deploy rollback

Follow [staging deployment](staging-deploy.md) and the recorded release contract.
Redeploy the previously accepted immutable image digest through the release
workflow; verify readiness, API smoke, worker progress and queue age. Keep the
expanded schema. Do not roll migrations back or remove policies to revive an old
binary. Step 9 changes repository transactions and grants together: **pre-Step-9
images cannot operate with RLS and are not a supported rollback target**. Before
first activation, retain a Step-9-compatible rollback image or plan a forward fix.
A database restore is a separate incident decision, not a routine deploy rollback.

## PITR rehearsal

The verifier reads PostgreSQL only; it starts no server, migration, worker, queue
consumer, STT or AI provider. A restored DB can contain pending jobs and an outbox;
**never attach the normal worker to it**.

1. Select an existing, completed synthetic patient. Stop modifying that patient.
   Record its org/patient IDs in restricted evidence and record the current UTC
   time. Wait for RDS `LatestRestorableTime` to cover that time. Confirm backup
   retention and the source account/region before proceeding.
2. Prepare a separate drill DB subnet group and security group in the VPC. Permit
   only the operator verifier's connection; exclude API/worker SGs. Do not reuse
   the live database's ingress rules. Keep the instance private and encrypted.
3. Restore explicitly to the recorded time (the command creates a billable DB):

   ```bash
   aws rds restore-db-instance-to-point-in-time \
     --source-db-instance-identifier "$SOURCE_DB_ID" \
     --target-db-instance-identifier "$DRILL_DB_ID" \
     --restore-time "$RESTORE_TIME_UTC" \
     --db-subnet-group-name "$DRILL_SUBNET_GROUP" \
     --vpc-security-group-ids "$DRILL_SECURITY_GROUP" \
     --no-publicly-accessible --no-deletion-protection
   aws rds wait db-instance-available --db-instance-identifier "$DRILL_DB_ID"
   ```

   Require `DRILL_DB_ID` to be a new identifier ending in `-drill`. Inspect the
   restored endpoint, encryption, SGs and parameter group before connecting.
4. From a trusted operator host that can reach both private endpoints, supply
   `SOURCE_DB_HOST`, `SOURCE_DB_PORT`, `SOURCE_DB_NAME`, `SOURCE_DB_USERNAME`,
   `SOURCE_DB_PASSWORD` and the corresponding `RESTORE_DB_*` variables through a
   protected environment. Use existing non-owner API/read credentials, never the
   RDS master. Supply `VERIFY_ORG_ID`, `VERIFY_PATIENT_ID`, `APP_ENV=staging` and
   `DB_SSL_ROOT_CERT` pointing to the checked RDS CA bundle. Do not echo secrets
   or store them in a shell history file. Then run:

   ```bash
   cargo run --locked -p er-server --bin er-restore-verify
   ```

   The verifier requires different endpoint/database tuples, enforces verify-full
   TLS in staging, sets read-only sessions, and has a 60-second deadline. It
   compares event identity/order/timestamps/payload digests and a deterministic
   full-event-replay note digest. It rejects an empty fixture. Registration is not
   an event, so replay creation time is anchored to the first persisted event.
   Snapshot acceleration is deliberately bypassed; the verifier does not prove
   audio object retention, external side effects, or snapshot correctness.
5. Save only exit status, digest/count output, restore timestamps, source recovery
   point, isolated SG proof and elapsed restore/verification time. Record measured
   RPO/RTO, discrepancies and operator signoff. A failed comparison is a failed
   drill, even if RDS says `available`.
6. Inventory unfinished jobs/chunks/outbox and account for messages/provider calls
   newer than the recovery point before any real failover. Replaying a restored
   outbox against the live queue can repeat external work. This rehearsal never
   switches the application endpoint.
7. Verify the drill identifier/account again, delete **only the disposable drill**,
   and record deletion. Synthetic-only drill cleanup can use
   `aws rds delete-db-instance --db-instance-identifier "$DRILL_DB_ID" --skip-final-snapshot`.
   Do not apply that command to the source. Remove temporary verifier network access.

Live execution and measured results: **pending**.

## Queue, outbox and incomplete uploads

The worker recovers accepted-but-unsent work from `dispatch_outbox` every 15 seconds.
Each claim leases at most ten IDs for one minute, and a cycle has a 30-second
budget. A crash after send but before acknowledgement can duplicate delivery.
Chunk advisory locks and existing summary/MDM locks prevent concurrent provider
calls for the same work; persisted terminal results prevent subsequent calls.
There is still an external-call-before-durable-result crash window: exactly-once
provider billing is not promised.

Outbox failures alarm after three consecutive minutes. Check DB function grants,
worker SendMessage/KMS permissions and queue reachability. Do not bypass the
outbox or manufacture status changes. Once sent and acknowledged, work is left
to SQS's existing retries and DLQ; the outbox does not repeatedly reset receive
counts for terminal failures. Stuck `running` rows after a DLQ event require
operator reconciliation with the retained message and event history.

Pending upload intents older than 24 hours become `failed/upload_expired` in
batches of at most 1,000. Rows are retained; S3 lifecycle owns bytes. This does
not invent a missing final chunk, complete an interrupted clinical encounter,
or retry a permanently failed provider request. Those remain visible exceptions
for the clinician/operator.

For a DLQ alarm:

1. Confirm whether the failure is malformed input, missing/overwritten audio,
   permissions, provider rejection or a transient outage. Inspect only opaque
   message IDs in restricted tooling, never dump clinical DB rows into logs.
2. Compare the ledger and deterministic event IDs. Terminal successful work needs
   no provider retry; permanently failed work requires an explicit repair decision.
   Redriving a terminal `failed` row alone does not reopen it.
3. Repair the root cause, select a small synthetic subset, and use the scoped
   operator redrive capability described in [data verification](data-verification.md).
   Start with one message, verify resulting note/events/metering, then widen.
4. Kill a synthetic worker task during a job; verify message visibility returns,
   retry behavior and eventual DLQ routing for poison input. Record actual SNS
   email arrival, not merely alarm state. This live drill remains pending.

## Rotation

RDS already uses `manage_master_user_password = true`. RDS manages and rotates
that master secret every seven days by default; it is never a runtime login.
Verify live rotation status and last/next rotation dates in Secrets Manager.
See [AWS RDS password management](https://docs.aws.amazon.com/AmazonRDS/latest/UserGuide/rds-secrets-manager.html).
Do not attach an unrelated Lambda rotation schedule to the managed secret.

Runtime database logins continue to use the tested staged maintenance rotation
in [identity verification](identity-verification.md): close ingress, stop tasks,
bootstrap the exact staged versions, promote only after success, migrate/converge,
replace tasks, test new credentials and deny old credentials. Automatic or
alternating-user zero-downtime runtime rotation is not implemented.

Rotate the Deepgram key quarterly and immediately after suspected compromise:
issue a replacement in the approved provider account, stage it in the worker's
secret, replace workers, prove synthetic STT, revoke the old key and verify denial.
Never revoke first unless incident containment requires immediate downtime.
Import [the quarterly reminder](deepgram-rotation.ics) into the operator calendar;
this repository file does not create a calendar event or notify anyone.
Record owner, date, secret version IDs, replacement task IDs and verification
outcomes in restricted evidence, never the key value.
