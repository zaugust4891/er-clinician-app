# Step 8 verification and handoff

Status **2026-10-31: implementation/local checks available; live acceptance unperformed**.
Use [the final setup checklist](step8-final-aws-live-checklist.md) before this record.
Do not mark Step 8's cloud gate complete until every essential row has dated evidence.

## Release identity (fill after a live run)

| Field | Recorded value |
|---|---|
| Verifier / time / restricted evidence location | Pending |
| AWS account / Region / GitHub workflow run | Pending |
| Source commit / image digest / architecture | Pending |
| Foundation manifest S3 version | Pending |
| Migration task ARN / definition / explicit exit code | Pending |
| API and worker task definitions / observed digests | Pending |
| Accepted release record S3 version / prior accepted record | Pending |
| Alarm and rollback owner | Pending |

The workflow records these non-secret deployment references in the versioned artifact
bucket. Its console output includes fixed gate labels and synthetic UUIDs only.
Do not attach tokens, presigned URLs, recordings, generated prose, raw task responses,
database dumps or raw Terraform plans to this document.

## Essential live evidence

| Gate | How to establish it | Status |
|---|---|---|
| Foundation + release convergence | Clean checkout at accepted source; reviewed backend/config; foundation and release `plan -detailed-exitcode` both exit 0, exact accepted digest/manifest inputs | Unperformed |
| Auth and tenant isolation | Automated smoke: invalid token, ID token, unprovisioned subject rejected; A/B provisioned; foreign events/snapshot/note/export/shift/audio/job return 404. Complete expired, wrong issuer/client, disabled-user and cache/revocation cases in S7 procedure | Unperformed |
| Provider lifecycle | Automated smoke: AudioStarted → intent → HTTPS PUT → complete → AudioStopped → transcript/summary → vital → MDM → replay → review/sign → snapshot/export → shift end; expected fixture facts and nonempty MDM | Unperformed |
| Bounded observation | Five-minute summary and MDM budgets; 15-minute whole smoke budget. On timeout inspect printed UUIDs and existing jobs; separately prove real CLI reload after its 30-second observation window | Unperformed |
| Idempotency | Smoke retries exact event/chunk/job IDs and counts derived events once. Verify database/provider accounting separately; persistence idempotency does not guarantee no repeat provider charges | Unperformed |
| Worker replacement | During a selected synthetic job, operator stops its worker task in ECS; allow service replacement, replay from fresh client, inspect eventual result or explicit failure/DLQ, retain accepted-event counts | Unperformed |
| Real usage and cache | Restricted diagnostics confirm task-role Bedrock Sonnet and Deepgram invocation, nonzero attributed summary/MDM metering. Repeat an eligible synthetic MDM request to inspect cache counters; record misses honestly | Unperformed |
| Private connectivity / TLS | S4 probes from both private subnets; verify RDS hostname/CA and negative CA/hostname fixtures, no public task/DB access | Unperformed |
| Least privilege | S6 execution/runtime/CI negative probes, correct KMS contexts, foreign state/credential denial; inspect actual role sessions rather than testing as administrator | Unperformed |
| Presigned upload boundary | Normal external PUT succeeds; changed key/type, anonymous reads and natural expiry fail. Inspect encryption independently of signature failures | Unperformed |
| Retention and recovery settings | S3 versions/lifecycle, queue/DLQ policy, RDS backups and advancing LatestRestorableTime. PITR rehearsal remains Step 9 | Unperformed |
| Logging and alarms | S9 canary search through every configured sink; no sensitive payloads; real alert delivery/acknowledgement; safe RDS logging parameters | Unperformed |
| Failed migration | Deliberate synthetic checksum/start/exit failure before rollout; record old service definitions/counts unchanged. Restore schema fixture through approved procedure | Unperformed |
| Failed rollout + rollback | Two accepted releases first; deliberate bad candidate fails; prior source/config/digest restored through release root; rollback smoke succeeds; workflow still reports failed candidate | Unperformed |
| First release / interrupted run | Failure stops services when no accepted predecessor exists. Recover an interrupted attempt after inspecting migration state; no duplicate active migration | Unperformed |
| Cost / capacity sanity | Record task counts, memory, DB connections, lifecycle/queue timings, dated run-rate estimate and actual counters; note billing lag | Unperformed |
| Production disabled | Dispatch promotion contract: expected failure, no AWS credentials/workloads. Confirm production roots still disabled; actual future approval/trust gate remains to be verified | Unperformed |

## Local evidence from this implementation

See the dated S11/S12 [implementation record](../planning/phase4-step8-implementation-steps.md#implementation-record--s11s12-2026-10-31)
for exact commands/results and files: 14 deployment tests, 7 existing policy tests,
6 API tests and 48 targeted Terraform mock runs passed; Clippy, workflow lint and
format/syntax checks also passed. These checks cover release failure behavior,
IAM/manifest constraints, workflow syntax, Rust compilation/Clippy and DTO tests.
They are not AWS acceptance or evidence of real-provider output quality.

## Follow-up ownership and closure

| Owner | Remaining condition |
|---|---|
| AWS/repository owner | Final checklist, separate foundation provisioning-policy approval, environment/branch protections, budget/domain, secret/fixture onboarding, live evidence |
| Step 9 engineering/security | Tenant RLS and read/export audit, revocation/clinical roles, upload checksum/version binding, outbox/stuck-job recovery, rotation/recovery drills and pre-PHI review |
| Step 10 engineering | Load/performance benchmarks, cost dashboard, STT cost ledger/invoice reconciliation, OpenAPI and Phase 5 handoff |

Known deployment limits: one NAT, single-AZ RDS, fixed two API/one worker tasks;
injected secrets require task replacement; completed presigned URLs can still
overwrite audio until expiry (Step 9 binding gate). Operator-driven destructive
drills and provider-cache inspection remain manual; the release smoke cannot prove them.
