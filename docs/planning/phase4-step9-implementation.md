# Phase 4 Step 9 implementation

Implemented 2026-11-03–04. This document covers repository changes for Step 9 of
[the implementation guide](phase4-implementation-guide.md#step-9--hardening-observability-drills)
and the related Step 8 hardening handoffs. Live AWS acceptance is still open;
this is not a patient-use or security-review signoff.

## Tenant enforcement

The new migration enables RLS on events, snapshots, patients, shifts,
shift_patients, audio_chunks, jobs, usage_log, quotas, read audit, audio bindings
and dispatch outbox. Tenant policies require `app.org_id` for both visibility and
writes. The roster join policy requires both parent rows to be visible. Missing
context returns no rows and rejects writes.

Every repository SQL query now executes inside a transaction with bound
`set_config('app.org_id', org_uuid, true)`. Existing multi-statement transactions
use the same entry point. Application org predicates remain. Commit, rollback
and cancellation end the setting's lifetime before the pool reuses the connection.
`tenant_query!` supplies this boundary for former autocommit queries without
changing their SQLx checked query text.

The worker's queue message still contains IDs only. Restricted SECURITY DEFINER
functions return only the owning org UUID, then the worker reads the row through
the scoped repository. Additional restricted functions claim/acknowledge outbox
IDs and expire abandoned uploads. Every function pins its search path and revokes
PUBLIC execution; grant convergence permits only the worker's explicit functions.
Triggers capture dispatch intent without granting runtime arbitrary outbox INSERT.

RLS is intentionally not forced on the schema owner: maintenance, migration and
these fixed-function lookups need owner access. Cloud startup already rejects
runtime owner membership, superuser and BYPASSRLS credentials. Non-owner runtime
role tests deliberately omit the org WHERE clause, reject foreign writes, exercise
normal API/worker paths, and verify connection reuse after commit and rollback.
RLS protects against a forgotten filter, not a compromised trusted application
that can set arbitrary tenant context. This follows PostgreSQL's documented
[owner and policy semantics](https://www.postgresql.org/docs/16/ddl-rowsecurity.html)
and [security-definer safeguards](https://www.postgresql.org/docs/16/sql-createfunction.html).

## Auth, quotas and read evidence

Membership is resolved from PostgreSQL on each request. Only active `physician`
rows are admitted; other roles fail closed. `disabled_at` or the existing
onboarding revocation takes effect at the next authentication check across all
replicas. Already admitted requests/jobs are not cancelled. Role provisioning
and hospital-specific patient assignments/MFA still require product/security
acceptance; the current access model is physician access within an organization.

Authenticated fixed UTC-minute quotas are 60 general requests, 5 MDM requests and
30 audio intents per clinician. The general quota also applies to expensive
requests. PostgreSQL UPSERT arbitrates concurrent replicas, storing just one row
per clinician/bucket rather than an unbounded request history. This replaces the
guide's suggested process-local `tower_governor` implementation to avoid multiplying
limits across ECS tasks. Boundary bursts can reach two windows' allowance; these
are request quotas, not daily spend ceilings. Retries count. Denial is JSON 429
with `Retry-After: 60`; storage failure is 503. Health/readiness remain outside auth.

Authenticated GETs except `/v1/me` insert a durable **access-attempt** row before
the handler releases data: clinician, org, matched route template, first resource
UUID if present, server timestamp. No token, query string, raw path or content is
stored. Failed writes block the read with 503. The API can INSERT but cannot read,
modify, delete or truncate this evidence. Attempts include later 404s/validation
failures; they do not assert that a response was delivered. Roster accesses identify
the shift, not each patient. RDS retention protects the DB copy; external immutable
archival, record-level audit scope and legal retention remain pre-PHI decisions.

## Audio and recovery

Completion records the first verified S3 ETag/version and uploaded status in one
transaction. Cloud requires a non-null version ID. Workers read that version with
an If-Match condition; unversioned development storage uses the ETag condition.
Replaying a PUT cannot silently replace the audio later transcribed. Completion
retries preserve the winning binding and no longer revalidate an unrelated latest
version. Existing accepted rows lacking a binding fail closed in the worker and
need reviewed re-completion/migration; no fabricated binding is backfilled.

An outbox trigger commits dispatch intent with MDM receipt insertion or the
pending→uploaded transition. API sends retain their low-latency path and clear the
intent after SQS success. A worker recovery loop leases ten due IDs, retries failed
sends and acknowledges only after success. It runs alongside polling and is
cancelled when the normal worker shuts down. A crash after send may duplicate work;
a chunk advisory lock now also serializes concurrent STT deliveries. Existing
summary/MDM locks and event IDs continue to supply idempotency. Provider success
before local persistence can still incur duplicate billing after a crash.

The worker also expires at most 1,000 pending intents older than 24 hours per
recovery pass, retaining failed rows and leaving object deletion to S3 lifecycle.
Missing final chunks, terminal failures, restored outbox reconciliation and DLQ
redrive remain explicit operator/clinical decisions, not automatic note completion.

## Observability and operations

The operations dashboard includes request rate, p50/p95, target/ALB 5xx, queue
size/age/DLQ, RDS connections/CPU/free storage, STT audio seconds, MDM job duration,
worker progress and metering failures. Task-count panels/alarms are generated
when Container Insights is enabled in the existing cluster configuration.

Added RDS CPU >80% for 15 minutes, p95 >500 ms for 3/5 minutes, per-service task
minimums (API 2, worker 1), and persistent outbox-failure alarms. Storage now uses
20% of configured allocated capacity; review after storage autoscaling. 5xx
alarms aggregate five-minute windows instead of counting three separate noisy
minutes. Existing encrypted SNS destinations, DLQ/queue/progress alarms and
actual/forecast budgets are retained. The reviewed $50 default remains; the older
guide's $400 example does not authorize raising it. Alarm actions and Container
Insights remain explicitly configured; no Terraform apply was run.

The [runbook](../ops/runbook.md) covers restore isolation, rollback compatibility,
DLQ/outbox recovery, incident contacts, proposed RPO/RTO, RDS-managed master
rotation, staged runtime credential rotation and quarterly Deepgram rotation.
`er-restore-verify` is an operator-only binary that compares a frozen synthetic
patient's event and replay digests across source/restored databases with verified
TLS and read-only sessions. It has no queue/provider/server path. A recurring ICS
reminder is supplied for import; no calendar or message was sent.

## Dependency hardening

The Step 9 audit found new RustSec advisories in the pre-existing dependency tree.
JWT verification now uses jsonwebtoken's supported AWS-LC backend. The regional
S3 client no longer enables unused SigV4a. Together those changes remove the
affected `crypto-bigint` dependency instead of suppressing
[RUSTSEC-2026-0387](https://rustsec.org/advisories/RUSTSEC-2026-0387.html).
The lockfile updates `ctutils` to 0.4.4 and Tokio to 1.53.3 for
[RUSTSEC-2026-0385](https://rustsec.org/advisories/RUSTSEC-2026-0385.html) and
[RUSTSEC-2026-0386](https://rustsec.org/advisories/RUSTSEC-2026-0386.html).
The existing RSA exception is unchanged and no new exception was added. The audit
policy still reports the existing `lru` unsoundness and yanked-package warnings;
passing that policy does not mean there are no remaining dependency findings.

## Rollout order and remaining live gates

1. Apply reviewed worker IAM, S3 endpoint and monitoring changes. Enable Container
   Insights for task-count coverage and confirm the desired counts match release.
2. Drain/stop old API and worker tasks; migrate as the schema owner and converge
   grants before starting Step 9 binaries. Old images have no tenant context and
   cannot serve this schema. Preserve a compatible rollback image.
3. Inventory pre-existing uploaded/transcribing chunks without bindings and
   queued/running work. Resolve them with synthetic evidence; do not point old
   receipts at the current latest S3 object by assumption.
4. Verify RLS with live runtime credentials, version-specific reads through the
   gateway endpoint, encrypted outbox sends, quotas across two API tasks,
   revocation, read-audit failures, metrics and SNS delivery.
5. Perform the isolated PITR replay drill and worker-kill/DLQ/email drill, verify
   master/runtime/provider rotation, assign named incident owners, and record
   outcomes in the [security checklist](../ops/step9-security-checklist.md).

Unclosed pre-PHI handoffs: minimum-necessary patient assignment and MFA policy,
audit export/retention approval, real provider/rotation failure drills, clinical
recovery decisions, BAAs/legal retention and security/privacy review. Neither
unit tests nor authored Terraform closes those gates.

## Verification

The final isolated application gate passed on 2026-11-04: 386 test executions
across workspace/standalone AI runs, Clippy, formatting, SQLx freshness and
missing-service checks. Changed Terraform module/foundation/network tests and the
existing dependency audit policy passed. See [the dated checklist](../ops/step9-security-checklist.md)
for commands, remaining dependency warnings and unperformed live gates. Local regression tests cover non-owner RLS and privilege paths,
concurrent quotas, all-route missing-token/wrong-role/disabled-user denial,
audit fail-closed behavior, deterministic replay evidence, overwrite rejection,
and outbox recovery without a returning client. Existing lifecycle, queue lease,
DLQ, cross-org and log privacy tests remain part of the full application gate.

## Files updated

The following inventory records files changed for this implementation. A
pre-existing deletion of `docs/ops/step8-live-aws-checklist.md` was left untouched.

<!-- FILE_INVENTORY -->

| File | Change |
| --- | --- |
| [.gitignore](../../.gitignore) | Keep the new Step 9 implementation record visible to version control. |
| `.sqlx/query-fb3ff9abd08f99be07c7d5c2ce78f9988eb69e6214e808ad33b881051b7ba14a.json` (removed) | Remove obsolete cached metadata for the replaced membership query. |
| [CLAUDE.md](../../CLAUDE.md) | Record the new behavior and supersede historical Step 8 deferrals. |
| [Cargo.lock](../../Cargo.lock) | Resolve the AWS-LC JWT feature graph and patched ctutils/Tokio dependencies. |
| [Cargo.toml](../../Cargo.toml) | Disable unused S3 SigV4a and its affected crypto dependency. |
| [README.md](../../README.md) | Link implementation, operations and acceptance evidence. |
| [crates/er-server/Cargo.toml](../../crates/er-server/Cargo.toml) | Select the supported JWT backend and register the operator restore binary. |
| [crates/er-server/migrations/20261103000000_hardening.sql](../../crates/er-server/migrations/20261103000000_hardening.sql) | Install RLS, membership disablement, quotas, audit/binding/outbox tables and restricted recovery functions/triggers. |
| [crates/er-server/src/audio.rs](../../crates/er-server/src/audio.rs) | Support conditional, version-specific bounded audio downloads. |
| [crates/er-server/src/auth/claims.rs](../../crates/er-server/src/auth/claims.rs) | Resolve unit-test keys from the current checkout when cached binaries are reused. |
| [crates/er-server/src/auth/keys.rs](../../crates/er-server/src/auth/keys.rs) | Fix JWKS unit-test fixture paths across disposable CI checkouts. |
| [crates/er-server/src/auth/middleware.rs](../../crates/er-server/src/auth/middleware.rs) | Apply clinician quotas and durable read-attempt auditing after authentication. |
| [crates/er-server/src/auth/resolver.rs](../../crates/er-server/src/auth/resolver.rs) | Resolve active physician membership on every request without a revocation cache. |
| [crates/er-server/src/bin/restore_verify.rs](../../crates/er-server/src/bin/restore_verify.rs) | Compare source/restored replay evidence; enforce read-only mode across reconnects without providers or workers. |
| [crates/er-server/src/bin/worker.rs](../../crates/er-server/src/bin/worker.rs) | Run bounded dispatch recovery alongside normal polling and shutdown. |
| [crates/er-server/src/hardening.rs](../../crates/er-server/src/hardening.rs) | Implement shared fixed-window quotas, read audit and static denial responses. |
| [crates/er-server/src/lib.rs](../../crates/er-server/src/lib.rs) | Export hardening, outbox and recovery modules. |
| [crates/er-server/src/outbox.rs](../../crates/er-server/src/outbox.rs) | Lease and dispatch accepted work, acknowledge success, and expire old intents. |
| [crates/er-server/src/privileges.rs](../../crates/er-server/src/privileges.rs) | Classify new relations/routines and test non-owner tenant, recovery, quota and audit boundaries. |
| [crates/er-server/src/recovery.rs](../../crates/er-server/src/recovery.rs) | Build stable event and full-replay note digests for a known patient. |
| [crates/er-server/src/repository.rs](../../crates/er-server/src/repository.rs) | Scope every query transaction; add restricted worker lookup, immutable binding and dispatch acknowledgement methods. |
| [crates/er-server/src/routes.rs](../../crates/er-server/src/routes.rs) | Bind first audio completion and acknowledge durable dispatch after queue success. |
| [crates/er-server/src/state.rs](../../crates/er-server/src/state.rs) | Require new runtime relations and audit INSERT privilege in readiness checks. |
| [crates/er-server/src/telemetry.rs](../../crates/er-server/src/telemetry.rs) | Emit fixed-field STT audio duration and MDM job duration metrics. |
| [crates/er-server/src/worker.rs](../../crates/er-server/src/worker.rs) | Read bound audio, serialize duplicate chunk processing and record duration metrics. |
| [crates/er-server/tests/cognito_token_gate.rs](../../crates/er-server/tests/cognito_token_gate.rs) | Require revocation on the current task without API replacement. |
| [crates/er-server/tests/hardening_gate.rs](../../crates/er-server/tests/hardening_gate.rs) | Test route/method authorization, quota responses, audit failure and replay evidence. |
| [crates/er-server/tests/worker_gate.rs](../../crates/er-server/tests/worker_gate.rs) | Bind fixtures and test overwrite rejection, version reads, outbox recovery and intent expiry. |
| [docs/api-contract.md](../api-contract.md) | Document quotas, roles/revocation, 429/503 responses, bound audio and dispatch recovery. |
| [docs/ops/auth-onboarding.md](../ops/auth-onboarding.md) | Replace cached-membership offboarding with next-request denial. |
| [docs/ops/deepgram-rotation.ics](../ops/deepgram-rotation.ics) | Provide an importable quarterly operator reminder. |
| [docs/ops/runbook.md](../ops/runbook.md) | Describe incident response, compatible rollback, isolated PITR, DLQ recovery and rotation. |
| [docs/ops/step9-security-checklist.md](../ops/step9-security-checklist.md) | Record dated local test results, remaining findings and live acceptance checks. |
| [docs/planning/phase4-implementation-guide.md](phase4-implementation-guide.md) | Link the original Step 9 requirements to the implementation record. |
| [docs/planning/phase4-step9-implementation.md](phase4-step9-implementation.md) | Explain implementation decisions, rollout limits and this complete file inventory. |
| [infra/envs/prod/foundation/observability.tf](../../infra/envs/prod/foundation/observability.tf) | Wire storage capacity and optional service desired counts while retaining production gating. |
| [infra/envs/staging/foundation/observability.tf](../../infra/envs/staging/foundation/observability.tf) | Wire storage capacity and optional service desired counts into monitoring. |
| [infra/modules/identity/tasks.tf](../../infra/modules/identity/tasks.tf) | Allow worker version reads and recovery sends with scoped queue/KMS permissions. |
| [infra/modules/identity/tests/identity.tftest.hcl](../../infra/modules/identity/tests/identity.tftest.hcl) | Pin the updated worker permission boundary. |
| [infra/modules/network/endpoints.tf](../../infra/modules/network/endpoints.tf) | Permit versioned audio reads through the scoped S3 gateway policy. |
| [infra/modules/observability/alarms.tf](../../infra/modules/observability/alarms.tf) | Add sustained RDS CPU, capacity-relative storage and five-minute 5xx windows. |
| [infra/modules/observability/dashboard.tf](../../infra/modules/observability/dashboard.tf) | Define operations panels, latency/task/recovery alarms and duration filters. |
| [infra/modules/observability/tests/observability.tftest.hcl](../../infra/modules/observability/tests/observability.tftest.hcl) | Verify dashboard panels, thresholds, desired task counts and alarm delivery configuration. |
| [infra/modules/observability/variables.tf](../../infra/modules/observability/variables.tf) | Accept allocated database capacity and optional desired service counts. |
