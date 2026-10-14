# Server, worker, and migration startup (Phase 4, Step 8 S1–S2)

S1 hardens application startup and resource use. S2 adds a one-shot migration
binary and a pinned source-only production image. Infrastructure provisioning
and authorization for PHI use are separate work.

## Local development

Start the existing Postgres, MinIO and ElasticMQ compose services and configure
the values in the repository's `.env.example`. Select development **in the shell**:

```bash
APP_ENV=dev cargo run -p er-server --bin er-server
APP_ENV=dev cargo run -p er-server --bin er-worker
```

Only explicit `APP_ENV=dev` loads `.env`. Missing/invalid `APP_ENV` exits before
network access. Dev permits local plaintext Postgres, paired S3/SQS emulator
endpoints, dev JWKS, direct AI, and mock providers when keys are absent. Use
synthetic data only with these defaults. The migration process parses only
its database settings and does not initialize auth, queues, or providers.

## Schema migration and production image

Run the migration with its own database login before updating API or worker
tasks. The first cloud database setup creates the owner/login roles separately;
`er-migrate` only applies the checked-in SQLx schema migrations. API and worker
startup do not apply migrations.

```bash
APP_ENV=dev cargo run --locked -p er-server --bin er-migrate
docker build --platform linux/amd64 -t er-note-app:local .
```

The image contains `/usr/local/bin/er-server`, `er-worker`, and `er-migrate`.
Its default entrypoint is the API. ECS worker and one-shot migration task
definitions must set `entryPoint` to the corresponding absolute binary path;
changing only `command` passes arguments to the API instead.

SQLx's embedded migrator keeps PostgreSQL advisory locking and validates
applied checksums on every run. The migration connection sets a 15-second lock
wait limit and a 300-second per-statement limit; the full run has a 600-second
limit. It exits nonzero for failures and prints only migration version and a
fixed status. A no-op rerun succeeds. Migration/role changes must be planned
for compatibility with the previous application image; a deploy rollback does
not reverse schema migrations.

