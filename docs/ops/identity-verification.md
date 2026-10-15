# Step 8 S6: identities, database bootstrap, and credential rotation

Implementation dated 2026-10-15. Code and offline checks are implemented. **No S6
resources have been applied and no AWS permission, RDS, or ECS rotation probe has
been performed.** The application foundation still needs the budget decision and
S7–S9 dependencies recorded in the [deployment runbook](staging-deploy.md).

## Permission boundaries

`infra/modules/identity` is called by both foundation roots; disabled production
creates no identities. All task/execution trusts require ECS tasks in the exact
account and Region. Each process has its own roles. The bootstrap has an execution
role and no task role: its only external operation is its private PostgreSQL session.

| Identity | Allowed work |
|---|---|
| API task | Audio PutObject/GetObject, ListBucket on that bucket, SendMessage on the main queue, GetQueueUrl on both queues; corresponding S3/SQS KMS use |
| Worker task | Audio GetObject, main-queue receive/delete/change-visibility, lookup of both queues; non-streaming InvokeModel on the approved US Sonnet profile and its three destination models |
| Migration task | No AWS application calls |
| Onboarding task | AdminGetUser on S7's exact pool once supplied; no Cognito authority before then |
| API execution | One ECR repository, its own log group, API DB secret |
| Worker execution | Same image repository, its own logs, worker DB and Deepgram secrets |
| Migration execution | Image, its own logs, owner DB secret |
| Onboarding execution | Image, its own logs, onboarding DB secret |
| DB bootstrap execution | Image, its own logs, four individual DB login secrets and the RDS-managed master secret |

HeadObject is authorized by GetObject. API ListBucket preserves the missing-upload
404 contract; it exposes key metadata to the API role but adds no listing endpoint.
Bucket-wide signing remains privileged; authenticated repository checks and
server-derived keys enforce tenancy.

