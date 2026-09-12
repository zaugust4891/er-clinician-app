# Server and worker startup (Phase 4, Step 8 S1)

This step hardens application startup and resource use. It does not add a
migration executable, package images, provision infrastructure, or authorize PHI use.

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
synthetic data only with these defaults. The migration configuration parser is
ready for S2; there is no new migration command in S1.

## Cloud configuration contract

All processes require `APP_ENV=staging|prod`, `AWS_REGION`, a process-specific
`DATABASE_URL` containing login/password/database, and `DB_SSL_ROOT_CERT`, an
absolute path to the reviewed RDS CA bundle. Connections force `verify-full`
(certificate and hostname verification), disable SQL statement logging, and
set application names `er-api`, `er-worker`, or `er-migration`.

| Process | Additional required settings |
|---|---|
| API | `BIND_ADDR`, `AUTH_MODE=cognito`, `COGNITO_REGION`, `COGNITO_POOL_ID`, `COGNITO_CLIENT_ID`, `AUDIO_BUCKET`, `STT_QUEUE` |
| Worker | `AUDIO_BUCKET`, `STT_QUEUE`, `DEEPGRAM_API_KEY`, `TRANSPORT=bedrock`, `SUMMARY_MODEL=sonnet`, `MDM_MODEL=sonnet` |
| Migration config | None; no auth, queue or provider initialization |

Cognito's region must match `AWS_REGION`. Deepgram credentials are rejected in
the API and migration environments. Dev auth/JWKS, direct Anthropic keys, static
AWS credentials, endpoint overrides (including generic/service-specific SDK
variables), AWS profiles, and `PG*` overrides are rejected outside dev. Cloud AWS
clients use the ECS task credential provider only and do not load shared AWS
profile/endpoint configuration. ECS container credential URI/token settings
remain supported. Local SSO/IMDS/web-identity fallback is deliberately unavailable
on this ECS deployment path.

`BEDROCK_SUMMARY_MODEL_ID` and `BEDROCK_MDM_MODEL_ID` accept only the existing
exact, priced model-family IDs. They do not enable arbitrary ARNs or geographic
inference profiles. Account access, actual supported invocation IDs, pricing and
residency must be reviewed before deployment; if a profile is required, extend
the explicit allowlist and its price mapping after that review. Startup does
not probe a live model or claim that access has been verified.

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
