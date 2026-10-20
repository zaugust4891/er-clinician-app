# Step 8 S9 — telemetry and deployment alarms

Implementation dated **2026-10-19**. Code and local checks are complete; the
validation record is below. **AWS acceptance is pending:** this work does not
apply the foundation, subscribe an inbox, send a notification, or enable services.
Staging remains synthetic only. See the [live checklist](step8-live-aws-checklist.md).

## Destinations and ownership

| Destination | Collected data | Retention and access |
|---|---|---|
| `/ecs/<project>-<environment>/{api,worker,migration,db-bootstrap,onboarding}` | Allowlisted JSON; fixed error codes; random request/delivery correlation; bounded aggregate worker counters | 14 days by default (7/14/30 selectable); dedicated rotating KMS key; each S6 execution role writes its own group; explicit telemetry operator role reads |
| `<project>-<environment>-audit-<account>-<region>` | Multi-region CloudTrail management events and digests; optional audio object events | 365 days for current and noncurrent versions by default; SSE-KMS, versioning, public access blocked, manual object deletion denied; only telemetry operator role can read objects |
| SNS `<project>-<environment>-alarms` | Fixed alarm names, infrastructure metrics, budget amounts | KMS encrypted; exact account/source-scoped CloudWatch and Budgets publishers; reviewed operator emails must confirm subscriptions |
| WAF | Aggregate native rule metrics | Raw request logging and request sampling remain disabled in S8 |
| ALB | Native target health and error metrics | Access logging remains disabled; no sensitive access-log bucket is created |
| PostgreSQL | Native RDS metrics; exceptional PANIC engine messages only | No CloudWatch engine export, Performance Insights, or enhanced-monitoring destination |

Foundation owns `infra/modules/observability/`. Production instantiates nothing
until its existing `enabled` gate is explicitly set. The versioned release manifest
now contains the three application log names, alarm topic ARN and
`telemetry_notifications_enabled`. Operator-only groups remain outside release
privileges. All five task templates inject `RELEASE_ID` from the pinned image digest.

The audit bucket and telemetry key have `prevent_destroy`; the bucket also refuses
force deletion and explicit object deletes. This is **not immutable/WORM storage**:
privileged administrators can change policies, and the configured lifecycle expires
objects. No ordinary application/task/CI role receives log-read or export access.
The telemetry operator role permits group-scoped reads/queries/live-tail and audit
object reads, and explicitly denies CloudWatch exports/subscription destinations.
Query-result/stop APIs require `Resource = "*"`; query creation is group-scoped.
Account administrators retain IAM/key-policy administration; this is not an SCP.

Use exact IAM operator role ARNs in `telemetry.operator_principal_arns`, then assume
`<project>-<environment>-telemetry-operator` to inspect destinations. Do not paste
raw log lines, query results, notification addresses, tokens or object keys into CI.
The input addresses and principal ARNs are deployment metadata stored in Terraform
state; supply them through the existing ignored local tfvars mechanism.

## Application logging contract

All five ECS binaries initialize the same subscriber before parsing configuration.
Outside exactly `APP_ENV=dev`, logs are JSON, dependency targets are **off**, and only
reviewed server/binary targets are enabled. `RUST_LOG` cannot enable SQL, SDK wire,
HTTP client or serde rejection logging. It may lower application detail to warn/error;
operational progress remains enabled. Startup and panic paths never print exception
chains or panic payloads. Configuration failures retain a fixed stage/code only.

HTTP logs contain generated request id, standard method (or `OTHER`), matched route
template (or `unmatched`), status, duration, service and release. Caller-supplied
`x-request-id` is ignored. Successful probes produce no request event. Request and
repetitive detail lines are capped at 20 per static category/site per minute; classified
errors report suppressed counts on the next admitted line. This sampling means log
counts are **not** authoritative HTTP traffic/error rates; alarms use ALB metrics.

Patient, encounter, chunk and job identifiers are omitted from ordinary event logs.
Worker attempts get fresh random delivery correlation, so retries do not form a durable
clinical audit. Bodies, queries, headers, fake tokens, invalid enum values, SQL driver
errors and provider bodies never become log fields. The log schema is an allowlist,
not a general-purpose redactor: new logging sites need the same review.

Worker counters accumulate in memory and flush at most once per minute **after a
successful receive and completion of every returned handler**. Empty completed long
polls count as progress; failed receives, active handlers and lease-renewal timers do
not. No detached heartbeat task exists. A completed failed attempt still shows loop
progress, while job/provider failure counters and DLQ/age alarms show work failure.

Aggregate log records use `metric_name`, `kind` (`transcribe`, `summarize`, `mdm`,
`all`), and numeric `value`. They expose job outcomes, provider calls/failures and
classified error counts, summed provider latency, input/output/cache token counts,
and usage-write failures. Transcription outcomes count dispatch attempts (including
idempotent deliveries); MDM terminal provider refusals count as failures even when
acknowledged. These are operational counters, not unique visits or invoice totals.
Canceled provider calls and process termination can lose in-memory measurements.
Durable attribution remains in `usage_log`.

