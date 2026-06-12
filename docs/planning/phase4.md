# Phase 4 — Cloud Backend & AWS Infrastructure: Implementation Plan

*2026-06-11 | Depends on: Phase 3 (Steps 0–4 complete, 5–9 in flight) | Feeds into: Phase 5 (iOS Swift frontend), Phase 6+ (FHIR) | Companion docs: `infrastructure-cost-model.md`, `pricing-strategy.md`, `approck-feasibility-analysis.md` (Phase 5 trigger), `folder-structure-options.md` (Option 2)*

---

## 0. Roadmap Re-Numbering (read first)

The original roadmap (`roadmap-and-next-phase-040826.md` §2) put "iOS platform layer" at Phase 4 and assumed an on-device-only architecture where the Rust core lives entirely inside the iPhone. Three things have changed since that document was written:

1. **The product is going to market.** Pilot with Dr. Joey's group (10 docs, Oakland), then sell to hospitals at monthly seat rates (`pricing-strategy.md`). A sellable product needs centralized auth, usage metering, audit trails, and a recovery story that survives a dropped phone — none of which an on-device-only design provides.
2. **The Claude API key cannot ship inside an iOS binary.** Any key embedded in the app is extractable. All AI calls must be brokered by a server the moment a second user exists.
3. **The cost model already assumes a backend.** `infrastructure-cost-model.md` §1.1 models Fargate + RDS + S3 + Bedrock + Deepgram. That architecture was priced but never specified. This document specifies it.

**New numbering from here forward:**

| Phase | Was | Now |
|---|---|---|
| **Phase 4** | iOS platform layer | **Cloud backend + AWS infrastructure (this document)** |
| Phase 5 | Patient-anchored quick switch | iOS Swift frontend + UniFFI + quick switch |
| Phase 6 | FHIR READ | FHIR READ (unchanged) |
| Phase 7 | FHIR WRITE | FHIR WRITE (unchanged) |
| Phase 8 | Deep Epic/HL7 | Deep Epic/HL7 (unchanged) |

The approck feasibility analysis said: *"Re-evaluate the framework question only when EMR integration forces a backend service… at that point pick a thin off-the-shelf framework (axum)."* The trigger arrived earlier than EMR integration — it's commercialization — but the conclusion stands: **axum, thin, no framework-building.**

---

## 1. What Phase 4 Accomplishes

Phase 4 turns the single-process, single-user, SQLite-backed engine into a **multi-tenant cloud service on AWS** that the future Swift app will talk to — while keeping the domain core 100% reusable on-device for offline capture.

```
                 ┌────────────────────────── Phase 4 (AWS, us-east-1) ──────────────────────────┐
                 │                                                                               │
 iOS device      │   ALB + WAF ── axum API (ECS Fargate) ──┬── RDS Postgres (events, snapshots, │
 (Phase 5)       │        │                                │        orgs, clinicians, usage)    │
  local SQLite ──┼──► JWT auth (Cognito)                   │                                     │
  offline queue  │        │                                ├── S3 (audio chunks, exports,        │
  audio capture ─┼──► presigned PUT ──► S3 audio bucket    │        7-yr Glacier archive)        │
                 │                          │              │                                     │
                 │                       SQS jobs          │                                     │
                 │                          │              │                                     │
                 │                 worker (ECS Fargate) ───┤                                     │
                 │                  ├─ STT: Deepgram Nova-3 Medical (BAA)                        │
                 │                  └─ AI:  Claude (Bedrock / Claude Platform on AWS)            │
                 │                          ├─ claude-haiku-4-5  → per-encounter summaries       │
                 │                          └─ claude-sonnet-4-6 → one-button MDM                │
                 └───────────────────────────────────────────────────────────────────────────────┘
```

**The core architectural insight:** the event-sourced design from Phases 0–3 ports to client–server almost for free. The device appends events to its local SQLite queue; the server's Postgres event store is the system of record; sync is `append_events` (up) and `get_events_since` (down) — the *exact* operations the `Repository` trait already defines. Replay, snapshots, checksums, and the state machine are reused verbatim because `domain/` has no I/O dependencies (the dependency rule from the 2026-06-10 restructure pays off here).

**What Phase 4 explicitly delivers:**

1. Cargo workspace split (Option 2 from `folder-structure-options.md` — already planned "after Phase 3") with a new `er-server` crate.
2. `PostgresRepository` implementing the existing `Repository` trait, multi-tenant.
3. axum REST API: auth, shifts, patients, event sync, audio upload, note retrieval, MDM trigger.
4. Server-side STT (Deepgram Nova-3 Medical, behind the existing `Transcriber` trait) and server-side AI generation (the existing `Summarizer` trait, with model routing and prompt caching).
5. Terraform-managed AWS infrastructure (VPC, ECS Fargate, RDS, S3, SQS, Cognito, KMS, CloudWatch, WAF) with staging + prod environments and GitHub Actions CI/CD.
6. HIPAA-posture foundations: BAAs, encryption everywhere, audit logging, 7-year retention, de-identification before every third-party AI/STT call.
7. Per-clinician usage metering (tokens, STT minutes, cost) — the data that makes the pricing model in `pricing-strategy.md` billable.

**What Phase 4 deliberately does NOT do:** Swift/iOS code, Apple Watch, WebSocket live transcript streaming, FHIR, SOC 2 certification (it preps the controls, not the audit), multi-region, Redis (see §5.6 — explicitly evaluated and deferred with trigger conditions).

---

## 2. Decisions (each with rationale and rejected alternatives)

