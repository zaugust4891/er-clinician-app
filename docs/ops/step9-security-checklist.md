# Step 9 security preparation and evidence

Review date: 2026-11-04. Scope: local synthetic fixtures, source review and mocked
Terraform. This is engineering preparation, not a penetration-test report,
clinical approval or live AWS acceptance. Implementation and exact changed files
are in [the Step 9 record](../planning/phase4-step9-implementation.md).

## Local checks

| Control | Evidence and outcome |
| --- | --- |
| Tenant RLS without application filters | Non-owner runtime tests select events without an org WHERE clause, see only their tenant, and reject foreign inserts. API/worker repository paths, join-table registration, snapshot replay and usage writes pass. Commit/rollback reuse leaves no tenant context. |
| Worker privilege boundary | API cannot execute worker tenant lookups; worker can execute bounded claim/ack/expiry functions but cannot select the outbox. Runtime grants and schema-owner exclusion checks pass. |
| Endpoint authentication/roles | All 18 route/method combinations reject no token, disabled membership and non-physician membership. Existing Cognito cross-org read/write/audio tests pass. Revocation denies the same unexpired token on the running API. |
| Per-clinician quotas | Twenty concurrent MDM admission calls admit exactly five; the next HTTP request returns 429 and Retry-After. Window reset restores admission. |
| Read/export evidence | Read attempt includes route template/resource ID; API cannot read/update/delete/truncate audit rows. Removing the audit destination blocks the clinical read with 503. |
| Audio upload scope and overwrite | Existing signed-key/content-type gates remain. Unversioned overwrite fails the ETag read. Versioned MinIO retains and returns original bytes after overwrite. Recompletion cannot replace the binding. |
| Recovery | Accepted outbox work reaches the queue without a returning client; successful dispatch is acknowledged. Old pending uploads expire while completed uploads remain unchanged. Existing shutdown/lease/redelivery/DLQ tests pass locally. |
| Restore verifier | Empty history is rejected; repeated evidence is deterministic; changed events change the digest. This does not substitute for a live PITR drill. |
| SSRF review | API request DTOs add no URLs. Presigned storage locations and provider/queue endpoints remain server-configured. Existing cloud configuration rejects endpoint overrides. |
| Error/log privacy | Existing response and telemetry canary suites pass. New middleware returns static 429/503 codes; audit/outbox/replay paths do not log request content or database errors. |
| Monitoring/IAM/network | Mocked observability (8), identity (10), staging foundation (12), production foundation (8) tests pass. Network module (13), plus foundation/network wiring tests pass. No live apply. |
| Dependency audit | `scripts/ci-audit.py` passes after supported crypto feature changes and ctutils/Tokio patches. No new exceptions. See remaining findings below. |

Reproducible commands:

```bash
scripts/ci-services.sh
scripts/tf-data-check.sh modules/observability modules/identity envs/staging/foundation envs/prod/foundation
scripts/tf-network-check.sh
python3 scripts/ci-audit.py
```

The application gate creates disposable services and fresh keys, runs format,
Clippy with warnings denied, workspace tests, standalone AI checks, online SQLx
metadata freshness, and negative missing-service/readiness checks. The final
post-dependency-change rerun is recorded below.

Final application gate (2026-11-04): **passed**, exit 0. Format and Clippy passed;
386 test executions passed across the workspace and standalone AI runs, with no
failed or ignored tests. Online SQLx freshness and all three missing-service
negative checks passed. Terraform formatting and `git diff --check` also passed.

One post-dependency full run failed the existing subsecond summary-deadline
fixture; both its isolated reproduction and the full concurrent worker suite
then passed. The fixture now allows two seconds for real DB/S3 setup while
holding the same summary advisory lock, so it tests deadline/terminal behavior
rather than a 400 ms setup assumption. Production timeout settings are unchanged.
Repeated isolated runs also exposed auth fixtures reading a compiled-in path
from a deleted temporary checkout. Both unit-test helpers now resolve keys from
the current Cargo test working directory, matching the integration fixtures.
The restore verifier also passes a reconnect test that proves read-only mode
and write rejection on both the initial and replacement connections.

The audit requires the repository-pinned `cargo-audit 0.22.2`; it was installed to
a temporary tool directory for this review. `RUSTSEC-2023-0071` remains covered by
the existing exception expiring 2026-11-20. The policy also reports unsound
`lru 0.16.4` and yanked `js-sys 0.3.87`, `spin 0.9.8`, and `wasm-bindgen 0.2.110`
as warnings. These are unresolved; no claim of a finding-free graph is made.
The existing RSA exception wording should be reviewed alongside ephemeral backend
certificate creation in `tls.rs`; private-key self-signing also occurs at startup,
while network TLS uses rustls's selected crypto provider.

## Live acceptance — unperformed

- [ ] Apply the reviewed migration, grants, IAM, endpoint and monitoring changes.
- [ ] Confirm RLS and cross-tenant denial with actual non-owner cloud logins.
- [ ] Enable Container Insights and confirm task-count alarms match desired counts.
- [ ] Confirm all SNS destinations and actual delivered alert email.
- [ ] Kill a synthetic worker mid-job; prove retry, poison DLQ routing and email arrival.
- [ ] Perform isolated RDS PITR and compare the frozen patient's event/replay evidence.
- [ ] Record measured RPO/RTO, cleanup, named incident contacts and recovery signoff.
- [ ] Verify live managed-master rotation and staged runtime/Deepgram rotation.
- [ ] Exercise bound-version reads and outbox sends through the real endpoint/KMS policies.
- [ ] Verify quotas/revocation across API replicas with issued Cognito tokens.
- [ ] Review patient assignment/minimum-necessary access, MFA, audit retention/export,
      clinical failure recovery, legacy unbound audio, BAAs and privacy obligations.

Use [the runbook](runbook.md) to execute and record these. There was no cloud
mutation, production activation, email, calendar import, or claimed drill result
in this implementation session. No container image rebuild/security scan was run
as part of this local review; the existing CI image gate remains required.
