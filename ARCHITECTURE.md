# ER Note architecture

The workspace's dependency direction points toward `er-domain`: pure patient/note
state and event replay. `er-store` defines persistence; `er-app` orchestrates the
local pipeline; `er-ai` and `er-pipeline` supply provider seams; `er-api` owns shared
wire types and the optional remote client; `er-server` implements HTTP, storage,
workers and operator binaries; `er-cli` drives either SQLite or the remote API.
`schema` is an optional documentation build feature on domain/API crates. It
adds JSON Schema derivation without adding schema generation to shipped binaries.

```text
CLI / future device
  | Cognito access JWT, stable event/job/chunk UUIDs
  v
ALB/WAF -> API: JWT -> current membership -> quotas/read audit -> tenant repository
                         |                     |
                         v                     v
                 S3 presigned upload       Postgres events / snapshots / receipts
                         |                     | atomic dispatch outbox
                         +--- complete --------+----> SQS ---> worker
                                                          | tenant routing functions
                                                          | advisory locks + leases
                                                          v
                                                bound S3 version -> STT
                                                          |
                                          de-identify -> Bedrock -> append events
                                                          |
                                                  usage_log / job status
```

All tenant repository queries run within transactions with local tenant context;
RLS and application predicates both enforce org scope. Runtime roles are non-owner
and cannot bypass RLS. Narrow owner functions bootstrap worker routing and recovery.
The worker alone receives STT/provider credentials or Bedrock permissions.

Sync is persist-first and at-least-once. The client retains UUID-stamped events
until every acknowledgment is durable. A timeout can occur after a partial batch
commit: resend the identical batch/IDs. Download `events?since=N&limit=500`, persist
and apply rows in sequence order, advance the cursor only after local commit, and
continue while `has_more`. Sequence IDs are global and can have gaps. Snapshots
accelerate replay and are checksum checked; events remain the recovery authority.

Audio uses intent → direct signed PUT → completion → asynchronous transcription.
Completion atomically fixes the S3 version/ETag and creates dispatch intent. The
outbox recovers sends lost to an API crash; SQS retry/visibility and advisory locks
serialize work. A provider success before persistence can still be billed twice.
Transcripts and generated notes travel through events; status endpoints expose no
clinical text. MDM is a durable asynchronous receipt polled until terminal.

`GET /api-docs/openapi.json` and `/api-docs` publish the committed OpenAPI 3.1
contract and Swagger UI behind the same membership gate. Regenerate the snapshot
with `cargo run -p er-api --example openapi --features schema`; CI checks freshness.
The human-readable contract is [docs/api-contract.md](docs/api-contract.md).

ECS uses one image with separate API, worker, migration, bootstrap and onboarding
entrypoints. Terraform foundation owns durable infrastructure; release owns task
revisions/services through a non-secret manifest. Production remains gated. See
[the runbook](docs/ops/runbook.md) for compatible rollback and isolated restore,
[the load profile](load/README.md) for Step 10 evidence requirements, and
[Phase 5 handoff](docs/planning/phase5-handoff.md) for device obligations.