### D1 — Web framework: **axum**

`approck-feasibility-analysis.md` already adjudicated this: approck is ~50 crates of full-stack machinery (HTML pipeline, TS codegen, multi-module routing) earning its cost only at a scale this product won't reach soon; the recommendation for a small backend service was axum. axum adds: tower middleware (auth, tracing, rate-limit, timeouts), first-class `tracing` integration, serde extractors, and the largest Rust-web ecosystem. We borrow the four approck *patterns* already identified (composition root, error context + correlation IDs, `catch_unwind` boundary, domain-tagged tracing spans) — as code, not as a framework.

*Rejected:* actix-web (no advantage that outweighs ecosystem momentum), approck wholesale (over-scaled), warp (filter-combinator ergonomics age poorly in teams).

### D2 — Claude access path: **Amazon Bedrock for the pilot, behind a transport-agnostic client**

Three options exist in mid-2026:

| Option | BAA story | Ops story | Model IDs | Pricing |
|---|---|---|---|---|
| **Amazon Bedrock** | HIPAA-eligible under the standard AWS BAA you sign anyway (AWS Artifact, free) | IAM auth, no API key to manage; partner-operated, feature subset, release lag | `anthropic.claude-sonnet-4-6`, `anthropic.claude-haiku-4-5` | Same per-token list price |
| **Claude Platform on AWS** | Anthropic-operated through AWS infra; SigV4 + IAM + Marketplace billing; same-day API parity. **Verify BAA/HIPAA eligibility at procurement time** | SigV4, `ANTHROPIC_AWS_WORKSPACE_ID`; full feature parity incl. Batches/Files | bare `claude-sonnet-4-6` | Same |
| **Direct Anthropic API** | Anthropic offers BAAs to qualifying customers — requires separate procurement and likely enterprise commitment | Simplest; what `claude.rs` does today | bare `claude-sonnet-4-6` | Same |

**Decision: Bedrock for pilot/production.** One BAA (AWS) covers compute, storage, *and* the LLM; IAM task-role auth removes the API-key secret entirely; the features we need (Messages API, prompt caching, adaptive thinking) are all supported on Bedrock. Build `er-ai`'s HTTP layer behind a small `ClaudeTransport` enum (`DirectApi | Bedrock | ClaudePlatformAws`) so dev machines keep using the direct API with `ANTHROPIC_KEY` and the swap is config, not code. Re-evaluate Claude Platform on AWS at procurement — if its BAA story checks out, it offers Bedrock's IAM/billing advantages *without* the feature lag.

**Defense in depth regardless of transport:** the existing `Deidentifier` runs server-side before every call. The BAA makes PHI transmission *legal*; de-identification keeps making it *minimal*.

### D3 — Models and routing: **Haiku 4.5 for per-encounter summaries, Sonnet 4.6 for MDM**

Current pricing (verified 2026-06): `claude-sonnet-4-6` $3.00/$15.00 per MTok in/out (1M context); `claude-haiku-4-5` $1.00/$5.00 per MTok (200K context). Routing summaries to Haiku cuts the largest-volume call (8 per patient) by ~3× with quality that is acceptable for *draft* HPI/Exam text that the clinician reviews — while the MDM, the billing-bearing headline artifact, stays on Sonnet. This is the `infrastructure-cost-model.md` "model routing" lever (§6, 30–50% of Claude bill) made concrete. The `model` field already exists on `ClaudeSummarizer`; Phase 4 makes it per-call-type config.

Run the pilot's first two weeks with summaries on Sonnet, capture outputs, then A/B Haiku against them before flipping the default — one config change, measurable quality gate.

### D4 — Prompt caching: **yes, but the prompts must grow first (real gotcha)**

Cache reads cost ~0.1× input; writes 1.25× (5-min TTL). But caching has a **minimum cacheable prefix: 2,048 tokens on Sonnet 4.6 and 4,096 on Haiku 4.5** — below that, `cache_control` silently does nothing. The current system prompts in `claude.rs` are ~350–600 tokens. **They will not cache at all today.**

The fix is aligned with quality anyway: expand the MDM system prompt with the full 2021 E/M MDM level-determination tables, specialty-specific documentation guidance, and 2–3 few-shot exemplar MDMs (good for output quality, pushes the stable prefix past 2,048 tokens), mark it `cache_control: {type: "ephemeral"}`, and put all volatile content (transcript, objective data) after it in the user message. Verify with `usage.cache_read_input_tokens > 0` in the usage log — make that an explicit Step 6 acceptance check. The cost model's "without caching ~$95+/clinician" warning (§2) only becomes real at the AI-heavy usage tier; at Phase 3's call pattern caching is a ~20–30% saver once prompts are cache-sized.

Also adopt the **Batches API (50% discount)** for non-interactive work: nightly re-summarization backfills, regression evals of prompt changes, bulk de-id audits. Never for interactive MDM (latency).

### D5 — STT: **Deepgram Nova-3 Medical (HIPAA tier, BAA) server-side; `Transcriber` trait keeps every door open**

The product summary says "audio transcribed on-device for privacy"; the cost model and doctor-feedback responses moved to cloud STT. Resolve the tension explicitly:

- **Pilot reality:** ambient ED audio (noisy, multi-speaker, medical vocabulary) is where on-device Whisper is weakest and Deepgram's medical model + diarization is strongest. Pilot quality determines whether the company exists; ship cloud STT.
- **Privacy posture:** audio goes device → S3 (KMS-encrypted, BAA) → Deepgram (HIPAA tier, BAA, no-retention config). PHI never touches a non-BAA service.
- **The trait is the hedge:** `Transcriber` already abstracts STT. A future `WhisperKitTranscriber` on-device (Phase 5+) or self-hosted Whisper on GPU (cost lever at 10k users, per cost model §6) slots in without architectural change. Don't build either now.
- *Rejected for now:* AWS Transcribe Medical — ~$0.075/min standard vs Deepgram's ~$0.009/min HIPAA tier, 8× the cost for comparable accuracy; it stays the fallback if Deepgram procurement stalls (it's under the AWS BAA with zero extra paperwork).

Pipeline shape for the pilot: **chunked pre-recorded transcription**, not live streaming. Client uploads ~30–60s Opus chunks; the worker transcribes each chunk on arrival and appends to the encounter transcript; `TranscriptReady` publishes when the encounter stops. Live word-by-word streaming to the phone screen is a Phase 5/6 UX decision that would force WebSockets + Redis now for no pilot value (the doctor isn't staring at the screen mid-encounter — that's the entire product thesis).

### D6 — Database: **RDS PostgreSQL 16; SQLite stays on-device; one `Repository` trait, two impls**

