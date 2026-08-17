# ER Note App - Claude Context

## Project Summary

Rust-based iOS ambient clinical documentation system for emergency departments. Uses Claude API for note summarization and MDM generation. Designed for non-linear, discontinuous ED encounters where a single patient visit involves 5-15 separate interactions across hours.

Key differentiator: **multi-encounter merging** — captures every interaction across a patient's entire ED stay and stitches them into one coherent note. Abridge/DAX record a single conversation; this records the full visit.

## Current Branch: `master`

## What's Working

- Domain model with PatientNote aggregate root, state machine (6 phases), event types
- Claude summarizer (Sonnet 4) with multi-encounter merging and MDM generation
- De-identification (regex-based PHI stripping before API calls)
- Plain text note export
- Serialization round-trip (hydrate test passes)
- SQLite schema with events + snapshots tables
- `SqliteRepository` — full implementation of event append, query, snapshot save/load with SHA-256 checksum validation
- `replay()` and `apply_event()` — reconstruct PatientNote from event stream
- `load_patient_note()` — snapshot fast-path with delta replay, full replay fallback
- `EventBus` — persist-first, then project to in-memory DashMap, then broadcast; snapshots on Idle/Finalizing/Signed transitions
- NoteEvent convenience constructors (`audio_started`, `audio_stopped`, etc.)
- `EncounterDriver` — mock audio/STT orchestration that publishes AudioStarted, AudioStopped, and TranscriptReady through EventBus
- `ShiftManager` — multi-patient shift orchestration, active-patient switching, guarded MDM/review/sign/reload operations, stable patient listing
- Interactive developer CLI in `crates/er-cli/src/main.rs` with shift resume via `shift.json` and SQLite-backed note reload
- Crash-recovery integration tests for full lifecycle reload and mid-encounter patient switching
- **AI note generation wired into the live pipeline (Phase 3):** `stop_encounter` auto-summarizes the transcript into HPI/Exam (de-identified first); `trigger_mdm` is a one-button MDM generator that assembles results + meds + diagnoses + disposition and persists a billing-aware MDM
- **Async bridge:** `ShiftManager<A, T, S>` carries a `tokio::Runtime` and `block_on`s the async `Summarizer` at exactly two points (summarize on stop, generate MDM on trigger); CLI stays synchronous
- **Objective-data capture:** disposition, medications, diagnoses, and free-text "data reviewed" are event-sourced onto the note and feed the MDM
- **Standalone `mdm_standalone` bin:** paste note + chart data on stdin → generated MDM, no audio/event-sourcing/FHIR (Dr. Joey's F10 demo path)
- **Cargo workspace (Phase 4 Step 1):** eight crates, dependency arrows inward — see *Project Layout*
- **`er-server` (Phase 4 Steps 2–5):** axum HTTP server, `/healthz`, Docker/docker-compose, Postgres via sqlx with checked-in `.sqlx` offline cache
- **`PostgresRepository`:** the same `Repository` trait over Postgres, org-scoped by construction, with a per-patient advisory lock on append
- **Auth + tenancy (Step 4):** JWKS/JWT middleware (dev keys or Cognito), `orgs`/`clinicians` tables, `cognito_sub → clinician` resolution, org scoping enforced in the repository constructor rather than in handlers
- **Sync protocol (Step 5):** `POST/GET /v1/patients/{id}/events` (idempotent by `event_id`, cursor paging), snapshot PUT/GET, note + export + stats reads, shift start/active/end
- **`er-api`:** the wire contract as a crate — DTOs for both sides, plus `ServerApi` + `RemoteRepository` behind the `client` feature
- **Remote CLI mode (Step 5 S10):** `ER_SERVER_URL` + `ER_TOKEN` swap SQLite for the server; every command works unchanged over HTTP, and a second device resumes the shift with no local state

## What's Stubbed

- No iOS platform layer (audio, FFI)
- No EMR/FHIR integration
- Real speech-to-text is not wired into the pipeline yet; Phase 2 uses `MockTranscriber`
- Real microphone capture is not wired into the pipeline yet; Phase 2 uses `MockAudioCapture`
- Transcript compaction / ephemeral mode (G7) and structured exam templates (G8) are deferred to later phases
- Audio upload (S3/SQS/worker) is Phase 4 Step 6; server-side STT and AI generation are Step 7 — today the summarizer still runs **inside the CLI**, and its output syncs up as an ordinary event
- No device offline queue: if the server is unreachable in remote mode, `EventBus::publish` fails and the command errors visibly (Phase 5 device work)
- No AWS deployment yet (Terraform/CI is Step 8); everything runs against docker-compose

## Completed: Phase 2 Audio Pipeline Wiring

Phase 2 connects the mock audio/STT layers to the event-sourced core:

```
MockAudioCapture -> MockTranscriber -> EncounterDriver -> EventBus -> SQLite + DashMap -> load_patient_note
```

### Working CLI Flow

The developer CLI starts with:

```bash
cargo run --bin er-cli
```

Supported working commands include:

- `new <chief complaint>`
- `list`
- `switch <index>`
- `start <ambient|dictation>`
- `stop` (auto-summarizes the encounter into HPI/Exam)
- `lab <name> <value> <unit>`
- `dispo <discharge|admit|transfer|ama|lwbs> ...`
- `med <home|given> <name> [dose] [route]`
- `dx <text> [--primary] [icd10]`
- `data <free text>`
- `mdm` (one-button MDM generation → Finalizing)
- `review`
- `sign`
- `note` (print the formatted note)
- `export` (plain-text note for clipboard/Epic)
- `reload`
- `status`
- `help`
- `quit`

The CLI persists event data to `./er_notes.db` and stores non-event-sourced shift metadata in `./shift.json`.

### Crash Recovery Coverage

`crates/er-app/tests/crash_recovery.rs` verifies:

- multi-encounter lifecycle survives dropping the in-memory DashMap and reloading from SQLite
- `switch_patient` closes the old patient's encounter, starts the new patient's encounter, and both patients reload correctly after simulated crash

## Completed: Phase 3 AI Note Generation + One-Button MDM

Phase 3 connects the AI layer to the event-sourced core so the note drafts its own prose, and delivers the headline one-button MDM. Implemented per `docs/planning/phase3.md` (Steps 0–8; Step 9 = these docs).

```
stop_encounter ─▶ TranscriptReady ─▶ [de-identify] ─▶ summarize_encounter ─▶ EncounterSummarized ─▶ note.hpi/exam
lab/med/dx/data/dispo ─▶ Result/Medication/Diagnosis/DataReviewed/Disposition events ─▶ note.{results,medication,diagnoses,data_reviewed,disposition}
mdm ─▶ assemble objective data ─▶ [de-identify] ─▶ generate_mdm ─▶ MdmGenerated ─▶ note.mdm ─▶ TriggerMDM (Idle→Finalizing)
```

### Key design points

- **Generic summarizer, not a trait object.** `Summarizer` is an async (RPITIT) trait, not `dyn`-compatible, so `ShiftManager<A, T, S: Summarizer>` threads it as a generic (mirroring `A`/`T`). `AppSummarizer` enum (`Claude`/`Mock`) lets `main` pick Claude when `ANTHROPIC_KEY` is set, else fall back to mock for offline runs.
- **Sync↔async bridge.** `ShiftManager` owns an `Arc<Runtime>` and `block_on`s only at the two AI call sites; the REPL stays synchronous.
- **De-identify before every Claude call.** `Deidentifier` strips PHI from the transcript (on `stop`) and the objective-data blob (on `mdm`) before any network call.
- **Best-effort summarization.** A summarization failure on `stop` is logged, not propagated — the transcript is already durably persisted, so the encounter is never lost and patient-switching stays robust.
- **Field-wise section merge.** `merge_sections` copies only HPI/ROS/Exam (and only when `Some`), so a re-eval encounter summarized *after* the MDM was generated can never clobber the MDM. Preserves the non-negotiable multi-encounter re-eval flow.
- **Persist content before advancing state.** `mdm` publishes `MdmGenerated` (while still `Idle`, snapshotted) *before* `TriggerMDM`, so a crash can't leave a `Finalizing` note with no MDM.
- **Guards.** `trigger_mdm` requires no active encounter + `Idle` phase, and refuses to call the model when HPI, objective data, and disposition are all empty.

### New events (folded in `apply_event`)

`EncounterSummarized`, `MdmGenerated`, `DispositionSet`, `MedicationRecorded`, `DiagnosisAssigned`, `DataReviewed`. All carry `clinical_occurred_at`, which `apply_event` uses for `updated_at`/audit/section-version timestamps so replay is deterministic.

### Standalone path

`cargo run --bin mdm_standalone` reads `<note> ---DATA--- <objective data> ---DISPO--- <disposition>` on stdin and prints a generated MDM — no audio, event sourcing, or FHIR (Dr. Joey F10).

### Test coverage

All automated tests use `MockSummarizer` (no network, deterministic). `crates/er-app/tests/generation_integration.rs` covers stop→HPI, multi-encounter merge, one-button MDM → Finalizing, the empty-note guard, and full crash-recovery (HPI/MDM/disposition/meds/diagnoses survive reload). Unit tests cover all five new `apply_event` arms, the MDM-preservation merge regression, post-Sign immutability, `AppSummarizer` dispatch, and `mdm_standalone`'s input split. Real-Claude runs (6b/7a) are manual.

## Completed: Phase 4 Steps 1–5 — Server, Auth, and the Sync Protocol

Implemented per `docs/planning/phase4.md` and, for Step 5, the S0–S11 breakdown in
`docs/planning/phase4-step5-implementation-steps.md`. Steps 6–10 (audio upload, server-side
AI, Terraform, hardening, load) are not started.

| Step | What landed |
|---|---|
| 1 | Workspace split into eight crates (Option 2), zero behavior change |
| 2 | `er-server` skeleton: axum, `/healthz`, config, tracing, Dockerfile, docker-compose (PG + MinIO + ElasticMQ) |
| 3 | `PostgresRepository` (sqlx + migrations), `event_id` on the event envelope, async `Repository` trait |
| 4 | Auth + tenancy: JWKS/JWT middleware, `orgs`/`clinicians`, org-scoped repository constructor |
| 5 | Core API + sync protocol + the remote client + CLI remote mode |

### The shape of Step 5

```
er-cli (remote mode)                        er-server
  ShiftManager / EventBus / load_patient_note
        │ Repository methods                    ┌─ auth middleware (JWT → clinician → org)
        ▼                                       │
  RemoteRepository ──▶ ServerApi ══ HTTPS ══▶ routes ──▶ PostgresRepository ──▶ Postgres
    (7 trait methods)   (URL + bearer +          │        (org-scoped by construction)
                         JSON + status)          └─ ownership guard: patient must be in your org
```

- **The wire contract is a crate** (`er-api`), not a document: server and client fail to compile
  if they disagree. DTOs are PHI-minimal *by construction* — `RegisterPatientRequest` has no
  field for a name, so a name cannot leak through it.
- **Idempotency is the sync design.** Client-stamped `event_id` (and `patient_id`) mean
  at-least-once delivery adds up to exactly-once storage: a resent batch returns the *original*
  `sequence_id`s and writes nothing new.
- **Cursors, not offsets.** `GET .../events?since=N&limit=M` means `sequence_id > N`;
  the client advances to the **last sequence actually received**, never `+1`, never the page
  length. `RemoteRepository` follows every page and returns the complete delta.
- **Tenancy fails closed in two layers:** the repository is constructed for one org and cannot
  express a cross-org query, and every per-patient route first checks the patient is registered
  in the caller's org. Missing and other-org are both plain `404` — no existence oracle.
- **Two failure classes stay distinct.** `StoreError::Backend` = no usable response (offline,
  DNS, timeout); `StoreError::Remote { status, message }` = the server answered and said no.
  The status must survive, because the same code means different things per endpoint: a snapshot
  `404` is the normal "no checkpoint, replay from zero", an event-upload `404` is a real error.
- **Snapshots are client-authored in Step 5** (flips to server-authored when the Step 6/7 worker
  projects notes). Checksums are computed server-side over the server's own serialization, so
  key-order differences can never raise a false corruption alarm. A downloaded snapshot **must**
  be `hydrate()`d — `PatientNote::state` is `#[serde(skip)]`, and a stateless note fails
  transitions *silently*.

### Remote CLI mode (S10)

`ER_SERVER_URL` + `ER_TOKEN` (both, non-blank) flip the CLI from local app to cloud client:

```bash
docker compose up -d postgres
cargo run -p er-server                                     # :8080
TOKEN=$(cargo run -q -p er-server --bin mint_dev_token -- token --sub <cognito_sub>)
ER_SERVER_URL=http://localhost:8080 ER_TOKEN=$TOKEN cargo run --bin er-cli
```

In remote mode the CLI opens **no SQLite database and no `shift.json`** — the server is the
shift memory, which is what lets a second device resume. Boot calls `GET /v1/shifts/active`
and falls back to `POST /v1/shifts`; quitting deliberately writes nothing, leaving the shift
open (ending it is an explicit `POST /v1/shifts/{id}/end`). Only the `new` command is
remote-aware, and it registers **server-first** so a failure can't leave a local ghost patient
whose every later append would 404. Everything else — `start`, `stop`, `lab`, `mdm`, `sign`,
`reload` — is untouched code that now crosses a network.

### Test coverage (Step 5)

- **One trait, three impls, one suite:** `repository_contract.rs` runs the same assertions
  against SQLite, Postgres, and `RemoteRepository` (real socket + auth + tenancy guard). The
  remote run injects a `PatientFactory` because the server adds a precondition the raw engines
  don't have: a patient must be registered before events are accepted.
- **`sync_gate.rs`** — protocol under fire: replayed batches, patient/path mismatch, oversized
  and empty batches, concurrent appends, cross-org probes, the full paging matrix, and
  `full_remote_lifecycle` (the Step 5 gate as a test: shift + register → full visit via
  `EventBus` over HTTP → device discarded → second client resumes, replays, reads the note and
  the export, ends the shift).
- **`remote_client.rs`** — what only the client can get wrong: snapshot-404-is-absence vs
  write-404-is-error, 401/409 keeping their status, unreachable server → `Backend`, the multi-page
  cursor loop, refusing to loop on a malformed page, snapshots arriving hydrated, the shift
  lifecycle via `ServerApi`, and the app layer running unchanged over HTTP.
- **`auth_gate.rs` / `read_gate.rs` / `snapshot_gate.rs` / `shift_gate.rs`** — Steps 3–5
  endpoint behavior. `common/mod.rs` holds the shared fixtures (`mint`, `seed_*`,
  `spawn_server`, `full_visit_events`).
- **Manual gate (E5), performed:** terminal A records and quits; terminal B, a different empty
  directory with the same token, boots to "Resumed shift / Patient records loaded: 1 / Notes
  reloaded from DB: 1" and prints the note. Neither directory gains a file.

Tests that touch Postgres need it running and `DATABASE_URL` exported:

```bash
docker compose up -d postgres
set -a && . ./.env && set +a && cargo test --workspace
```

## Completed: EventBus Persistence Wiring + Snapshots

Strategy B from `repository-architecture.md`, steps 4 and 5. Fully implemented and tested.
*(Historical record — the `src/...` paths below predate the Phase 4 Step 1 workspace split;
see *Project Layout* for where each file lives now. The design decisions still hold.)*

### Design Decision: Persist-First, Then Broadcast

`tokio::sync::broadcast` is lossy — if a subscriber falls behind, events are silently dropped. For medical records, this is unacceptable. Events must be written to SQLite BEFORE broadcasting to in-memory subscribers.

### Changes (4 files)

**1. `src/events.rs`**
- Add `Serialize, Deserialize` to `NoteEvent`
- Add `patient_id()` and `event_type()` helper methods on `NoteEvent`
- Add `Option<Arc<dyn Repository>>` to `EventBus`
- `publish()` persists to repository first, then broadcasts


**2. `src/repository.rs`**
- Remove default `todo!()` bodies — make methods required
- Add `save_snapshot()` and `get_latest_snapshot()` to trait
- Implement `SqliteRepository` struct wrapping `std::sync::Mutex<Connection>`
- `append_event()`: INSERT into events table (patient_id, event_type, payload JSON, occurred_at)
- `get_events()`: SELECT by patient_id ordered by sequence_id
- `get_events_since()`: SELECT where sequence_id > given value

**3. `src/db.rs`**
- Add `snapshots` table: `(patient_id TEXT PRIMARY KEY, sequence_id INTEGER, snapshot TEXT, created_at TEXT)`
- Update schema test to include snapshots table

**4. `src/state.rs`**
- Implement `replay(events)` — creates PatientNote, applies each event via handle_* methods
- Add `apply_event(note, event)` helper that maps NoteEvent variants to PatientNote mutations
- Add `load_patient_note(repo, patient_id)` — loads from snapshot + delta replay

### Snapshot Strategy

- Trigger snapshots on significant state transitions: Idle, Finalizing, Signed
- On load: get latest snapshot, replay only events after snapshot's sequence_id
- Fallback: if no snapshot exists, full replay from event 0

### Key Detail: std::sync::Mutex, Not tokio::sync::Mutex

rusqlite::Connection is !Sync. Use `std::sync::Mutex<Connection>` because SQLite operations are fast and we never hold the lock across an `.await` point.

## Doctor Feedback (2026-03-12)

ED physician validated the concept. Two key questions answered in `docs/product/doctor-feedback-responses.md`:
1. **Listening model**: Recommended patient-anchored quick switch (continuous shift recording, one-tap Apple Watch switch)
2. **EMR integration**: Clipboard MVP, FHIR V2, deep Epic V3

Product pitch document: `docs/product/product-summary.md`

## Project Layout (Cargo Workspace, Option 2)

Split from the layered `src/` tree into eight crates on 2026-06-14 (Phase 4 Step 1), per
`docs/planning/folder-structure-options.md`. The dependency rule is unchanged and now
compiler-enforced by the manifests: **arrows point inward only**. `er-domain` imports no
other crate and performs no I/O (no rusqlite/sqlx/reqwest/tokio/dashmap).

```
crates/
├── er-domain/     # pure core — no I/O, no other er-* crate
│   ├── note.rs encounter.rs patient.rs shift.rs result.rs disposition.rs audit.rs
│   ├── medication.rs diagnosis.rs
│   ├── events.rs  # NoteEvent enum + constructors (no EventBus here)
│   ├── state.rs   # NoteState trait, phase structs, ingest_result
│   └── replay.rs  # apply_event, replay
├── er-store/      # Repository trait + SqliteRepository + StoreError
│   └── repository.rs db.rs error.rs
├── er-ai/         # claude.rs summarizer.rs (+ AppSummarizer) deidentify.rs
├── er-pipeline/   # audio.rs stt.rs (mocks today)
├── er-app/        # orchestration: event_bus.rs load.rs shift_manager.rs
│   ├── encounter_driver.rs shift_snapshot.rs export.rs
│   ├── conversions/   # CLI arg → domain parsing
│   └── tests/         # crash_recovery, generation_integration, persistence_integration
├── er-api/        # THE WIRE CONTRACT (Phase 4 Step 5)
│   ├── types.rs   # DTOs — serde only, shared by both sides
│   └── client.rs  # ServerApi + RemoteRepository, behind feature = "client"
├── er-server/     # axum HTTP server (Phase 4 Steps 2–5)
│   ├── routes.rs error.rs state.rs config.rs repository.rs (PostgresRepository)
│   ├── auth/      # claims keys extractor middleware resolver
│   ├── migrations/  events_and_snapshots → tenancy → care_model
│   ├── bin/mint_dev_token.rs
│   └── tests/     # auth_gate read_gate snapshot_gate shift_gate sync_gate
│                  # repository_contract remote_client common/
└── er-cli/        # interactive developer CLI + standalone bins
    ├── main.rs interface/cli.rs
    └── bin/       # mdm_standalone.rs spike_summarizer.rs
dev/keys/          # dev RSA keypair + JWKS for offline auth
docs/              # architecture / product / planning documents
docker-compose.yml # postgres:5433, minio, elasticmq
```

Only `er-cli` and `er-server` are binaries; everything else is a library. `er-server` depends
on `er-api` for DTOs only — the `client` feature (reqwest, er-store) is enabled by `er-cli` and
by `er-server`'s dev-dependencies for tests, never by the shipped server binary.

## Key Files

| File | Purpose |
|------|---------|
| `crates/er-domain/src/note.rs` | PatientNote aggregate, NotePhase enum, transition methods |
| `crates/er-domain/src/state.rs` | State pattern, NoteState trait, ingest_result |
| `crates/er-domain/src/events.rs` | NoteEvent enum + constructors |
| `crates/er-domain/src/replay.rs` | apply_event, replay (pure event application) |
| `crates/er-domain/src/medication.rs` | Medication + MedicationKind, `summary_line()` |
| `crates/er-domain/src/diagnosis.rs` | Diagnosis, `summary_line()` |
| `crates/er-app/src/event_bus.rs` | EventBus (persist-first publish, projection, snapshots) |
| `crates/er-app/src/load.rs` | PatientNoteStore, load_patient_note (snapshot fast-path) |
| `crates/er-app/src/shift_manager.rs` | Multi-patient shift orchestration; `register_patient_with_id` for remote mode |
| `crates/er-app/src/encounter_driver.rs` | Mock audio/STT encounter orchestration |
| `crates/er-app/src/shift_snapshot.rs` | ShiftSnapshot for shift.json save/resume (local mode only) |
| `crates/er-app/src/export.rs` | Plain text note formatting |
| `crates/er-app/src/conversions/` | CLI arg → domain parsing (disposition, labs, medication, diagnosis) |
| `crates/er-store/src/repository.rs` | Repository trait, SqliteRepository (events + snapshots + checksum) |
| `crates/er-store/src/db.rs` | SQLite schema, open_db/open_memory_db |
| `crates/er-store/src/error.rs` | StoreError — note `Remote { status, message }` vs `Backend` |
| `crates/er-ai/src/claude.rs` | Real Claude API client for HPI/ROS/Exam/MDM |
| `crates/er-ai/src/summarizer.rs` | `Summarizer` trait, `MockSummarizer`, `AppSummarizer` enum |
| `crates/er-ai/src/deidentify.rs` | PHI stripping before API calls |
| `crates/er-api/src/types.rs` | Every request/response body (serde only), `NoteView::from` |
| `crates/er-api/src/client.rs` | `ServerApi` (HTTP mechanics) + `RemoteRepository` (the trait over HTTP) |
| `crates/er-server/src/routes.rs` | The whole v1 surface + `/healthz`; auth via `route_layer` |
| `crates/er-server/src/repository.rs` | PostgresRepository (org-scoped, advisory-locked append), patient/shift rows |
| `crates/er-server/src/error.rs` | ApiError → status + `{"error": code}`; PHI-free by type |
| `crates/er-server/src/auth/` | JWKS/JWT validation, clinician resolution, `require_auth` middleware |
| `crates/er-server/migrations/` | events+snapshots → tenancy (orgs/clinicians) → care_model (patients/shifts) |
| `crates/er-cli/src/main.rs` | CLI startup: local/remote swap, shift boot, command loop |
| `crates/er-cli/src/interface/cli.rs` | Command parsing and dispatch; `RemoteContext` for server-only ops |
| `crates/er-cli/src/bin/spike_summarizer.rs` | End-to-end Claude test (chest pain case) |
| `crates/er-cli/src/bin/mdm_standalone.rs` | Standalone MDM-from-text (stdin → MDM), Phase 3 Step 7 |
| `crates/er-app/tests/persistence_integration.rs` | Round-trip, snapshot, checksum, consistency |
| `crates/er-app/tests/crash_recovery.rs` | Phase 2 crash-recovery integration tests |
| `crates/er-app/tests/generation_integration.rs` | Phase 3 generation tests |
| `crates/er-server/tests/repository_contract.rs` | One suite × three backends (SQLite, Postgres, Remote) |
| `crates/er-server/tests/sync_gate.rs` | Sync protocol + `full_remote_lifecycle` (the Step 5 gate) |
| `crates/er-server/tests/remote_client.rs` | S9 client behavior: error classes, paging, hydrate, shifts |
| `crates/er-server/tests/common/mod.rs` | Shared fixtures: mint, seeds, spawn_server, full_visit_events |
| `docs/planning/phase4-step5-implementation-steps.md` | Step 5 S0–S11 with the concepts (§C1–§C9) |
| `docs/planning/phase4-step6-implementation-steps.md` | Step 6 S0–S9: audio upload, SQS worker, Deepgram STT (§C1–§C8) |
| `docs/product/doctor-feedback-responses.md` | Responses to ED doctor's questions |
| `docs/product/product-summary.md` | Product pitch document |
