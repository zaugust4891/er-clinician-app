# Phase 4 Step 10 implementation and acceptance

Implemented 2026-11-05. Step 10 supplies the load/cost/contract/handoff tooling.
Live acceptance remains open; this record does not mark Phase 4 fully complete.
The original scope is in [the implementation guide](phase4-implementation-guide.md#step-10--load-test-cost-validation-docs-handoff-to-phase-5).

## Implementation

`load/shift.js` models ten distinct clinicians, eighteen patients each and eight
encounters per patient over a compressed hour. It alternates one/two uploads,
interleaves labs/medications/diagnoses/data review, retries identical event batches
and compares acknowledgments, pages event cursors, polls chunks and measures MDM
receipt completion. k6 fails on errors, throttling, missing patients, altered
acknowledgments or failed latency thresholds. Metrics use route names without raw
URLs; signed upload URLs never receive the API Bearer header. Real runs default
to one clinician/three patients. The integration test executes the same JS against
a real local HTTP listener, disposable database/S3/queue and a mock worker.

The default planned polling rate needs 108 cursor reads/minute/clinician before
writes, exceeding Step 9's 60/minute general quota. The full profile correctly
fails if throttled. The implementation preserves the quota and reports the
capacity decision as open. MDM submissions are paced to respect the five/minute
limit. Resolve multiplexed sync or an explicitly reviewed quota change before
claiming the original full load gate. No shortened profile is labelled a full
hour/pilot run.

Mock load stays in isolated `APP_ENV=dev` infrastructure. This deliberately
preserves existing cloud provider gates instead of adding the originally suggested
staging mock flag. Real synthetic staging retains Cognito, versioned S3, Deepgram
and Bedrock. No cloud run or spend was initiated. See [the load guide](../../load/README.md)
for commands, inputs, thresholds and evidence collection.

Successful STT now writes duration and frozen cost receipts to `usage_log`, with
an ID derived from the chunk so redelivery does not duplicate the receipt. The
worker reads an optional reviewed `DEEPGRAM_MICROS_PER_MINUTE` rate (wired from the validated
`deepgram_micros_per_minute` release input into worker task templates); missing,
invalid or zero rates produce explicit `unpriced` model labels and zero cost.
Mock STT is labelled mock/unpriced. Duration is reported in rounded-up seconds;
cost is computed from milliseconds with integer microdollars. Provider success
before durable persistence can still be billed without a receipt; repeat provider
calls can exceed deduplicated receipts. Invoice reconciliation, rather than a
claim of exactly-once billing, closes that gap. Runtime-role tests exercise STT
INSERT/ON CONFLICT under the actual worker grants. PostgreSQL requires SELECT
on the conflict key: grant convergence adds only `SELECT (id)` alongside INSERT;
no cost, token, actor or patient columns become readable by the worker. Run
`er-migrate` to converge this grant before deploying the Step 10 worker.

`docs/ops/cogs.sql` is a read-only org/time-scoped report of provider costs, cache
usage, audio duration and unpriced/mock counts, grouped by clinician/kind/model.
Summary/STT clinician attribution uses patient creator when the receipt lacks an
actor; it does not assert recording ownership in shared care. Preserve unmatched
rows and allocate infrastructure/provider minimums separately. The reconciliation
script compares each provider independently with a maximum 5% absolute relative
error and rejects absent/nonpositive invoice evidence. It does not manufacture
provider exports or substitute console estimates for a settled invoice.

All five Terraform roots already have `Project`/`Env` default tags. Cost allocation
tag names are case-sensitive; the historical lowercase examples are superseded.
The new cost dashboard exposes the account's delayed USD billing metric in
us-east-1 and explains the separate ledger/allocation workflow. Billing metric
collection and cost allocation tag activation still need live verification. No
historical planning cost band is represented as a measured result/current price.

OpenAPI 3.1 schemas derive from the actual serde types via optional `schema`
features in `er-domain` and `er-api`; runtime/default builds do not acquire schema
code. The generator supplies route/method/status/auth/query metadata and rewrites
schema references into components. The committed snapshot includes nested domain
and event/snapshot shapes, not free-form stand-ins. Authenticated `/api-docs` serves
Swagger UI and `/api-docs/openapi.json` serves the snapshot. Assets are exact-version
and SRI pinned; tokens stay in browser memory. CI checks snapshot freshness and
reference resolution; route tests verify authentication/no-store behavior. The
Docker source allowlist includes the embedded snapshot/UI and generator examples.

Architecture, README/CLAUDE status, API contract and Phase 5 handoff now document
existing behavior, device offline/ack/cursor obligations, audio containers,
revocation, quota conflict and missing staging evidence. The handoff uses the
foundation manifest for actual URLs/IDs and the existing onboarding procedure;
no unverified staging endpoint or account was invented.

## Verification

- Isolated application gate including actual k6 smoke: **passed**, exit 0; 388 passing test executions across workspace, opt-in k6 and standalone AI runs. The k6 test is skipped in the ordinary run and then executed explicitly.
- OpenAPI reference/cost reconciliation tests: three passed.
- Mocked observability Terraform tests: nine passed, including cost visibility.
- CI policy: seven tests passed. Dependency audit policy passed with the existing
  RSA exception and warnings for lru unsoundness, pin-utils maintenance, and yanked
  js-sys/spin/wasm-bindgen versions; no new advisory exception was added.
- Terraform formatting, documentation links and `git diff --check`: passed.
- Final gate also passed format, Clippy with warnings denied, schema-feature
  Clippy, generated snapshot freshness, online SQLx freshness and all three
  absent-service readiness checks. The load acknowledgment comparison checks
  event/sequence values rather than incidental JSON object-key order.
- Compute module mocked tests: eleven passed; staging release: ten passed;
  production release: five passed. Both roots validated with the new pricing input.
- No live provider/load/invoice or cloud apply was run; no container image build
  or image scan was performed in this session. Existing image CI remains required.

Commands:

```bash
K6_BIN=/absolute/path/to/k6 scripts/ci-services.sh
PYTHONDONTWRITEBYTECODE=1 python3 scripts/test_step10.py
scripts/tf-data-check.sh modules/observability
python3 scripts/test_ci_policy.py
python3 scripts/ci-audit.py
```

The load integration test is intentionally opt-in (`K6_BIN`); without it Cargo
reports one ignored test. The default CI path still runs all normal HTTP/storage/
worker tests, generated-schema freshness and the Python checks. k6 v2.3.0 was
installed temporarily from the official release with its SHA-256 verified for
this session. No k6 binary, token, recording or private summary is committed.

## Literal Phase 4 acceptance checklist

These are the twelve original criteria from phase4.md §10, with truthful current
status. Checked local evidence does not turn an unchecked live gate into a pass.

| # | Criterion | Status / evidence |
| --- | --- | --- |
| 1 | Workspace tests pass, zero warnings | Passed final local application gate; Clippy uses `-D warnings`. The optional k6 test also passed when explicitly executed. |
| 2 | One Repository, three implementations, shared persistence/recovery suites | Implemented; existing generic contract, SQLite/Postgres/Remote and recovery tests in application gate. |
| 3 | Full remote CLI lifecycle against staging with server AI | Local HTTP lifecycle passes; actual staging lifecycle remains unperformed. |
| 4 | Idempotent sync | Existing dedupe tests plus k6 identical-batch acknowledgment comparison; live load integrity query pending. |
| 5 | Tenant access fails closed | Existing non-owner RLS and cross-org HTTP tests; actual deployment verification pending. |
| 6 | De-identification before third-party calls | Claude input scrub tests exist. STT necessarily receives uploaded audio; no audio PHI redactor exists. Provider/BAA/privacy acceptance remains required. |
| 7 | Live prompt cache reads | Meter fields implemented; real repeat-call `cache_read_tokens > 0` evidence pending. |
| 8 | Metering within 5% of provider invoices | STT/AI ledger, COGS SQL and reconciliation implemented; actual matched provider exports pending. |
| 9 | Clean-clone CI staging deployment | Infrastructure/CI authored; live clean-clone rollout/drift evidence pending. |
| 10 | Worker/DLQ alarm and PITR drill | Local worker failure/recovery tests; live email delivery and isolated restore drill remain pending under the Step 9 runbook. |
| 11 | Published accurate OpenAPI | Generated snapshot and authenticated HTTP publication implemented/tested locally; live staging publication and mobile generator compatibility pending. |
| 12 | Truthful CLAUDE/ARCHITECTURE/phase4 docs | Updated with implementation, deviations, remaining gates and Phase 5 handoff. |

Additional Step 10 targets: full-profile API p95 under 300 ms, real MDM p95 under
30 s, zero DLQ entries, zero idempotency violations and observed monthly run-rate.
All remain pending the live measured runs; full polling also needs the explicit
quota/design resolution. `load/verify.sql` and the load guide define the post-run
job/chunk/outbox/queue checks. Production activation and clinical use remain gated.

## Files updated

<!-- FILES -->

| File | Purpose |
| --- | --- |
| [.dockerignore](../../.dockerignore) | Build inputs, document tracking, local evidence exclusions and CI checks. |
| [.github/workflows/ci.yml](../../.github/workflows/ci.yml) | Build inputs, document tracking, local evidence exclusions and CI checks. |
| [.gitignore](../../.gitignore) | Build inputs, document tracking, local evidence exclusions and CI checks. |
| [ARCHITECTURE.md](../../ARCHITECTURE.md) | Implementation status, architecture, contracts, evidence or Phase 5 handoff. |
| [CLAUDE.md](../../CLAUDE.md) | Implementation status, architecture, contracts, evidence or Phase 5 handoff. |
| [Cargo.lock](../../Cargo.lock) | Optional schema dependency/features and generated-contract example. |
| [Cargo.toml](../../Cargo.toml) | Optional schema dependency/features and generated-contract example. |
| [README.md](../../README.md) | Implementation status, architecture, contracts, evidence or Phase 5 handoff. |
| [crates/er-api/Cargo.toml](../../crates/er-api/Cargo.toml) | Optional schema dependency/features and generated-contract example. |
| [crates/er-api/examples/openapi.rs](../../crates/er-api/examples/openapi.rs) | Generated contract, generator or authenticated publication regression test. |
| [crates/er-api/src/types.rs](../../crates/er-api/src/types.rs) | Optional schemas for all wire DTOs. |
| [crates/er-domain/Cargo.toml](../../crates/er-domain/Cargo.toml) | Optional schema dependency/features and generated-contract example. |
| [crates/er-domain/src/audit.rs](../../crates/er-domain/src/audit.rs) | Optional schema derivation matching existing serde serialization. |
| [crates/er-domain/src/diagnosis.rs](../../crates/er-domain/src/diagnosis.rs) | Optional schema derivation matching existing serde serialization. |
| [crates/er-domain/src/disposition.rs](../../crates/er-domain/src/disposition.rs) | Optional schema derivation matching existing serde serialization. |
| [crates/er-domain/src/encounter.rs](../../crates/er-domain/src/encounter.rs) | Optional schema derivation matching existing serde serialization. |
| [crates/er-domain/src/events.rs](../../crates/er-domain/src/events.rs) | Optional schema derivation matching existing serde serialization. |
| [crates/er-domain/src/medication.rs](../../crates/er-domain/src/medication.rs) | Optional schema derivation matching existing serde serialization. |
| [crates/er-domain/src/note.rs](../../crates/er-domain/src/note.rs) | Optional schema derivation matching existing serde serialization. |
| [crates/er-domain/src/patient.rs](../../crates/er-domain/src/patient.rs) | Optional schema derivation matching existing serde serialization. |
| [crates/er-domain/src/result.rs](../../crates/er-domain/src/result.rs) | Optional schema derivation matching existing serde serialization. |
| [crates/er-domain/src/shift.rs](../../crates/er-domain/src/shift.rs) | Optional schema derivation matching existing serde serialization. |
| [crates/er-server/src/api_docs.rs](../../crates/er-server/src/api_docs.rs) | Authenticated documentation routing and embedded snapshot/UI. |
| [crates/er-server/src/lib.rs](../../crates/er-server/src/lib.rs) | Authenticated documentation routing and embedded snapshot/UI. |
| [crates/er-server/src/privileges.rs](../../crates/er-server/src/privileges.rs) | Grant conflict-key ID reads only; test receipt insertion/retries under worker privileges. |
| [crates/er-server/src/repository.rs](../../crates/er-server/src/repository.rs) | Successful STT duration/cost receipts and retry deduplication. |
| [crates/er-server/src/routes.rs](../../crates/er-server/src/routes.rs) | Authenticated documentation routing and embedded snapshot/UI. |
| [crates/er-server/src/worker.rs](../../crates/er-server/src/worker.rs) | Successful STT duration/cost receipts and retry deduplication. |
| [crates/er-server/tests/load_gate.rs](../../crates/er-server/tests/load_gate.rs) | Actual k6 smoke, usage-ledger assertions and failure diagnostics. |
| [crates/er-server/tests/openapi_gate.rs](../../crates/er-server/tests/openapi_gate.rs) | Generated contract, generator or authenticated publication regression test. |
| [crates/er-server/tests/worker_gate.rs](../../crates/er-server/tests/worker_gate.rs) | Actual k6 smoke, usage-ledger assertions and failure diagnostics. |
| [docs/api-contract.md](../api-contract.md) | Implementation status, architecture, contracts, evidence or Phase 5 handoff. |
| [docs/api/openapi.json](../api/openapi.json) | Generated contract, generator or authenticated publication regression test. |
| [docs/api/swagger-assets.json](../api/swagger-assets.json) | Pinned Swagger UI and asset provenance. |
| [docs/api/swagger.html](../api/swagger.html) | Pinned Swagger UI and asset provenance. |
| [docs/ops/cogs.sql](../ops/cogs.sql) | Restricted clinician/provider COGS and metering quality report. |
| [docs/planning/phase4-implementation-guide.md](phase4-implementation-guide.md) | Implementation status, architecture, contracts, evidence or Phase 5 handoff. |
| [docs/planning/phase4-step10-implementation.md](phase4-step10-implementation.md) | Implementation status, architecture, contracts, evidence or Phase 5 handoff. |
| [docs/planning/phase4.md](phase4.md) | Implementation status, architecture, contracts, evidence or Phase 5 handoff. |
| [docs/planning/phase5-handoff.md](phase5-handoff.md) | Implementation status, architecture, contracts, evidence or Phase 5 handoff. |
| [infra/envs/prod/release/compute.tf](../../infra/envs/prod/release/compute.tf) | Validated reviewed STT rate from release input to worker template, with mocked tests. |
| [infra/envs/prod/release/variables.tf](../../infra/envs/prod/release/variables.tf) | Validated reviewed STT rate from release input to worker template, with mocked tests. |
| [infra/envs/staging/release/compute.tf](../../infra/envs/staging/release/compute.tf) | Validated reviewed STT rate from release input to worker template, with mocked tests. |
| [infra/envs/staging/release/variables.tf](../../infra/envs/staging/release/variables.tf) | Validated reviewed STT rate from release input to worker template, with mocked tests. |
| [infra/modules/compute/main.tf](../../infra/modules/compute/main.tf) | Validated reviewed STT rate from release input to worker template, with mocked tests. |
| [infra/modules/compute/tests/compute.tftest.hcl](../../infra/modules/compute/tests/compute.tftest.hcl) | Validated reviewed STT rate from release input to worker template, with mocked tests. |
| [infra/modules/compute/variables.tf](../../infra/modules/compute/variables.tf) | Validated reviewed STT rate from release input to worker template, with mocked tests. |
| [infra/modules/observability/cost.tf](../../infra/modules/observability/cost.tf) | Account cost dashboard and mocked billing visibility test. |
| [infra/modules/observability/tests/observability.tftest.hcl](../../infra/modules/observability/tests/observability.tftest.hcl) | Account cost dashboard and mocked billing visibility test. |
| [load/README.md](../../load/README.md) | Synthetic workload, run instructions or post-run integrity queries. |
| [load/shift.js](../../load/shift.js) | Synthetic workload, run instructions or post-run integrity queries. |
| [load/verify.sql](../../load/verify.sql) | Synthetic workload, run instructions or post-run integrity queries. |
| [scripts/ci-services.sh](../../scripts/ci-services.sh) | Load/schema/cost validation and isolated CI source/freshness support. |
| [scripts/reconcile-costs.py](../../scripts/reconcile-costs.py) | Load/schema/cost validation and isolated CI source/freshness support. |
| [scripts/test_step10.py](../../scripts/test_step10.py) | Load/schema/cost validation and isolated CI source/freshness support. |
