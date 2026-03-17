# ER Note App - Claude Context

## Project Summary

Rust-based iOS ambient clinical documentation system for emergency departments. Uses Claude API for note summarization and MDM generation. Designed for non-linear, discontinuous ED encounters where a single patient visit involves 5-15 separate interactions across hours.

Key differentiator: **multi-encounter merging** — captures every interaction across a patient's entire ED stay and stitches them into one coherent note. Abridge/DAX record a single conversation; this records the full visit.

## Current Branch: `db-snapshot-mechanism`

## What's Working

- Domain model with PatientNote aggregate root, state machine (6 phases), event types
- Claude summarizer (Sonnet 4) with multi-encounter merging and MDM generation
- De-identification (regex-based PHI stripping before API calls)
- Plain text note export
- Serialization round-trip (hydrate test passes)
- SQLite schema with events table

## What's Stubbed

- `Repository` trait methods are `todo!()`
- `replay()` function in `state.rs` is `todo!()`
- EventBus publishes and persists before broadcasting
- No iOS platform layer (audio, FFI)
- No EMR/FHIR integration

## Next Implementation: EventBus Persistence Wiring + Snapshots

Strategy B from `repository-architecture.md`, steps 4 and 5. Plan is fully designed, ready to code.

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

ED physician validated the concept. Two key questions answered in `doctor-feedback-responses.md`:
1. **Listening model**: Recommended patient-anchored quick switch (continuous shift recording, one-tap Apple Watch switch)
2. **EMR integration**: Clipboard MVP, FHIR V2, deep Epic V3

Product pitch document: `product-summary.md`

## Key Files

| File | Purpose |
|------|---------|
| `src/state.rs` | State pattern, NoteState trait, replay (todo), PatientNoteStore |
| `src/models/note.rs` | PatientNote aggregate, NotePhase enum, transition methods |
| `src/events.rs` | NoteEvent enum, EventBus (broadcast channel) |
| `src/repository.rs` | Repository trait (todo stubs) |
| `src/db.rs` | SQLite schema, open_db/open_memory_db |
| `src/claude_summarizer.rs` | Real Claude API client for HPI/ROS/Exam/MDM |
| `src/export.rs` | Plain text note formatting |
| `src/deidentify.rs` | PHI stripping before API calls |
| `src/bin/spike_summarizer.rs` | End-to-end Claude test (chest pain case) |
| `repository-architecture.md` | Persistence strategy analysis (Strategy B = event-sourced) |
| `doctor-feedback-responses.md` | Responses to ED doctor's questions |
| `product-summary.md` | Product pitch document |