The pinned image manifests are `linux/amd64` and use the Rust version in
`rust-toolchain.toml`. The build uses `--locked` and `SQLX_OFFLINE=true` in
both cargo-chef and final compilation. The runtime has CA certificates and the
[AWS RDS global bundle](https://docs.aws.amazon.com/AmazonRDS/latest/UserGuide/UsingWithRDS.SSL.html)
at `/etc/ssl/certs/rds-global-bundle.pem` (SHA-256
`e5bb2084ccf45087bda1c9bffdea0eb15ee67f0b91646106e466714f9de3c7e3`).
Set `DB_SSL_ROOT_CERT` to that path in cloud tasks and review the bundle at
rotation time.

The Docker context allows only manifests, source, prompts, migrations, SQLx
metadata, the toolchain file, and this public CA bundle. No development keys,
`.env`, local database, shift state, test data, or Terraform output enters any
build layer. The runtime database/shift paths are untracked and ignored; the owner
confirmed synthetic-only history on 2026-10-10. Development signing files remain
tracked authentication-test fixtures and are excluded from the image context.
See [artifact provenance](../../docs/ops/repository-artifact-provenance.md).

## Cloud configuration contract

All processes require `APP_ENV=staging|prod`, `AWS_REGION`, a process-specific
`DATABASE_URL` containing login/password/database, and `DB_SSL_ROOT_CERT`, an
absolute path to the reviewed RDS CA bundle. Connections force `verify-full`
(certificate and hostname verification), disable SQL statement logging, and
set application names `er-api`, `er-worker`, or `er-migration`.

| Process | Additional required settings |
|---|---|
| API | `BIND_ADDR`, `AUTH_MODE=cognito`, `COGNITO_REGION`, `COGNITO_POOL_ID`, `COGNITO_CLIENT_ID`, `AUDIO_BUCKET`, `STT_QUEUE` |
| Worker | `AUDIO_BUCKET`, `STT_QUEUE`, `DEEPGRAM_API_KEY`, `TRANSPORT=bedrock`, `SUMMARY_MODEL=sonnet`, `MDM_MODEL=sonnet`, `BEDROCK_SUMMARY_MODEL_ID`, `BEDROCK_MDM_MODEL_ID` |
| Migration config | None; no auth, queue or provider initialization |

Cognito's region must match `AWS_REGION`. Deepgram credentials are rejected in
the API and migration environments. Dev auth/JWKS, direct Anthropic keys, static
AWS credentials, endpoint overrides (including generic/service-specific SDK
variables), AWS profiles, and `PG*` overrides are rejected outside dev. Cloud AWS
clients use the ECS task credential provider only and do not load shared AWS
profile/endpoint configuration. ECS container credential URI/token settings
remain supported. Local SSO/IMDS/web-identity fallback is deliberately unavailable
on this ECS deployment path.

`BEDROCK_SUMMARY_MODEL_ID` and `BEDROCK_MDM_MODEL_ID` are required outside dev,
so the worker never falls back to a default Bedrock route. For Sonnet they accept
exactly two IDs: the in-Region model ID `anthropic.claude-sonnet-4-6` and the US
geographic profile `us.anthropic.claude-sonnet-4-6`. Staging uses the US profile
because Sonnet 4.6 has no in-Region inference in us-east-2; a request through it
may be processed in us-east-1, us-east-2 or us-west-2. The global profile, other
geographies and ARNs are rejected. Adding one means extending
`ModelId::set_bedrock_id` and `price_for` together, after a residency, account-access
and pricing review. Both accepted IDs are metered at Bedrock's regional rate, 10%
above first-party pricing. Startup does not probe a live model or claim that access
has been verified.

## Bounds and retry behavior

| Limit | Default / policy |
|---|---|
| `DB_MAX_CONNECTIONS` | 5 (migration 1); supported 1–32, worker minimum 2 |
| `DB_ACQUIRE_TIMEOUT_SECS` | 5; range 1–30; also bounds physical connection creation |
| `DB_CONNECT_TIMEOUT_SECS` | 10; range 1–30; bounds initial pool connection |
| `MAX_AUDIO_BYTES` | 16 MiB; configurable 1 byte–64 MiB |
| Audio download | 20 seconds, including headers and streaming body |
| AWS operation | 35 seconds total, at most 3 SDK attempts |
| AI generation / Bedrock operation | 45 seconds total per call, including retries |
| Worker job | 270 seconds total; dispatch gets 260, reserving 10 for terminal bookkeeping |
| Queue visibility | 300 seconds, renewed every 60; renewal must finish within 5 seconds |
| Shutdown | No new receives; at most 100 seconds to drain, then drop unacknowledged work |

Worker time/lease values are pinned together in `WorkerLimits`; only audio size
is an environment override. Change the time budgets together and rerun timed
tests. STT's existing worst-case retry budget is approximately 181.5 seconds;
adding 20 seconds download and 45 seconds AI leaves limited DB/lock overhead.
Validate these bounds with synthetic staging workloads before rollout. Provision
enough DB connections for all replicas and deployment overlap, not just one pool.

Audio completion verifies size and exact supported content type before enqueue.
The worker verifies them again and counts bytes even without Content-Length.
Audio stays in memory. These checks are **not an S3 pre-upload storage quota**
and do not validate a file's codec from its bytes.

Lease failure cancels the handler. Shutdown keeps renewing during the bounded
drain, then releases in-flight transactions without deleting unfinished messages.
Redelivery uses saved transcript/MDM state and existing idempotency rules. Final
job deadlines follow terminal failure/DLQ handling; a completed transcript is
never overwritten as failed and deadline bookkeeping does not start another
summary request. If DB bookkeeping itself is unavailable, the message remains
unacknowledged; DLQ monitoring and operator recovery remain necessary. Cancellation
cannot prove that a remote provider stopped processing or did not bill a request.

## Validation and remaining deployment work

`config_gate`, `audio_limits`, `audio_complete_gate`, `worker_gate`, `ai_gate`, and
server/AI unit tests cover unsafe configuration, byte/time bounds, lease loss,
real emulator lease renewal, shutdown redelivery, saved-state repair and final
deadline/DLQ behavior. Run integration tests against the local compose services
with explicit dev variables and synthetic credentials; never source production
provider secrets for these tests.

Live RDS trust-chain/hostname-negative tests, task-role permissions (including
`sqs:ChangeMessageVisibility`), Bedrock account/model access, packaged CA trust,
and ECS stop timeout belong to deployment validation. No real-data rollout is
approved by these tests. S0's BAA, retention/residency, consent, and organizational
privacy/security determinations still require the designated expert reviewers.