Only `worker_progress` and `usage_write_failed` become custom CloudWatch metrics,
without dimensions. Other aggregates remain queryable with Logs Insights:

```text
fields metric_name, kind, value
| filter message = "operational metric"
| stats sum(value) as total by bin(5m), kind, metric_name
```

Divide summed `provider_latency_ms` by summed `provider_calls` for mean call latency;
never average averages. Job/provider identifiers are not metric dimensions.

## Database logging floor

The data parameter group disables statement/duration/sample logging and parameter
logging, including `log_parameter_max_length_on_error = 0`. Setting only
`log_statement = none` is insufficient: PostgreSQL errors can themselves echo values.
`log_min_error_statement = panic` suppresses ordinary failing statements and
`log_min_messages = panic` suppresses ordinary error messages too. This deliberately
trades engine error detail for classified application codes and native RDS metrics.
No query-body inspection extension or Bedrock invocation-body logging is enabled.

The local regression check reads the actual Terraform parameter values, starts a
network-isolated PostgreSQL 16 container on tmpfs, verifies effective settings,
exercises literal/bind failures, and checks that engine logs contain neither canary:

```sh
python3 scripts/check-db-log-privacy.py
```

It requires Docker and a cached `postgres:16` image. It destroys only its own disposable
container and prints pass/fail categories. This validates PostgreSQL behavior, not
RDS parameter availability/application; the equivalent RDS check remains mandatory.
PANIC messages and platform failure artifacts still require live destination review.

## Activation and alarms

Configure `telemetry.alarm_emails`, exact `operator_principal_arns`, and a reviewed
`monthly_budget_usd` in foundation. The default $50 notification threshold preserves
the existing contract; it does not approve a larger staging budget. Confirm SNS
subscriptions and a real notification before setting `alarms_enabled = true` for
traffic. Foundation refuses open ingress without this flag; release refuses services
unless the pinned manifest contains the correct alarm topic and enabled flag.
Terraform cannot prove an inbox is monitored or that an email was received.

| Alarm | Threshold/window (60-second periods) | Missing data |
|---|---|---|
| API healthy targets | Minimum < 1 for 3 consecutive periods | Breaching |
| API target 5xx / ALB 5xx (separate alarms) | Sum >= 5 in 3 of 5 periods | Not breaching |
| Worker progress | Sum < 1 for 6 consecutive periods | Breaching |
| Oldest queued message | Maximum > 600 seconds for 3 periods | Not breaching |
| DLQ visible | Maximum > 0 for 1 period | Not breaching |
| RDS free storage | Minimum < 5 GiB for 3 periods | Breaching |
| RDS connections | Maximum >= 35 for 3 periods (configurable; verify live capacity) | Breaching |
| Usage-write failure | Sum > 0 for 1 period | Not breaching |
| Monthly AWS spend | Actual > 80%, forecast > 100% of configured account-wide budget | AWS Budgets, separate from CloudWatch |

Runtime actions default disabled while infrastructure is idle. Budget notifications
are independent and always configured. Before activation, transition a synthetic
alarm OK → ALARM and verify receipt (merely enabling actions on an already-ALARM
metric does not prove delivery). Record only alarm category/time/pass/fail. Expect
roughly six missing periods plus CloudWatch ingestion/evaluation lookback for worker
stall detection; measure the live bound, targeting notification within ten minutes.
The handler's 300-second deadline and 20-second idle poll fit inside this window.

S9 adds one KMS key, nine runtime alarms when edge is present, two custom metric
series, five log groups, audit storage/requests and a budget. Record current regional
prices and measured ingestion before apply; include them in the existing full-stack
budget review. Optional audio data events default off. A starting volume estimate is
`chunks/day × (PUT + HEAD + GET + retries)`: 1,000 chunks/day at three calls each is
about 90,000 data events/month before retries, lifecycle and version operations.
Inspect measured CloudTrail events before enabling this collection.

The trail is account-scoped even though an AWS organization exists: this stack does
not assume organization-wide trail administration. Inventory existing account/org
coverage before applying, adopt a suitable owned trail or resolve duplicate delivery
explicitly. It must never silently replace or disable a central security trail.

## Diagnostics beyond SQS depth

Run through the existing restricted private operator database path. These queries
return aggregate counts only; do not grant these queries to the public API. Use a
bounded statement timeout and read-only transaction. No query repairs/requeues data.

```sql
BEGIN READ ONLY;
SET LOCAL statement_timeout = '5s';
SELECT kind, status, count(*) AS receipts,
       count(*) FILTER (WHERE enqueued_at IS NULL) AS never_confirmed_enqueued
FROM jobs
WHERE (status = 'queued' AND created_at < now() - interval '10 minutes')
   OR (status = 'running' AND updated_at < now() - interval '10 minutes')
GROUP BY kind, status;

SELECT status, count(*) AS old_chunks
FROM audio_chunks
WHERE status IN ('pending', 'uploaded', 'transcribing')
  AND created_at < now() - interval '15 minutes'
GROUP BY status;

WITH encounters AS (
  SELECT org_id, encounter_id,
         max(chunk_index) FILTER (WHERE is_final) AS final_index,
         count(*) AS received,
         bool_and(status IN ('done', 'failed')) AS terminal
  FROM audio_chunks
  WHERE created_at < now() - interval '15 minutes'
  GROUP BY org_id, encounter_id
)
SELECT count(*) AS possibly_incomplete_encounters
FROM encounters
WHERE final_index IS NULL OR received < final_index + 1 OR NOT terminal;
ROLLBACK;
```