KMS grants constrain each SDK operation by service and encryption context. S3
accepts both bucket and object ARN contexts for Bucket Key compatibility; the API
needs Decrypt as well as GenerateDataKey. SQS producers need both operations and
consumers need Decrypt. Secret decryption is restricted to exact secret ARNs.
The data key's separate `aws_kms_key_policy` preserves account IAM delegation and
adds the exact-role service delegations without a key/role dependency cycle.
Only that resource owns the policy; do not add an inline key policy as a second owner.
See [SQS key management](https://docs.aws.amazon.com/AWSSimpleQueueService/latest/SQSDeveloperGuide/sqs-key-management.html).

Terraform creates five encrypted secret containers and no secret versions. Their
resource policies deny GetSecretValue outside their execution roles, including
operator/deployment identities. Operators populate values with write-only authority;
changing a read policy is a separate privileged infrastructure change.
Deepgram is environment/project-specific JSON `{"api_key":"..."}`, entered through
the approved Secrets Manager console or secure bootstrap path. There is no Anthropic
secret in cloud. No service receives the RDS master secret.

The release manifest contains API/worker/migration role ARNs and four secret pointers
(`api_db`, `worker_db`, `migration_db`, `deepgram`). Master/bootstrap/onboarding
references stay in the foundation's operator outputs. Release roots reject missing
S6 role or secret keys.

## Database contract

Every executable shares `DB_HOST`, `DB_PORT`, `DB_NAME`, `DB_USERNAME`, `DB_PASSWORD`
and `DB_SSL_ROOT_CERT`; cloud connections use verified TLS. `DATABASE_URL` remains
dev-only. The bootstrap additionally receives `OWNER_`, `API_`, `WORKER_`, and
`ONBOARDING_DB_USERNAME`/`DB_PASSWORD` pairs, for example `OWNER_DB_PASSWORD`.
Only `er-db-bootstrap` bypasses the runtime check that rejects the master login.
All connections put `pg_catalog` before `public` in the search path.

`er-db-bootstrap` validates four distinct lowercase login names and generated
32–128-character printable ASCII passwords. In one transaction it creates or
updates the four logins, ensures the three NOLOGIN group roles, grants only the
intended memberships, revokes PUBLIC database privileges/public-schema CREATE,
and grants schema CREATE only to the owner. It rejects elevated attributes,
unexpected or administrative memberships, runtime ownership, CREATE privileges,
and direct runtime ACLs before committing. Passwords reach PostgreSQL as salted
SCRAM-SHA-256 verifiers; SQL statements, password values and raw database errors
are not logged by the tool. It then proves each credential by reconnecting.

The bootstrap has a 120-second deadline. A timeout/failure after commit can leave
the staged credentials applied. **Keep that rotation record and its staged secret
versions; retry the same pinned task.** A failed run is not evidence of rollback.

`er-migrate` runs next as the schema owner, applies SQLx migrations, then converges
the explicit relation/column allowlist in `src/privileges.rs`. It removes stale
table, column, sequence and routine ACLs plus global/per-schema default privileges
for the actual owner. Unclassified future objects receive no runtime grant.

| Login/group | Database authority |
|---|---|
| `er_schema_owner` | Non-superuser schema owner; migrations and privilege convergence |
| `er_api_login` / `er_api_runtime` | API queries, append-only events, snapshot upserts, patient/shift/audio/job receipts; no membership mutation or usage ledger reads |
| `er_worker_login` / `er_worker_runtime` | Replay/append events, chunk/job state transitions and usage INSERT; no patient/clinician administration or snapshot writes |
| `er_onboarding_login` / `er_identity_onboarding` | One-shot SELECT/INSERT and selected UPDATE columns on orgs/clinicians only; no clinical history, DDL or DELETE |

Runtime logins own no schema objects and have no BYPASSRLS. Table permissions do
not implement tenant RLS; that remains Step 9's separate requirement. S7 supplies
the actual approved subject-to-clinician onboarding procedure using its own login.

## First bootstrap and maintenance rotation

Use a protected SSO operator session with the expected account/Region. Keep shell
tracing disabled. The operator needs STS identity, DescribeSecret/PutSecretValue/
UpdateSecretVersionStage on these four login containers, corresponding exact-secret
KMS encryption/decryption, and ECS registration/run/describe authority for the
privileged bootstrap only. This is separate from release CI. Never run the tool
from an untrusted workflow or give it production credentials for a staging operation.

After the approved foundation, registry, log groups and candidate image exist:

1. Save non-secret interfaces with `terraform -chdir=infra/envs/staging/foundation
   output -json identity > /tmp/ernote-identity.json`. Use durable, access-controlled
   operator storage for the rotation record; the `/tmp` path below is an example.
2. Stage four generated credentials. The script sends JSON through AWS CLI stdin,
   never plaintext arguments/files, and never calls GetSecretValue. It saves version
   IDs before writing, so retrying the same record recovers a lost response.

   ```bash
   python3 scripts/db-secrets.py stage \
     --identity-file /tmp/ernote-identity.json --record /tmp/ernote-rotation.json \
     --account-id "$EXPECTED_ACCOUNT_ID" --region us-east-2 --profile "$OPERATOR_PROFILE"
   ```

3. For rotation, close ingress and stop API/worker/onboarding tasks before applying
   passwords. Wait for bounded worker drain and task termination; preserve queued
   work. First deployment already has workloads disabled. This implementation uses
   scheduled maintenance, with one operator/rotation at a time.
4. Start from S8's reviewed bootstrap task-definition template. It must use one
   container named `db-bootstrap`, entryPoint `/usr/local/bin/er-db-bootstrap`, the
   exact bootstrap execution role, no task role, Fargate/awsvpc, non-secret DB/TLS
   environment, the candidate image digest and its dedicated log group. Render the
   exact version references:

   ```bash
   python3 scripts/db-secrets.py render \
     --identity-file /tmp/ernote-identity.json --record /tmp/ernote-rotation.json \
     --account-id "$EXPECTED_ACCOUNT_ID" --region us-east-2 \
     --task-definition-template /tmp/ernote-bootstrap-template.json \
     > /tmp/ernote-bootstrap-pinned.json
   ```

   The module's AWSPENDING references describe the source stages; the rendered
   task pins each managed login's version ID. ECS does not allow selecting both
   version ID and stage. Consequently the bootstrap execution role permits reads
   of those four secrets by ARN, rather than an incompatible VersionStage condition.
5. Register/run that pinned definition privately with `assignPublicIp=DISABLED`,
   S4 private subnets and the onboarding SG. The role holds master authority: use
   only the reviewed digest/definition. Do not supply command/environment/role
   overrides. Wait for STOPPED and inspect the single bootstrap container's exit
   code. RunTask submission alone is not completion.
6. Promote only after successful bootstrap. The helper independently checks task
   exit 0, digest, execution role, entrypoint and exact rotation version references;
   an old bootstrap or an altered definition fails before any label changes.

   ```bash
   python3 scripts/db-secrets.py promote \
     --identity-file /tmp/ernote-identity.json --record /tmp/ernote-rotation.json \
     --account-id "$EXPECTED_ACCOUNT_ID" --region us-east-2 --profile "$OPERATOR_PROFILE" \
     --cluster "$CLUSTER_ARN" --task "$BOOTSTRAP_TASK_ARN" --digest "$IMAGE_DIGEST"
   ```

7. Run `er-migrate` with the migration execution/task roles and the owner secret at
   AWSCURRENT. Require exit 0 and completed grants, then start replacement API/worker
   tasks and verify authenticated synthetic writes/reads and worker progress. Running
   tasks do not pick up new injected secrets; force replacement.
8. Verify the previous passwords cannot open fresh DB connections, that all old
   tasks/connections are gone, and that the current tasks use the accepted digest.
   Retain the non-secret rotation record and restricted AWS evidence. Secret stage
   promotion is not atomic across containers: if interrupted, keep workloads stopped
   and retry `promote` with the same record. Never generate another set to hide a failure.

The first role bootstrap precedes the first migration. Bootstrap credential changes
and migrations are separate transactions; failure after bootstrap is recovered with
the same staged credentials. Restoring an old application image does not restore an
old password. AWSPREVIOUS is retained for controlled recovery, not automatic rollback;
retire it through the approved retention procedure after acceptance. Alternating-user
rotation without maintenance remains Step 9 work.
See [ECS secret injection and task replacement](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/secrets-envvar-secrets-manager.html).

## CI templates and S11 handoff

`ci = null` creates no GitHub trust. Optional staging templates create separate
infrastructure, release and smoke roles trusting one exact repository and protected
GitHub environments `staging-infrastructure`, `staging`, `staging-smoke`, with
audience `sts.amazonaws.com`. PR/branch subjects and other environments do not match.
All three deny role chaining, clinical bucket access and DB/provider secret reads.

Infrastructure receives only its S3 bootstrap state policy. S11 must review and attach
a provisioning policy for the final S7–S9 resource set before infrastructure CI can
apply it; SSO owns initial provisioning. Release receives only release state plus
one-repository ECR push, scoped ECS services/migration tasks, and PassRole for the
six API/worker/migration roles. Bootstrap/onboarding roles are excluded. Registry
authentication and ECS register/describe-task-definition APIs need Resource `*`;
task/service operations and PassRole remain scoped. Smoke can retrieve only S7's
separate synthetic credential through its exact KMS context. These templates expect
cluster `<project>-staging`, services/families `<project>-staging-{api,worker,migration}`;
S8/S11 must keep those names aligned and complete live deployment-policy checks.

GitHub environment branch restrictions and approvals must be installed before
enabling trust: an environment subject alone carries no branch restriction. Protect
deployment code, since arbitrary images plus permitted PassRole can exercise injected
credentials. Production CI remains inactive; S11 supplies its separate protected
promotion path without staging's synthetic-password exception.

## Verification and live acceptance still required

Offline gates are `cargo fmt --all -- --check`, workspace Clippy with warnings denied,
workspace tests against local compose services, `scripts/tf-data-check.sh` (mocked
data, identity and both foundations), `scripts/tf-network-check.sh`, and
`python3 -m unittest scripts/test_db_secrets.py`. They test actual repository queries
under runtime permissions, schema-owner migration, onboarding boundaries, stale ACL
cleanup, failed-bootstrap rollback, old-password rejection and safe secret promotion.
They do not prove RDS's managed-master behavior or AWS authorization.

On 2026-10-15 the workspace passed **314 Rust tests** with no ignored tests, format
and Clippy. Mocked data/identity/foundation checks passed **33 runs**, network checks
**25 runs** (foundation cases overlap), manifest guard cases passed, and all five
roots validated in clean backend-disabled copies with locked AWS provider 6.64.0.
Python unittest passed **95 tests**: five credential-tool regressions and 90 existing
agent-runner regressions.

Run these disposable-task probes only after S8/S9 and the approved foundation apply:

| Probe | Required result | Status |
|---|---|---|
| API encrypted presigned PUT, missing-key HEAD, SQS send and both queue lookups | Positive operations succeed; missing HEAD maps to chunk_not_uploaded | Unperformed |
| Worker encrypted audio GET, SQS receive/visibility/delete, exact-profile Bedrock invocation | Succeed with the actual worker task role | Unperformed |
| API Bedrock/Deepgram read, worker audio PUT, runtime DDL/history DELETE | Access denied without policy expansion | Unperformed in AWS/RDS |
| Image pull, each log stream, each execution role's secret injection | Only own secrets readable; master only in bootstrap | Unperformed |
| Release DB/provider secret reads, bootstrap PassRole, production role assumption | Access denied; untrusted GitHub subjects cannot assume roles | Unperformed |
| Maintenance rotation and replacement-task reconnect | New passwords work, old passwords fail, no values in logs/artifacts | Unperformed in ECS/RDS |

Record only assertion names/status, resource/revision IDs, dates and links to
restricted evidence. Never include passwords, tokens, clinical rows, generated notes
or presigned URLs. Local logs are development evidence, not a claim that S6's live
acceptance gate passed.