Postgres because: real concurrency (rusqlite's `std::sync::Mutex<Connection>` design was explicitly single-process), `jsonb` for event payloads with indexing, row-level multi-tenancy, RDS PITR (35-day window) for the medico-legal "never lose an event" requirement, and a clean partitioning story at scale (`events` partitioned by org or month at ~5–10k users, per cost model §6 "co-located/regional DB sharding").

Pilot sizing: **db.t4g.small, single-AZ, 50GB gp3** (~$30/mo). Promote to Multi-AZ db.t4g.medium+ at the first *paid* hospital contract — Multi-AZ is a contractual-SLA artifact, not a pilot need. Schema in §4. The existing events/snapshots/checksum design maps 1:1; sequence becomes `BIGSERIAL` with per-patient ordering preserved by `(patient_id, sequence_id)` index, exactly as SQLite's AUTOINCREMENT does today.

### D7 — Redis: **No. Explicitly deferred, with written triggers** (you asked — here's the analysis)

Everything Redis is typically bought for already has a cheaper, lower-ops answer at this scale:

| Typical Redis job | Phase 4 answer | Add ElastiCache when… |
|---|---|---|
| Session store | Stateless JWTs from Cognito; no server session state | Never (JWTs scale) |
| Job/work queue | **SQS** — serverless, $0.40/M requests, DLQ built in, zero cluster ops | Never for this workload (SQS is strictly better for fire-and-forget jobs) |
| Hot cache (patient lists, notes) | DashMap projection in-process + Postgres; pilot read volume is trivial | p95 read latency degrades at 1k+ users AND Postgres read replicas don't fix it |
| Rate limiting | Per-IP at WAF + in-process tower middleware per token | Multiple API tasks need *shared* rate-limit state (real at ~50+ RPS sustained) |
| WebSocket pub/sub fan-out | No WebSockets in Phase 4 (D5) | Live transcript streaming ships and API tasks > 1 |

A `cache.t4g.micro` is only ~$12/mo — the cost isn't the money, it's a second stateful system to secure (HIPAA scope!), patch, monitor, and explain in the security review. Defer until a trigger fires.

### D8 — Compute: **ECS Fargate (2 services: `api`, `worker`), Docker, no EC2/EKS**

- Fargate: no instance patching (shrinks the HIPAA/SOC2 surface — AWS owns host compliance), per-task IAM roles, scales to the cost model's assumptions (§1.1 names Fargate). Rust's tiny memory footprint makes Fargate's premium irrelevant: the API serves the pilot in 0.5 vCPU / 1GB.
- Two services from day one: `er-server-api` (ALB-fronted, latency-sensitive) and `er-server-worker` (SQS consumer doing STT + Claude calls — slow, bursty, retryable). Splitting them means a 90-second MDM storm never starves health checks, and they scale on different signals (CPU vs queue depth).
- *Rejected:* EKS (cluster tax absurd at this size), Lambda (Rust works well, but 15-min cap collides with long STT jobs, and persistent connection reuse to Postgres/Deepgram/Bedrock is cleaner in a long-lived process; revisit for spiky preprocessing at scale), App Runner (no SQS-driven worker story).

### D9 — Auth & tenancy: **Cognito user pool, org-scoped JWTs, server-enforced tenancy**

Cognito: HIPAA-eligible, free under 50k MAU (pilot cost: $0), native SwiftSDK/Amplify path for Phase 5, hosted UI good enough for pilot onboarding, SAML/OIDC federation door open for hospital SSO at the enterprise tier (it will be demanded — `pricing-strategy.md` §7 security review). Claims: `sub` → clinician, `custom:org_id` → tenant. Every repository query is org-scoped by the server (never trust client-supplied org). Add Postgres RLS as a second belt in the hardening step.
*Rejected:* Auth0 (better DX, ~$0.55+/user HIPAA tier vs $0 — revisit if hospital SSO requirements outgrow Cognito), roll-your-own (never, this is medical software).

### D10 — Sync protocol: **event upsync/downsync with client-stamped event UUIDs**

The device works offline (ED basements have dead zones); the server is authoritative once events land. Protocol:

1. Every event gets a **client-stamped `event_id: Uuid` (UUIDv7)** at creation. This is new — today events have no identity until SQLite assigns `sequence_id`. UUIDv7 gives time-ordered IDs for free.
2. **Up:** `POST /v1/patients/{id}/events` with a batch. Server inserts with `ON CONFLICT (event_id) DO NOTHING` → idempotent retries, at-least-once upload becomes exactly-once storage. Response returns assigned `sequence_id`s.
3. **Down:** `GET /v1/patients/{id}/events?since={seq}` — the existing `get_events_since` semantics over HTTP. Server-generated events (`TranscriptReady` from the STT worker, `EncounterSummarized`, `MdmGenerated`) reach the device through the same cursor. One mechanism, no special cases.
4. **Conflicts:** structurally rare — one treating clinician authors one note. The state machine's existing guards (`can_ingest_results`, no-op invalid transitions) absorb out-of-order arrivals the same way replay already tolerates them.
5. `shift.json` dies; shift metadata moves to server tables (it was always the non-event-sourced wart).

This also gives Phase 4 a free integration client: **point the existing CLI at the server** via a `RemoteRepository: Repository` impl. The dev CLI becomes the API's first consumer and contract test — before a line of Swift exists.

### D11 — IaC & CI/CD: **Terraform + GitHub Actions (OIDC, no long-lived AWS keys)**

Terraform (industry default; hospital security reviews ask for IaC evidence; module ecosystem). Two workspaces: `staging`, `prod` — separate state, separate VPCs, prod-only deletion protection. GitHub Actions: `cargo test` + `clippy -D warnings` + `cargo audit` on PR; on merge to master → build multi-stage Docker image (cargo-chef for layer-cached builds; ~slim runtime image) → push ECR → `aws ecs update-service` to staging; manual approval gate → prod. *Rejected:* CDK (Terraform reads better in security review), CodePipeline (GH Actions already home).

---

## 3. Workspace Split (Step 0) — the Option 2 trigger fires now

`folder-structure-options.md` Option 2 was "planned after Phase 3." The server crate is the forcing function: `er-server` must depend on the domain and application layers **without** dragging in the CLI, mocks, or rusqlite. The current layer folders convert ~1:1:

```
er-note-app/                       # workspace root
├── Cargo.toml                     # [workspace], shared workspace.dependencies
├── crates/
│   ├── er-domain/                 # src/domain/* — deps: serde, uuid, chrono, thiserror ONLY
│   ├── er-app/                    # src/application/* — EventBus, ShiftManager, load, drivers
│   ├── er-store/                  # Repository trait + SqliteRepository (+ feature "sqlite")
│   ├── er-ai/                     # Summarizer trait, ClaudeSummarizer (+ transport enum), Deidentifier
│   ├── er-pipeline/               # AudioCapture/Transcriber traits + mocks (+ later: deepgram, whisper)
│   ├── er-cli/                    # main.rs, cli.rs, export.rs, spike/standalone bins
│   └── er-server/                 # NEW — axum API + worker (Phase 4 Steps 1–6)
├── tests/ → move into owning crates (er-store: persistence; er-app: crash_recovery)
└── docs/
```

Dependency arrows stay inward: `er-server → {er-app, er-store, er-ai, er-domain}`; `er-domain` depends on nothing internal. `er-server` adds the server-only deps (axum, sqlx, aws-sdk-*, jsonwebtoken) so the future `er-ffi` (Phase 5) never compiles them. This is a `git mv` + Cargo.toml surgery refactor — zero behavior change, gated on the full existing test suite passing.

---

## 4. Data Model (Postgres)

```sql
-- Tenancy & identity ------------------------------------------------------
CREATE TABLE orgs (
    id            UUID PRIMARY KEY,
    name          TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE clinicians (
    id            UUID PRIMARY KEY,
    org_id        UUID NOT NULL REFERENCES orgs(id),
    cognito_sub   TEXT NOT NULL UNIQUE,          -- JWT subject
    display_name  TEXT NOT NULL,
    role          TEXT NOT NULL DEFAULT 'physician',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Clinical aggregates ------------------------------------------------------
CREATE TABLE patients (                          -- registration stub; PHI-minimal
    id            UUID PRIMARY KEY,
    org_id        UUID NOT NULL REFERENCES orgs(id),
    created_by    UUID NOT NULL REFERENCES clinicians(id),
    chief_complaint TEXT NOT NULL,
    display_label TEXT,                          -- "Room 4" — not the patient name
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE shifts (
    id            UUID PRIMARY KEY,
    org_id        UUID NOT NULL REFERENCES orgs(id),
    clinician_id  UUID NOT NULL REFERENCES clinicians(id),
    started_at    TIMESTAMPTZ NOT NULL,
    ended_at      TIMESTAMPTZ,
    status        TEXT NOT NULL DEFAULT 'active'
);

CREATE TABLE shift_patients (
    shift_id      UUID NOT NULL REFERENCES shifts(id),
    patient_id    UUID NOT NULL REFERENCES patients(id),
    position      INT  NOT NULL,                 -- stable list order (ShiftManager invariant)
    PRIMARY KEY (shift_id, patient_id)
);

-- Event store (mirrors SQLite schema + tenancy + identity) ------------------
CREATE TABLE events (
    sequence_id   BIGSERIAL PRIMARY KEY,
    event_id      UUID NOT NULL UNIQUE,          -- client-stamped UUIDv7 → idempotent sync (D10)
    org_id        UUID NOT NULL,
    patient_id    UUID NOT NULL,
    clinician_id  UUID,
    device_id     TEXT,
    event_type    TEXT NOT NULL,
    payload       JSONB NOT NULL,                -- serde NoteEvent, same JSON as SQLite today
    clinical_occurred_at TIMESTAMPTZ NOT NULL,
    recorded_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_events_patient_seq ON events (patient_id, sequence_id);
CREATE INDEX idx_events_org         ON events (org_id, recorded_at);

CREATE TABLE snapshots (
    patient_id    UUID PRIMARY KEY,
    org_id        UUID NOT NULL,
    sequence_id   BIGINT NOT NULL,
    snapshot      JSONB NOT NULL,
    checksum      TEXT NOT NULL,                 -- SHA-256, verified on load (unchanged design)
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Pipeline & metering --------------------------------------------------------
CREATE TABLE audio_chunks (
    id            UUID PRIMARY KEY,
    org_id        UUID NOT NULL,
    patient_id    UUID NOT NULL,
    encounter_id  UUID NOT NULL,
    s3_key        TEXT NOT NULL,
    duration_ms   INT,
    status        TEXT NOT NULL DEFAULT 'uploaded',  -- uploaded|transcribing|done|failed
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE usage_log (                          -- COGS per clinician → feeds billing/pricing
    id            UUID PRIMARY KEY,
    org_id        UUID NOT NULL,
    clinician_id  UUID,
    patient_id    UUID,
    kind          TEXT NOT NULL,                  -- 'summarize'|'mdm'|'stt'
    model         TEXT,                           -- 'claude-haiku-4-5'|'claude-sonnet-4-6'|'nova-3-medical'
    input_tokens  BIGINT, output_tokens BIGINT,
    cache_read_tokens BIGINT, cache_write_tokens BIGINT,
    stt_seconds   INT,
    cost_micros   BIGINT NOT NULL,                -- USD micro-dollars, computed at call time
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

Notes:
- `PostgresRepository` (sqlx, compile-time-checked queries) implements the **unchanged** `Repository` trait, plus an org-scoped constructor (`PostgresRepository::for_org(pool, org_id)`) so tenancy can't be forgotten at a call site.
- The SQLite migration adds one column on-device too: `event_id TEXT UNIQUE` (D10). No production data exists; no migration tooling needed yet beyond `sqlx migrate`.
- Transcripts ride inside `events.payload` exactly as today. The G7 transcript-compaction question (Dr. Joey F4, "saved and discoverable") gets **more** acute with a central store — see Risks R5.

---

## 5. API Surface (v1)

All routes under `/v1`, JWT bearer required (Cognito JWKS validation in tower middleware), org inferred from token. OpenAPI spec generated via `utoipa` — it becomes the Phase 5 Swift contract.

```
Auth/identity
  GET  /v1/me                              → clinician profile + org

Shifts
  POST /v1/shifts                          → start shift
  POST /v1/shifts/{id}/end
  GET  /v1/shifts/{id}                     → shift + patient list (replaces shift.json)

Patients & notes
  POST /v1/patients                        → register {chief_complaint, display_label}
  GET  /v1/patients/{id}/note              → projected PatientNote (snapshot fast-path)
  GET  /v1/patients/{id}/note/export       → plain-text note (er-cli export logic, server-side)

Event sync (D10 — the core protocol)
  POST /v1/patients/{id}/events            → batch append, idempotent on event_id
  GET  /v1/patients/{id}/events?since=N    → delta download (server- and client-authored)

Audio → STT
  POST /v1/patients/{id}/encounters/{eid}/audio
       → { upload_url (S3 presigned PUT, 15-min expiry), chunk_id }
  POST /v1/audio/{chunk_id}/complete       → enqueue STT job (SQS)

AI generation
  POST /v1/patients/{id}/mdm               → enqueue MDM job → 202 {job_id}
  GET  /v1/jobs/{id}                       → queued|running|done|failed (client polls; push in Phase 5)

Ops
  GET  /healthz (no auth)  ·  GET /v1/usage/summary (org admin)
```

Worker consumes SQS:
- `TranscribeChunk{chunk_id}` → fetch from S3 → Deepgram → append transcript; on encounter stop → publish `TranscriptReady` → de-identify → `summarize_encounter` (Haiku) → publish `EncounterSummarized` → log usage. (Step 5/6 of Phase 3's flow, relocated server-side.)
- `GenerateMdm{patient_id, job_id}` → assemble objective data (Phase 3 Step 6 logic, unchanged) → de-identify → `generate_mdm` (Sonnet, cached prompt) → publish `MdmGenerated` + `TriggerMDM` → log usage.

Persist-first ordering, snapshot triggers, and the `MdmGenerated`-before-`TriggerMDM` crash-safety ordering all carry over from Phase 3 untouched — the `EventBus` just gets a `PostgresRepository` instead of a `SqliteRepository`.

---

## 6. AWS Infrastructure (Terraform)

| Resource | Pilot config | Why / scaling note |
|---|---|---|
| **VPC** | 2 AZ, public + private subnets, 1 NAT gateway | Tasks in private subnets; NAT for egress to Deepgram/Bedrock endpoints. (Bedrock + S3 + SQS get **VPC interface/gateway endpoints** to cut NAT data charges and keep PHI on the AWS backbone.) |
| **ALB + WAF** | 1 ALB, HTTPS only (ACM cert), WAF managed rules + per-IP rate limit | TLS 1.2+; WAF is cheap ($10/mo) and a security-review checkbox |
| **ECS Fargate `api`** | 2 × (0.5 vCPU / 1GB) across AZs | Rust/axum is comfortably <100MB RSS; 2 tasks = zero-downtime deploys. Autoscale on CPU later |
| **ECS Fargate `worker`** | 1 × (0.5 vCPU / 1GB) | Autoscale on SQS queue depth (target tracking); burst-friendly |
| **RDS Postgres 16** | db.t4g.small, single-AZ, 50GB gp3, PITR on, 7-day backups | → Multi-AZ + db.t4g.medium at first paid contract (D6) |
| **S3** | `audio` bucket (SSE-KMS, lifecycle: 30d hot → Glacier Deep Archive 7yr), `exports` bucket | HIPAA retention; cost model §1.4 |
| **SQS** | `stt-jobs`, `ai-jobs`, each with DLQ (maxReceive 3) | DLQ alarm = the "summarization failed silently" detector |
| **Cognito** | 1 user pool, hosted UI, MFA optional-on | $0 at pilot scale |
| **KMS** | 1 CMK shared by RDS/S3/SQS/Secrets | $1/mo + API calls |
| **Secrets Manager** | Deepgram key, (dev-only Anthropic key) | Bedrock path needs no LLM secret at all — IAM task role |
| **CloudWatch** | Logs (90d hot, archive to S3), dashboards, alarms (5xx, DLQ>0, RDS storage, p95) | Datadog deferred to post-pilot (cost model lists it as production-tier) |
| **CloudTrail** | Org trail → S3, locked | HIPAA audit prerequisite |
| **ECR + Route53 + ACM** | 1 repo, `api.<domain>` | |

**Environments:** `staging` (always-on, tiny) and `prod`. Local dev = `docker-compose` with Postgres + MinIO (S3-compatible) + ElasticMQ (SQS-compatible) so `cargo test` never needs AWS.

---

## 7. Cost Analysis

### 7.1 Claude API per patient (Phase 3 call pattern: 8 encounter summaries + 1 MDM)

Token assumptions per call: summary ≈ 1.2K in (system ~0.4K + transcript ~0.3K + prior sections ~0.5K) / 0.5K out; MDM ≈ 2.5K in / 0.8K out (before prompt-expansion + caching, which roughly nets out: bigger prefix, but 90% of it at 0.1× on cache hits).

| Strategy | Summaries (8×) | MDM (1×) | **Per patient** | **Per clinician/mo (324 pts)** |
|---|---:|---:|---:|---:|
| All Sonnet 4.6 | $0.089 | $0.020 | **$0.109** | **~$35** |
| **Routed (Haiku summaries + Sonnet MDM)** | $0.030 | $0.020 | **$0.050** | **~$16** |

Matches the cost model's $32.40/clinician (§2) for the unrouted case; routing halves it. **Per-note COGS of ~5–11¢ against a $349/seat price** — AI is not the margin problem; fixed costs are (cost model §5).

### 7.2 Monthly infrastructure totals

| Line item | Pilot (10 docs) | 100 docs |
|---|---:|---:|
| Claude (routed → all-Sonnet range) | $160 – $350 | $1,600 – $3,500 |
| Deepgram Nova-3 Medical (~1,944 min/doc × $0.009) | ~$175 | ~$1,750 |
| Fargate (api + worker) | ~$45 | ~$150 (4–6 tasks) |
| RDS Postgres | ~$30 | ~$130 (Multi-AZ t4g.medium) |
| ALB + WAF + NAT + VPC endpoints | ~$75 | ~$110 |
| S3 + Glacier + SQS + KMS + Secrets + ECR | ~$10 | ~$60 |
| CloudWatch + CloudTrail | ~$15 | ~$50 |
| Cognito | $0 | $0 (<50k MAU) |
| **Total infra COGS** | **≈ $510 – $700/mo** | **≈ $3,850 – $5,750/mo** |
| **Per clinician** | ≈ $51–70 | ≈ $39–58 |

Consistent with the Lean-MVP curve in `infrastructure-cost-model.md` §4 ($620/user at 10 → $95 at 100, which includes labor/compliance this table excludes). The pilot burns well under $1k/mo of hard infrastructure — the free-pilot recommendation in `pricing-strategy.md` §1 is affordable.

### 7.3 Cost levers wired in from day one

1. **Model routing** (D3) — config flag, A/B-able. ~50% of Claude spend.
2. **Prompt caching** (D4) — requires the prompt-expansion work; verify via `usage_log.cache_read_tokens`. ~20–30% once prompts are cache-sized.
3. **Batch API** for offline jobs — 50% on that traffic.
4. **Opus/VAD audio** — client encodes Opus ~16kbps + drops silence; 40–60% of STT minutes (cost model §6). Encode params live client-side (Phase 5) but the chunk contract supports it now.
5. **usage_log** makes every lever measurable per org/clinician — and is the metering substrate for per-seat true-up billing (`pricing-strategy.md` §2).

---

## 8. Security & HIPAA Posture (Phase 4 scope)

| Control | Implementation |
|---|---|
| **BAAs** | AWS (Artifact, free, sign before any PHI lands) · Deepgram HIPAA tier · Bedrock covered under AWS BAA (verify Claude models listed as HIPAA-eligible at signing) |
| Encryption in transit | TLS 1.2+ ALB; TLS to RDS (`sslmode=require`); HTTPS-only S3 |
| Encryption at rest | KMS CMK: RDS, S3, SQS, Secrets, CloudWatch Logs |
| PHI minimization | `Deidentifier` server-side before every Claude call (unchanged Phase 3 invariant — now enforced in one place instead of N devices); Deepgram no-retention config; `patients` table stores chief complaint + room label, not demographics |
| Audit | App-level `AuditEntry` on the note (existing) + `events` table is itself the immutable audit log + CloudTrail for infra |
| Retention | Audio: 30d hot → Glacier Deep Archive (7yr) · events: never deleted (medico-legal) · logs: 90d hot, 7yr cold |
| Access control | Cognito JWT → org-scoped queries; per-task IAM roles (api can't call Bedrock; worker can't reach ALB); Postgres RLS in hardening step |
| Network | Private subnets, SG least-privilege, VPC endpoints, WAF |
| Secrets | Secrets Manager + rotation; **no Anthropic key anywhere in prod** (Bedrock IAM) |
| Incident readiness | DLQ + 5xx alarms → email/Slack; runbook doc; RDS PITR restore drill in Step 8 |
| SOC 2 prep (not cert) | This table *is* most of the controls evidence; start the audit clock post-pilot per `pricing-strategy.md` §7 (HECVAT/SIG reviews will demand it) |

---

## 9. Step-by-Step Implementation

```
Order of Operations

Step  Action                                                        Gate
────  ───────────────────────────────────────────────────────────   ─────────────────────────────
0     Finish Phase 3 (Steps 5–9)                                    Phase 3 acceptance criteria
1     Workspace split (Option 2): er-domain/app/store/ai/           cargo test --workspace passes,
      pipeline/cli crates; tests move into owning crates            zero behavior change
2     er-server skeleton: axum, /healthz, config, tracing,          container builds & runs locally;
      Dockerfile (cargo-chef), docker-compose (PG+MinIO+ElasticMQ)  healthz 200
3     PostgresRepository (sqlx) + migrations + event_id on          persistence + crash-recovery
      NoteEvent envelope; SQLite gains event_id too                 suites pass against BOTH repos
4     Auth + tenancy: Cognito pool (Terraform), JWKS middleware,    401 without token; cross-org
      orgs/clinicians tables, org-scoped repo constructor           access test fails closed
5     Core API + sync protocol: shifts/patients/events/note         CLI-as-client (RemoteRepository)
      endpoints; RemoteRepository in er-cli                         drives full lifecycle remotely
6     Audio + STT: presigned S3 upload, SQS, worker service,        recorded WAV → transcript on
      DeepgramTranscriber behind Transcriber trait                  note; DLQ catches poison chunk
7     AI generation server-side: Summarizer in worker, model        full server lifecycle test:
      routing config, expanded+cached prompts, ClaudeTransport      register→audio→summaries→mdm→
      (DirectApi|Bedrock), usage_log metering                       sign→export; cache_read>0 verified
8     Terraform: VPC/ALB/ECS/RDS/S3/SQS/Cognito/KMS/CW/WAF;         staging deploy green; smoke test
      GitHub Actions OIDC → ECR → ECS staging (+gated prod)         passes against staging URL
9     Hardening + ops: RLS, alarms, dashboards, PITR restore        security checklist signed off;
      drill, runbook, rate limits, pen-test prep                    restore drill documented
10    Load & cost validation + docs: k6 shift-scale load test       p95 targets met; cost dashboard
      (10 docs × 18 pts × 8 enc), CLAUDE.md/ARCHITECTURE.md         live; docs truthful
      truth-up, OpenAPI spec published for Phase 5
```

Steps 3–7 are pure Rust and run entirely against docker-compose — AWS spend starts at Step 8 (≈ first month of staging ~$150). Step 5's CLI-as-client gate is the cheapest possible proof that the API contract is right before Swift exists.

### Per-step details worth pinning

**Step 1 (workspace)** — Hard scope: `git mv` + manifest edits + import paths only. The one allowed code change: `EventBus`'s repo field is already `Arc<dyn Repository>` — confirm nothing in `application/` names `SqliteRepository` concretely (it shouldn't; `main.rs` constructs it).

**Step 3 (Postgres)** — sqlx with offline mode (`.sqlx` checked in) so CI doesn't need a DB for type-checking. The integration suite from `tests/persistence_integration.rs` becomes generic over `R: Repository` and runs twice (SQLite + Postgres/testcontainers). The crash-recovery suite is the real prize: it proves replay/snapshot/checksum semantics survived the port.

**Step 4 (auth)** — JWKS cached with kid-based refresh; clock-skew tolerance; map `cognito_sub → clinician` once per request (cacheable in-process). Org enforcement lives in the repository constructor, not in handlers — handlers literally cannot query cross-org.

**Step 5 (sync)** — `RemoteRepository` implements `Repository` over reqwest. The CLI's `reload` command now exercises: local DashMap drop → server `get_events_since` → replay — i.e., the Phase 2 crash-recovery story, but the "disk" is now the cloud. Demo script: two CLI processes, same patient, one records, the other reloads and sees the note.

**Step 6 (STT)** — `DeepgramTranscriber` implements `Transcriber` (pre-recorded endpoint, diarization on, medical model). Chunk contract: client may upload chunks out of order; worker orders by chunk index before concatenation. Poison chunks (corrupt audio) → 3 receives → DLQ → alarm; the encounter's transcript notes the gap rather than failing the note (Phase 3's "best-effort, never lose the encounter" rule, extended).

**Step 7 (AI)** — Port `ClaudeSummarizer` HTTP layer to the transport enum; Bedrock via `aws-sdk-bedrockruntime` (model IDs `anthropic.claude-haiku-4-5`, `anthropic.claude-sonnet-4-6`); direct API path keeps `claude-haiku-4-5`/`claude-sonnet-4-6` for dev. Expand the MDM system prompt past 2,048 tokens (D4) with E/M leveling tables + exemplars; `cache_control` on the system block; assert `cache_read_input_tokens > 0` on the second call in the lifecycle test. Every call writes `usage_log` (tokens × current price table → `cost_micros`).

**Step 10 (load)** — k6 profile: 10 concurrent "shifts," each 18 patients × 8 encounters over a compressed hour; targets: API p95 < 300ms (excluding AI jobs), MDM job completion p95 < 30s, zero DLQ entries, zero event-sync conflicts. This is the number you show a hospital IT reviewer.

---

## 10. Acceptance Criteria

1. **`cargo test --workspace` passes; zero warnings** across all crates.
2. **One `Repository` trait, three impls** (SQLite, Postgres, Remote) passing the same generic persistence + crash-recovery suites.
3. **Full remote lifecycle:** CLI against staging completes `register → start/stop ×N → lab/med/dx/dispo → mdm → review → sign → export` with all AI generation server-side.
4. **Idempotent sync:** replaying the same event batch upload twice yields identical server state (event_id dedupe proven by test).
5. **Tenancy fails closed:** authenticated user of org A cannot read or write org B data (tested, not assumed).
6. **De-identification before every third-party call** — unit test asserts patient name/label never appears in the string passed to the Claude/Deepgram clients (Phase 3 criterion #10, now server-side).
7. **Prompt caching live:** `usage_log` shows `cache_read_tokens > 0` on second-and-later MDM calls.
8. **Metering accurate:** `usage_log` totals for a scripted load run match Bedrock/Deepgram invoiced usage within 5%.
9. **Staging deployed by CI from a clean clone** — `terraform apply` + GH Actions, no console clicking.
10. **Alarms fire:** kill the worker mid-job → DLQ alarm; restore RDS to point-in-time in the drill.
11. **OpenAPI spec published** and accurate (contract for Phase 5 Swift work).
12. **Docs truthful:** CLAUDE.md, ARCHITECTURE.md, and this file's status updated.

---

## 11. Risks & Open Questions

**R1 — BAA timing gates everything PHI.** No real patient audio touches AWS/Deepgram until BAAs are signed and the hospital's own BAA/DUA is executed (`pricing-strategy.md` §9: "start the BAA the day after the friend says yes"). Mitigation: Steps 1–7 run on synthetic audio locally; start AWS BAA (instant, via Artifact) and Deepgram procurement at Step 0.

**R2 — Offline-first vs server-authoritative tension.** The ED dead-zone story demands the device keep working offline; the pilot could over-rely on connectivity. Mitigation: the sync protocol (D10) is designed for store-and-forward from day one; Phase 5 must implement the device queue with the same discipline as the server side. Decision recorded: server is source of truth *once events arrive*; device queue is durable until ack.

**R3 — Deepgram accuracy on real ED audio is unproven.** Mitigation: Step 6 gate includes a transcription eval on recorded (consented/synthetic) ED-style audio before the pilot; AWS Transcribe Medical is the zero-procurement fallback; the `Transcriber` trait makes switching a config change.

**R4 — Haiku summary quality.** Mitigation: pilot starts all-Sonnet; routing flips only after A/B against captured outputs (D3). Cost difference at 10 users (~$190/mo) doesn't justify quality risk in week one.

**R5 — Centralizing transcripts amplifies the discoverability concern (Dr. Joey F4 / G7).** A subpoena now has one address. The `TranscriptCompacted` tombstone design (`creative-improvements.md`) moves from "before production" to "before any *paid* production" — schedule it as Phase 4.5 or early Phase 5, and decide Q6 (ephemeral-transcript mode vs immutable-audit selling point) with counsel before the first paid contract.

**R6 — Cognito DX friction in Phase 5.** Hosted UI is clunky in native apps. Mitigation: budget for the Amplify Swift SDK path; if it fights back, the JWT middleware is issuer-agnostic enough to swap Auth0 in front without API changes.

**Q1 — Claude Platform on AWS vs Bedrock at procurement:** if its BAA story verifies, it wins on parity + IAM. Decide at Step 7; the transport enum makes it a one-line config swap.
**Q2 — Per-encounter summarize server-side vs batch at MDM time:** Phase 4 keeps Phase 3's inline-on-stop semantics (Q3 there). If pilot telemetry shows summaries are rarely read before MDM, batch them — `usage_log` will say.
**Q3 — Live transcript streaming (WebSockets + Redis trigger D7):** decide after pilot feedback, not before. The product thesis says the doctor isn't watching the screen.
**Q4 — When does `er-ffi` (UniFFI) get cut?** Phase 5, against `er-app` — the workspace split makes that crate boundary already correct.

---

## 12. What Phase 5 (Swift) Inherits From This Phase

- **OpenAPI spec** + staging environment to develop against from day one.
- **Auth flow:** Cognito (Amplify Swift or AppAuth) → JWT → every API call.
- **Sync contract:** stamp UUIDv7 event_ids, queue locally in SQLite (the same `er-store` SQLite code via UniFFI), upload batches, page `?since=` downloads. The Rust core is the offline engine on-device and the projection engine on the server — one codebase, both sides of the wire.
- **Audio contract:** Opus chunks ~30–60s, presigned PUT, `complete` callback.
- **No AI keys, no PHI processing decisions, no prompt logic** on the device — the server owns all of it, which is exactly what makes the App Store binary boring and the security review survivable.

---

*End of Phase 4 plan. Begin after Phase 3 Steps 5–9 land. First actions: sign the AWS BAA (free, instant), open Deepgram HIPAA-tier procurement, and execute Step 1's workspace split.*
