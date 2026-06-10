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

## What's Stubbed

- No iOS platform layer (audio, FFI)
- No EMR/FHIR integration
- Real speech-to-text is not wired into the pipeline yet; Phase 2 uses `MockTranscriber`
- Real microphone capture is not wired into the pipeline yet; Phase 2 uses `MockAudioCapture`
- CLI `lab` and `note` handlers are still placeholders even though lower-level result/note pieces exist
- Summarization is not triggered by the encounter pipeline yet; Claude summarization exists as a separate component/spike

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
- `stop`
- `mdm`
- `review`
- `sign`
- `reload`
- `status`
- `help`
- `quit`

The CLI persists event data to `./er_notes.db` and stores non-event-sourced shift metadata in `./shift.json`.

### Crash Recovery Coverage

`src/tests/crash_recovery.rs` verifies:

- multi-encounter lifecycle survives dropping the in-memory DashMap and reloading from SQLite
- `switch_patient` closes the old patient's encounter, starts the new patient's encounter, and both patients reload correctly after simulated crash

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

## Key Files

| File | Purpose |
|------|---------|
| `src/state.rs` | State pattern, NoteState trait, replay, apply_event, load_patient_note, PatientNoteStore |
| `src/models/note.rs` | PatientNote aggregate, NotePhase enum, transition methods |
| `src/events.rs` | NoteEvent enum + constructors, EventBus (persist-first broadcast) |
| `src/repository.rs` | Repository trait, SqliteRepository (events + snapshots + checksum) |
| `src/tests/persistence_integration.rs` | Integration tests: round-trip, snapshot, checksum, consistency |
| `src/tests/crash_recovery.rs` | Phase 2 crash-recovery integration tests |
| `src/encounter_driver.rs` | Mock audio/STT encounter orchestration |
| `src/shift_manager.rs` | Multi-patient shift orchestration and CLI-facing manager API |
| `src/cli.rs` | CLI command parsing and dispatch |
| `src/main.rs` | Interactive developer CLI startup, command loop, shift save/resume |
| `src/db.rs` | SQLite schema, open_db/open_memory_db |
| `src/claude_summarizer.rs` | Real Claude API client for HPI/ROS/Exam/MDM |
| `src/export.rs` | Plain text note formatting |
| `src/deidentify.rs` | PHI stripping before API calls |
| `src/bin/spike_summarizer.rs` | End-to-end Claude test (chest pain case) |
| `docs/product/doctor-feedback-responses.md` | Responses to ED doctor's questions |
| `docs/product/product-summary.md` | Product pitch document |
