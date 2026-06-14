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
- Interactive developer CLI in `src/main.rs` with shift resume via `shift.json` and SQLite-backed note reload
- Crash-recovery integration tests for full lifecycle reload and mid-encounter patient switching
- **AI note generation wired into the live pipeline (Phase 3):** `stop_encounter` auto-summarizes the transcript into HPI/Exam (de-identified first); `trigger_mdm` is a one-button MDM generator that assembles results + meds + diagnoses + disposition and persists a billing-aware MDM
- **Async bridge:** `ShiftManager<A, T, S>` carries a `tokio::Runtime` and `block_on`s the async `Summarizer` at exactly two points (summarize on stop, generate MDM on trigger); CLI stays synchronous
- **Objective-data capture:** disposition, medications, diagnoses, and free-text "data reviewed" are event-sourced onto the note and feed the MDM
- **Standalone `mdm_standalone` bin:** paste note + chart data on stdin → generated MDM, no audio/event-sourcing/FHIR (Dr. Joey's F10 demo path)

## What's Stubbed

- No iOS platform layer (audio, FFI)
- No EMR/FHIR integration
- Real speech-to-text is not wired into the pipeline yet; Phase 2 uses `MockTranscriber`
- Real microphone capture is not wired into the pipeline yet; Phase 2 uses `MockAudioCapture`
- Transcript compaction / ephemeral mode (G7) and structured exam templates (G8) are deferred to later phases

## Completed: Phase 2 Audio Pipeline Wiring

Phase 2 connects the mock audio/STT layers to the event-sourced core:

```
MockAudioCapture -> MockTranscriber -> EncounterDriver -> EventBus -> SQLite + DashMap -> load_patient_note
```

### Working CLI Flow

The developer CLI starts with:

```bash
cargo run --bin er-note-app
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

`tests/crash_recovery.rs` verifies:

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

All automated tests use `MockSummarizer` (no network, deterministic). `tests/generation_integration.rs` covers stop→HPI, multi-encounter merge, one-button MDM → Finalizing, the empty-note guard, and full crash-recovery (HPI/MDM/disposition/meds/diagnoses survive reload). Unit tests cover all five new `apply_event` arms, the MDM-preservation merge regression, post-Sign immutability, `AppSummarizer` dispatch, and `mdm_standalone`'s input split. Real-Claude runs (6b/7a) are manual.

## Completed: EventBus Persistence Wiring + Snapshots

Strategy B from `repository-architecture.md`, steps 4 and 5. Fully implemented and tested.

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

## Project Layout (Layered, Option 1)

Reorganized 2026-06-10 per `docs/planning/folder-structure-options.md`. Four layers
under `src/`; the dependency rule is **arrows point inward only** — `domain/` imports
nothing from the other layers and performs no I/O (no rusqlite/reqwest/tokio/dashmap).
Each layer folder maps 1:1 to a future workspace crate (Option 2, planned after Phase 3).

```
src/
├── domain/          # pure core (future er-domain)
│   ├── note.rs encounter.rs patient.rs shift.rs result.rs disposition.rs audit.rs
│   ├── medication.rs diagnosis.rs   # Phase 3 objective-data models
│   ├── events.rs    # NoteEvent enum + constructors (no EventBus here)
│   ├── state.rs     # NoteState trait, phase structs, ingest_result
│   └── replay.rs    # apply_event, replay
├── application/     # orchestration (future er-app)
│   ├── event_bus.rs # persist-first publish, projection, snapshot trigger
│   ├── load.rs      # PatientNoteStore, load_patient_note
│   ├── shift_manager.rs encounter_driver.rs shift_snapshot.rs
│   ├── conversions/ # disposition.rs labs.rs medication.rs diagnosis.rs (CLI arg parsing)
├── infrastructure/  # outside world (future er-store / er-ai / er-pipeline)
│   ├── persistence/ # repository.rs, db.rs
│   ├── ai/          # claude.rs, summarizer.rs (+ AppSummarizer), deidentify.rs
│   ├── audio.rs stt.rs
├── interface/       # delivery (future er-cli)
│   ├── cli.rs export.rs
├── bin/             # spike_summarizer.rs, mdm_standalone.rs
├── error.rs main.rs
tests/               # integration tests — compile against the public API only
docs/                # architecture / product / planning documents
```

## Key Files

| File | Purpose |
|------|---------|
| `src/domain/note.rs` | PatientNote aggregate, NotePhase enum, transition methods |
| `src/domain/state.rs` | State pattern, NoteState trait, ingest_result |
| `src/domain/events.rs` | NoteEvent enum + constructors |
| `src/domain/replay.rs` | apply_event, replay (pure event application) |
| `src/domain/medication.rs` | Medication + MedicationKind, `summary_line()` |
| `src/domain/diagnosis.rs` | Diagnosis, `summary_line()` |
| `src/application/event_bus.rs` | EventBus (persist-first publish, projection, snapshots) |
| `src/application/load.rs` | PatientNoteStore, load_patient_note (snapshot fast-path) |
| `src/application/shift_manager.rs` | Multi-patient shift orchestration and CLI-facing manager API |
| `src/application/encounter_driver.rs` | Mock audio/STT encounter orchestration |
| `src/application/shift_snapshot.rs` | ShiftSnapshot for shift.json save/resume |
| `src/infrastructure/persistence/repository.rs` | Repository trait, SqliteRepository (events + snapshots + checksum) |
| `src/infrastructure/persistence/db.rs` | SQLite schema, open_db/open_memory_db |
| `src/infrastructure/ai/claude.rs` | Real Claude API client for HPI/ROS/Exam/MDM |
| `src/infrastructure/ai/summarizer.rs` | `Summarizer` trait, `MockSummarizer`, `AppSummarizer` enum |
| `src/infrastructure/ai/deidentify.rs` | PHI stripping before API calls |
| `src/application/conversions/` | CLI arg → domain parsing (disposition, labs, medication, diagnosis) |
| `src/interface/cli.rs` | CLI command parsing and dispatch |
| `src/interface/export.rs` | Plain text note formatting |
| `src/main.rs` | Interactive developer CLI startup, command loop, shift save/resume |
| `src/bin/spike_summarizer.rs` | End-to-end Claude test (chest pain case) |
| `src/bin/mdm_standalone.rs` | Standalone MDM-from-text (stdin → MDM), Phase 3 Step 7 |
| `tests/persistence_integration.rs` | Integration tests: round-trip, snapshot, checksum, consistency |
| `tests/crash_recovery.rs` | Phase 2 crash-recovery integration tests |
| `tests/generation_integration.rs` | Phase 3 generation tests: HPI summarization, one-button MDM, reload |
| `docs/product/doctor-feedback-responses.md` | Responses to ED doctor's questions |
| `docs/product/product-summary.md` | Product pitch document |