`enqueued_at IS NULL` is evidence of an unconfirmed send, not proof no receipt was
sent; a crash between send and stamp can produce the same state. Chunk age is based
on creation because there is no last-progress timestamp. The encounter query is a
screening query over old rows, so a recently arrived final chunk can cause a false
positive. Inspect the complete encounter only in the restricted database session.
Automated outbox/reconciliation/janitor behavior remains Phase 4 Step 9.

## Validation record and remaining live gate

Final local results are recorded below. The original agent run's
candidate was recovered without modifying its retained artifacts. Its MinIO disk
failure was bypassed using a disposable tmpfs MinIO on localhost; upload expectations
now honor `S3_ENDPOINT_URL`. Generated Rust incremental caches were removed to free
build space; existing MinIO objects/volumes were preserved.

Terraform tests use mocked providers and no backend. Terraform 1.10 still enforces
`prevent_destroy` during mock teardown, so the two infrastructure gate scripts first
check both source guards exist, then relax them **only in the temporary mock copy**.
Real source, local tfvars, state and provider locks are not altered by these gates.

| Gate | Evidence/status |
|---|---|
| Rust format, Clippy and workspace suite | PASS: `cargo fmt --all -- --check`; workspace/all-target Clippy with `-D warnings`; `cargo test --jobs 2 --locked --workspace` — 360 passed, 0 failed, 0 ignored, including doc-test targets |
| Telemetry canary + stalled-handler tests | PASS: all 3 telemetry integration tests; focused worker/AI regression suites also pass (12 + 5 tests). Real DB lock prevents progress; error-only cloud filtering preserves delivery correlation and completed-poll metrics |
| Mocked Terraform data/network/manifest gates | PASS: data gate 92 tests; network gate 33 tests; manifest gate 7 cases. Final affected compute/foundation rerun passes 29 tests (overlapping coverage, not additional unique tests). Recursive Terraform formatting and shell syntax checks pass |
| PostgreSQL engine log canary | PASS: effective Terraform settings verified on PostgreSQL 16; literal/bind errors exercised and absent from engine logs. Existing database-secret helper suite also passes all 5 tests |
| Live application/KMS delivery | Pending AWS foundation and candidate deployment |
| SNS subscription and real alarm notification | Pending reviewed destinations and confirmed receipt |
| Worker stall notification bound | Pending deployed worker stall/stop exercise |
| RDS effective parameters and failing-query log inspection | Pending applied parameter group/reboot verification |
| CloudTrail delivery/digest validation and audit read-denial checks | Pending trail inventory/apply |
| WAF samples/logging, ALB logging, Bedrock invocation configuration | Pending live inspection; expected disabled |

Run SQLx-backed suites one invocation at a time against the local test database:
concurrent invocations reuse deterministic test-database names and can drop each
other's fixtures. The recorded full workspace run completed alone, after focused
checks finished. Local checks used no live provider calls or AWS mutations.

Before signing off, inject fabricated markers into bodies, invalid enums, query
strings, arbitrary headers, fake bearer tokens, provider and DB failures. Inspect
all five log groups, RDS engine logs, ECS stopped-task reasons, deployment artifacts,
WAF sampling/log configuration, ALB attributes and CloudTrail/digests through the
restricted operator path. Application destinations must omit canaries. CloudTrail
may contain keys, principals and canaries deliberately placed in AWS request metadata;
that is expected **restricted audit data**, never anonymous telemetry. Keep only
category/pass/fail evidence in source control. If edge logging cannot suppress all
headers/path/query/rule snippets, keep it off; log redaction alone does not protect
WAF samples. Enabling ALB S3 logging later requires a separate restricted SSE-S3
bucket, not a change to audio/state/audit encryption.

Application read/export audit and tamper-resistance policy remain pre-PHI work.
CloudTrail and the clinical event stream do not establish who viewed/exported a note.

Implementation references: [CloudWatch KMS context and caller permissions](https://docs.aws.amazon.com/AmazonCloudWatch/latest/logs/encrypt-log-data-kms.html),
[CloudTrail KMS policy](https://docs.aws.amazon.com/awscloudtrail/latest/userguide/create-kms-key-policy-for-cloudtrail.html),
[SNS budget delivery and confirmation](https://docs.aws.amazon.com/cost-management/latest/userguide/budgets-sns-policy.html),
[PostgreSQL logging parameters](https://www.postgresql.org/docs/16/runtime-config-logging.html),
[WAF log versus sample protection](https://docs.aws.amazon.com/waf/latest/developerguide/waf-data-protection-and-logging.html),
[ALB access-log encryption](https://docs.aws.amazon.com/elasticloadbalancing/latest/application/enable-access-logging.html).
