# Phase 2 — Audio Pipeline Wiring: Implementation Plan

*2026-04-12 | Depends on: Phase 1 (complete) | Feeds into: Phase 3 (whisper-rs drop-in)*

---

## What This Phase Accomplishes

Phase 2 connects the mock audio and STT layers to the event-sourced core so that a simulated encounter flows end-to-end:

```
MockAudioCapture → MockTranscriber → EventBus::publish → SQLite + DashMap → load_patient_note → verify
```

Today, every component exists in isolation. `MockAudioCapture` can start/stop but never publishes events. `MockTranscriber` returns canned text but nothing feeds it audio. `EventBus` persists and projects but nothing drives it except hand-written test code. `main.rs` calls `handle_audio_started()` directly on the model — bypassing persistence entirely.

After Phase 2, a single `EncounterDriver` orchestrates the full pipeline, and `main.rs` becomes an interactive CLI where you type commands like `start chest-pain` and watch a real event stream flow through SQLite, get projected into the DashMap, and produce a reconstructable clinical note.

**Why before iOS (Phase 4):** The CLI workflow is where we catch orchestration design mistakes — wrong event ordering, missing encounter IDs, timing bugs — before they cost FFI rework. Phase 3 (whisper-rs) then becomes a drop-in replacement for `MockTranscriber` with zero orchestration changes.

---

## Table of Contents

1. [Prerequisites and Current State](#1-prerequisites-and-current-state)
2. [Architecture Overview](#2-architecture-overview)
3. [Step-by-Step Implementation](#3-step-by-step-implementation)
   - Step 1: EncounterDriver struct
   - Step 2: start_encounter method
   - Step 3: stop_encounter method (audio → STT → events)
   - Step 4: Active patient context + switch_patient
   - Step 5: ShiftManager (multi-patient orchestrator)
   - Step 6: Interactive CLI in main.rs
   - Step 7: Crash-recovery integration test
   - Step 8: Multi-encounter summarization wiring
   - Step 9: Cleanup and documentation
4. [Acceptance Criteria](#4-acceptance-criteria)
5. [Files Changed](#5-files-changed)
6. [Risks and Decisions](#6-risks-and-decisions)
7. [What Phase 2 Does NOT Do](#7-what-phase-2-does-not-do)

---

```
Order of Operations

Step  Action                                              Gate
────  ──────────────────────────────────────────────────  ──────────────
1     Create EncounterDriver struct + new()               cargo check
2     Implement start_encounter                           cargo check
2a      Add unit test: start publishes AudioStarted       test passes
2b      Add unit test: double-start returns error         test passes
3     Implement stop_encounter                            cargo check
3a      Add unit test: stop publishes AudioStopped +      test passes
        TranscriptReady in order
3b      Add unit test: stop-without-start returns error   test passes
4     Implement switch_patient                            cargo check
4a      Add unit test: switch stops old, starts new       test passes
5     Create ShiftManager struct + commands               cargo check
5a      Add unit test: register + start + stop lifecycle  test passes
6     Rewrite main.rs as interactive CLI                  cargo run works
6a      Test: new → start → stop → note                  manual verify
6b      Test: reload command reconstructs from SQLite     manual verify
7     Add crash_recovery integration tests                cargo test passes
8     (Optional) Wire summarization                       tests pass
9     Update CLAUDE.md + cleanup                          docs accurate

```

## 1. Prerequisites and Current State

### Phase 1 delivered (verified):

- `apply_event()` and `replay()` reconstruct a `PatientNote` from events
- `load_patient_note()` with snapshot fast-path and full-replay fallback
- `EventBus::publish(event, &store)` persists to SQLite, projects to DashMap, triggers snapshots on Idle/Finalizing/Signed
- 29 tests passing, zero warnings
- Checksum validation on snapshots with corruption fallback

### Existing traits that Phase 2 wires together:

| Trait | File | Current Impl | What it does |
|-------|------|-------------|-------------|
| `AudioCapture` | `src/audio.rs` | `MockAudioCapture` | start/stop recording, returns raw PCM bytes |
| `Transcriber` | `src/stt.rs` | `MockTranscriber` | Takes PCM bytes, returns `TranscriptResult` with text |
| `Summarizer` | `src/summarizer.rs` | `MockSummarizer` | Takes transcript text + prior sections, returns updated `NoteSections` |
| `Repository` | `src/repository.rs` | `SqliteRepository` | Event append, query, snapshot save/load |

### Key types involved:

| Type | File | Role |
|------|------|------|
| `EventBus` | `src/events.rs` | Persist-first event publishing + DashMap projection |
| `PatientNoteStore` | `src/state.rs` | `Arc<DashMap<PatientId, PatientNote>>` — in-memory read model |
| `Shift` | `src/models/shift.rs` | Container for a clinical shift's patient list |
| `PatientRecord` | `src/models/patient.rs` | Minimal patient demographics + chief complaint |
| `Encounter` | `src/models/encounter.rs` | A single recording session for one patient |
| `CaptureMode` | `src/models/encounter.rs` | `Ambient` or `Dictation` |

---

## 2. Architecture Overview

### The orchestration gap

Right now, the code has producers (audio, STT) and consumers (EventBus, repository, DashMap) but no middle layer connecting them. Phase 2 introduces two new structs:

```
┌──────────────────────────────────────────────────────────────────────┐
│                          ShiftManager                                │
│  Owns: Shift, Vec<PatientRecord>, active_patient_id                 │
│  Commands: register_patient, set_active_patient, end_shift          │
│                                                                      │
│  ┌────────────────────────────────────────────────────────────────┐  │
│  │                      EncounterDriver                           │  │
│  │  Owns: AudioCapture, Transcriber, EventBus ref, Store ref     │  │
│  │  Commands: start_encounter, stop_encounter                     │  │
│  │                                                                │  │
│  │  start_encounter(patient_id, mode):                            │  │
│  │    1. Generate encounter_id = Uuid::new_v4()                   │  │
│  │    2. audio.start(patient_id, encounter_id, mode)              │  │
│  │    3. bus.publish(AudioStarted { ... }, &store)                │  │
│  │                                                                │  │
│  │  stop_encounter():                                             │  │
│  │    1. pcm = audio.stop()                                       │  │
│  │    2. bus.publish(AudioStopped { ... }, &store)                │  │
│  │    3. result = transcriber.transcribe(&pcm)                    │  │
│  │    4. bus.publish(TranscriptReady { text, ... }, &store)       │  │
│  └────────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────────┘
```

### Why two structs instead of one

`EncounterDriver` is stateless about patients — it takes a `patient_id` parameter and orchestrates one encounter. It doesn't know which patients exist or which one is "active." That's `ShiftManager`'s job. This separation matters because:

1. **Testability.** `EncounterDriver` can be tested with a single fake patient. `ShiftManager` tests patient switching, which is the hard part.
2. **Phase 5 compatibility.** The Apple Watch quick-switch feature calls `ShiftManager::set_active_patient()`, which internally calls `EncounterDriver::stop_encounter()` for the old patient and `start_encounter()` for the new one. If encounter logic and patient-context logic were mixed, this would be a tangled mess.
3. **Single responsibility.** The driver knows *how* to run an encounter. The manager knows *which* patient is active. Neither needs to know the other's internals.

---

## 3. Step-by-Step Implementation

### Step 1: Create `src/encounter_driver.rs` — The EncounterDriver struct

**Purpose:** This is the core orchestrator that turns audio capture into events. It holds references to the audio layer, the STT layer, and the event bus, and exposes two commands: `start_encounter` and `stop_encounter`.

**File:** `src/encounter_driver.rs` (new file)

**Struct definition:**

```rust
pub struct EncounterDriver<A: AudioCapture, T: Transcriber> {
    audio: A,
    transcriber: T,
    bus: Arc<EventBus>,
    store: PatientNoteStore,
    active_encounter: Option<ActiveEncounter>,
}

/// Tracks the in-flight encounter so stop_encounter knows what to close.
struct ActiveEncounter {
    patient_id: PatientId,
    encounter_id: EncounterId,
    started_at: DateTime<Utc>,
}
```

**Why generics (`A: AudioCapture, T: Transcriber`) instead of `dyn` trait objects:**

- In Phase 2, the concrete types are `MockAudioCapture` and `MockTranscriber`. In Phase 3, they become `WhisperTranscriber`. In Phase 4, the audio impl is a Swift-FFI bridge. Generics let each phase swap implementations at compile time with zero boxing overhead. For a real-time audio pipeline on a mobile device, avoiding `Box<dyn>` dispatch on the hot path matters.
- The `EventBus` is behind `Arc` because it's shared with the `ShiftManager` and the CLI loop. The bus itself is not generic — it's always the same type.

**Why `active_encounter` is an `Option`:**

- The driver can only have one encounter in flight at a time. If you try to start a second encounter without stopping the first, that's an error. The `Option` makes this state machine explicit — `None` means idle, `Some` means recording.
- This is simpler and safer than a boolean `is_recording` flag plus separate patient_id/encounter_id fields that could get out of sync.

**Relationship to existing code:**

- `AudioCapture` trait is already defined in `src/audio.rs:9`
- `Transcriber` trait is already defined in `src/stt.rs:8`
- `EventBus` is in `src/events.rs:95`
- `PatientNoteStore` is in `src/state.rs:215`

**What to add to `src/lib.rs`:** `pub mod encounter_driver;`

---

### Step 2: Implement `EncounterDriver::start_encounter`

**Purpose:** Begin recording audio for a patient and publish the `AudioStarted` event into the event-sourced pipeline.

**Method signature:**

```rust
pub fn start_encounter(
    &mut self,
    patient_id: PatientId,
    mode: CaptureMode,
) -> AppResult<EncounterId>
```

**Implementation logic:**

```
1. Guard: if self.active_encounter.is_some(), return Err(AppError::Audio("encounter already in progress"))
2. Generate encounter_id = Uuid::new_v4()
3. Call self.audio.start(patient_id, encounter_id, mode.clone())?
4. Publish: self.bus.publish(NoteEvent::audio_started(patient_id, encounter_id, mode), &self.store)?
5. Set self.active_encounter = Some(ActiveEncounter { patient_id, encounter_id, started_at: Utc::now() })
6. Return Ok(encounter_id)
```

**Why publish AFTER audio.start, not before:**

- If audio capture fails to start (microphone permissions denied, hardware error), we don't want an `AudioStarted` event in the log for an encounter that never actually started. The event stream must reflect reality.
- The sequence is: hardware starts → event is published → DashMap is updated → snapshot is considered. Each step depends on the previous succeeding.

**Why return the `EncounterId`:**

- The caller (ShiftManager or CLI) may need to display it, log it, or pass it to other systems. Generating it inside the driver and returning it keeps ID generation centralized — no risk of the caller generating one ID and the event carrying a different one.

**Relationship to Phase 1:**

- `NoteEvent::audio_started()` is the convenience constructor we added in Phase 1 (`src/events.rs`). It auto-fills `clinical_occurred_at: Utc::now()`.
- `EventBus::publish()` is the persist-first method from Phase 1 that writes to SQLite, then projects to the DashMap, then triggers a snapshot if the phase lands on Idle/Finalizing/Signed.

---

### Step 3: Implement `EncounterDriver::stop_encounter`

**Purpose:** Stop recording, run speech-to-text on the captured audio, and publish both `AudioStopped` and `TranscriptReady` events.

**Method signature:**

```rust
pub fn stop_encounter(&mut self) -> AppResult<EncounterId>
```

**Implementation logic:**

```
1. Guard: take self.active_encounter. If None, return Err(AppError::Audio("no encounter in progress"))
2. Let active = self.active_encounter.take().unwrap()
3. Call pcm_bytes = self.audio.stop()?
4. Publish: self.bus.publish(NoteEvent::audio_stopped(active.patient_id, active.encounter_id), &self.store)?
   — This transitions the state machine from Encountering → Idle and triggers a snapshot
5. Call transcript_result = self.transcriber.transcribe(&pcm_bytes)?
6. Publish: self.bus.publish(NoteEvent::transcript_ready(
       active.patient_id,
       active.encounter_id,
       transcript_result.text,
       Utc::now(),
   ), &self.store)?
7. Return Ok(active.encounter_id)
```

**Why AudioStopped publishes BEFORE transcription:**

- Transcription takes time (even the mock returns instantly, but real Whisper in Phase 3 takes seconds). The state machine should transition to `Idle` the moment the clinician stops talking, not after transcription finishes. This is clinically correct — the doctor may walk away to see another patient while transcription is still running.
- The event ordering is `AudioStarted → AudioStopped → TranscriptReady`. During replay, this means the encounter gets its `ended_at` timestamp before the transcript arrives. That's correct — the encounter ended, then the transcript was produced.

**Why `self.active_encounter.take()` (move out of Option):**

- After stop, there's no active encounter. Taking it out of the Option enforces this at the type level. Any subsequent call to `stop_encounter` without a `start_encounter` in between will hit the None guard.

**Relationship to the DashMap projection:**

- `AudioStopped` publishes → `apply_event` runs inside `EventBus::publish` → state transitions from Encountering to Idle → the Idle phase triggers a snapshot write to SQLite. This all happens synchronously before `stop_encounter` continues to transcription.
- `TranscriptReady` publishes → `apply_event` attaches the transcript text to the encounter in the DashMap's copy of `PatientNote`. No state transition (still Idle). No snapshot trigger (Idle already snapshotted after AudioStopped). This is efficient — the transcript lands in the event log for replay, and in the DashMap for UI reads, but doesn't cause redundant snapshot writes.

**Error handling philosophy:**

- If `audio.stop()` fails, the encounter is already taken out of `active_encounter`. We're in a bad state — audio hardware errored. Return the error; the caller (ShiftManager or CLI) must decide whether to retry or surface an alert. We do NOT publish AudioStopped for an encounter whose audio capture failed to stop cleanly.
- If transcription fails, AudioStopped has already been published (the encounter is closed). We should still propagate the error, but the encounter is in a valid state — it just has no transcript. A future retry mechanism could re-transcribe from the raw audio.

---

### Step 4: Active patient context + `switch_patient`

**Purpose:** When a clinician taps the Apple Watch to switch patients (Phase 5), the system needs to stop the current encounter, flush its audio/transcript, and start a new encounter for the new patient — all atomically from the clinician's perspective.

In Phase 2, we implement this at the `EncounterDriver` level so the wiring is tested before any iOS work begins.

**Method signature:**

```rust
pub fn switch_patient(
    &mut self,
    new_patient_id: PatientId,
    mode: CaptureMode,
) -> AppResult<EncounterId>
```

**Implementation logic:**

```
1. If self.active_encounter.is_some():
   a. self.stop_encounter()?   — closes old patient's encounter, publishes AudioStopped + TranscriptReady
2. self.start_encounter(new_patient_id, mode)  — starts new patient's encounter, publishes AudioStarted
3. Return the new encounter_id
```

**Why this is just stop + start:**

- `AudioCapture::switch_patient` exists in the trait (src/audio.rs:24) and returns the previous patient's audio chunk. But for the event-sourced pipeline, we need two distinct event sequences — one for the closing encounter and one for the opening encounter. The simplest correct implementation is to stop and start. The `AudioCapture::switch_patient` method can be used internally by the hardware layer (Phase 4) to avoid dropping audio samples during the switch, but the event layer sees two atomic operations.

**Relationship to Phase 5 (Apple Watch quick-switch):**

- Phase 5's `ShiftManager::set_active_patient(new_id)` will call `EncounterDriver::switch_patient(new_id, mode)`. The Watch UI handles patient selection; the Rust layer handles the event atomics. By implementing and testing `switch_patient` now with mocks, we validate the event ordering before it touches real hardware.

---

### Step 5: Create `src/shift_manager.rs` — The ShiftManager

**Purpose:** Manages the multi-patient context for an ED shift. Knows which patients exist, which one is "active" (currently being seen), and delegates encounter operations to the `EncounterDriver`.

**File:** `src/shift_manager.rs` (new file)

**Struct definition:**

```rust
pub struct ShiftManager<A: AudioCapture, T: Transcriber> {
    shift: Shift,
    patients: HashMap<PatientId, PatientRecord>,
    active_patient_id: Option<PatientId>,
    driver: EncounterDriver<A, T>,
    bus: Arc<EventBus>,
    store: PatientNoteStore,
}
```

**Commands:**

| Method | What it does | Events published |
|--------|-------------|-----------------|
| `register_patient(cc: &str) -> PatientId` | Creates a `PatientRecord`, adds to shift, inserts an empty `PatientNote` into the store | None (registration is a local operation, not an event — the patient exists in the store but has no encounter history yet) |
| `start_encounter(mode) -> EncounterId` | Guards that `active_patient_id` is set, delegates to `driver.start_encounter` | `AudioStarted` |
| `stop_encounter() -> EncounterId` | Delegates to `driver.stop_encounter` | `AudioStopped`, `TranscriptReady` |
| `set_active_patient(id) -> ()` | If an encounter is in progress for the old patient, stops it first. Sets the new active patient. Does NOT auto-start an encounter — the clinician must explicitly start recording. | `AudioStopped` + `TranscriptReady` (if an encounter was in progress) |
| `add_lab(result) -> ()` | Publishes a `ResultReceived` event for the active patient | `ResultReceived` |
| `trigger_mdm() -> ()` | Publishes `TriggerMDM` for the active patient | `TriggerMDM` |
| `review_ready() -> ()` | Publishes `ReviewReady` for the active patient | `ReviewReady` |
| `sign_note() -> ()` | Publishes `Sign` for the active patient | `Sign` |
| `get_note(patient_id) -> Option<PatientNote>` | Reads from the DashMap store | None (read-only) |
| `list_patients() -> Vec<(PatientId, &str, NotePhase)>` | Returns patient ID, chief complaint, and current phase for all patients | None (read-only) |
| `end_shift() -> ()` | Stops any active encounter, marks shift as ended | Conditional |

**Why `register_patient` doesn't publish an event:**

- In the current event model, there's no `PatientRegistered` event variant. The patient's existence is tracked by the `Shift` and `PatientRecord` models, which are not event-sourced (they're simple in-memory state). The first event for a patient is `AudioStarted`, which triggers `PatientNote::new()` via the DashMap `or_insert_with` in `EventBus::publish`.
- This is a deliberate simplification for the MVP. A production system would likely event-source patient registration too, but that's a Phase 6+ concern (FHIR integration creates patients from the EMR).

**Why `set_active_patient` doesn't auto-start recording:**

- The doctor feedback document (2026-03-12) describes "one-tap switch" which implies starting recording immediately. But that's a Phase 5 UX decision. At the pipeline wiring level, switching context and starting recording are separate actions. This lets the CLI test both behaviors: switch-then-start and switch-without-start (e.g., switching to a patient to check their note without recording).

---

### Step 6: Transform `main.rs` into an interactive CLI

**Purpose:** Turn the current hardcoded demo into a command-driven CLI that exercises the full pipeline. This is the primary manual testing tool for Phase 2 and the template for Phase 4's iOS debug interface.

**Current state of `main.rs`:** Calls `handle_audio_started()` directly on the model, bypassing EventBus entirely. Creates a shift and patient but never persists events. Runs once and exits.

**New `main.rs` structure:**

```
fn main():
  1. Initialize tracing
  2. Open SQLite (file-based for persistence across runs, not in-memory)
  3. Create SqliteRepository + EventBus + PatientNoteStore
  4. Create ShiftManager<MockAudioCapture, MockTranscriber>
  5. Print welcome message with available commands
  6. Enter command loop (read stdin line by line)
  7. Parse each line into a Command enum
  8. Execute command via ShiftManager
  9. Print result
```

**Command set:**

```
Available commands:
  new <chief-complaint>    Register a new patient (e.g., "new chest pain")
  list                     Show all patients with phase
  switch <patient-index>   Set active patient by list index (1-based)
  start [ambient|dictation] Start recording for active patient
  stop                     Stop recording (runs STT, publishes transcript)
  lab <name> <value> <unit> Add a lab result (e.g., "lab Troponin 0.04 ng/mL")
  mdm                      Trigger MDM generation
  review                   Mark note ready for review
  sign                     Sign the note
  note                     Print the current note for active patient
  export                   Export note as plain text (Epic clipboard format)
  reload                   Cold-reload active patient from SQLite (tests persistence)
  status                   Show EventBus stats (events published, snapshots written)
  quit                     End shift and exit
```

**Why `reload` command:**

- This is the crash-simulation feature from the roadmap. It drops the in-memory `PatientNote` from the DashMap, then calls `load_patient_note(repo, patient_id)` to reconstruct from SQLite. If the note comes back identical, persistence is working correctly. This is the interactive equivalent of the integration test in Step 7.

**Why no `rustyline` or other line-editing dependency:**

- Phase 2 is about pipeline wiring, not CLI UX. `std::io::stdin().read_line()` is sufficient. Adding `rustyline` (with its `libc` and `nix` transitive deps) would bloat the dependency tree for a feature that gets replaced by SwiftUI in Phase 4. If CLI polish becomes important, it can be added later as a dev dependency.

**Relationship to existing `main.rs`:**

- The current `main.rs` is replaced entirely. Its code (create shift, create patient, exercise state machine, verify serialization) is subsumed by the CLI commands. The serialization round-trip verification is now the `reload` command.

**Database location:**

- Use a file-based SQLite database at a well-known path (e.g., `./er_notes.db`) instead of in-memory. This lets you quit the CLI, restart it, and find your patients still there via `load_patient_note`. This is the first time the app has cross-session persistence.
- On startup, check if the database exists. If it does, attempt to load the previous shift's patients into the store using `load_patient_note` for each known patient. If it doesn't, start fresh.

**Saving the shift/patient registry:**

- The `Shift` and `PatientRecord` structs are not event-sourced. For Phase 2, serialize them to a `shift.json` file alongside the SQLite database. On startup, if `shift.json` exists, deserialize it and reload each patient's note from SQLite. This is a pragmatic shortcut — event-sourcing the shift itself is not needed until Phase 6.

---

#### Step 6 — Detailed Implementation Logic

The sections above describe *what* the CLI does. The sections below describe *how*, broken into the startup flow, the command loop, and each command handler. Pseudocode only; the actual Rust translation should be straightforward once the logic is settled.

##### A. `main()` startup flow

###### A1. Initialize tracing
Set up `tracing_subscriber` with a default filter (probably `info` level). This gives you a single place to enable verbose event logging later by setting `RUST_LOG=debug`. Do this first so any setup errors are logged through the same channel.

###### A2. Decide on the database path
Per the plan: a file-based SQLite database, not in-memory. The plan suggests `./er_notes.db` or `./er_notes_<shift_id>.db`. For Phase 2, keep it simple — a fixed `./er_notes.db` in the current working directory. The shift-id-suffix variation matters only if you want multiple parallel shifts on the same machine; defer that until it actually causes a problem.

###### A3. Open SQLite
Call `db::open_db("./er_notes.db")` (the file-based equivalent of `open_memory_db`). This runs the schema migration if the file is new and validates the schema if it already exists. The function returns `AppResult<Connection>`; on failure, the error is already an `AppError` (typically `AppError::Db(rusqlite::Error)`). Handle it with an explicit match + `eprintln!` + `std::process::exit(1)` so the user gets a clean one-line message (`thiserror`'s `Display` impl) rather than a `Debug` dump. Use `eprintln!` to keep stdout clean for actual data output. Consider including a hint such as "check the current directory is writable."

###### A4. Build the persistence + event stack
In order:
1. Wrap the connection in `SqliteRepository::new(connection)`.
2. Box it in an `Arc<dyn Repository>`.
3. Construct `EventBus::new(1024, Some(repo.clone()))` — the existing constructor at `src/events.rs:133`. The first arg is the broadcast channel capacity (any round number is fine — there are no subscribers in the CLI). The second arg is the optional repository; always `Some(...)` in the CLI since we always want persistence.
4. Wrap the bus in `Arc<EventBus>`. Both `ShiftManager` and `EncounterDriver` hold `Arc<EventBus>`, so the bus must be shared.
5. Create an empty `PatientNoteStore` via `state::new_store()`.

Order matters because the bus needs the repo, and the store is a separate concern that lives alongside.

###### A5. Build the mocks
Instantiate `MockAudioCapture` and `MockTranscriber`. These have no configuration — just defaults. Pass them into a new `EncounterDriver::new(audio, transcriber, bus.clone(), store.clone())`.

###### A6. Determine whether we're starting fresh or resuming
Check if `./shift.json` exists: 
  - Using std::path

- **Exists:** read it, deserialize into a `Shift` and `Vec<PatientRecord>`. For each patient record, call `load_patient_note(repo, patient_id)` to reconstruct the note from SQLite and insert it back into the `PatientNoteStore`. The active patient context (`active_patient_id`) is intentionally NOT persisted — the clinician should re-select on resume, both for safety (confirms intent) and simplicity.
- **Doesn't exist:** create a fresh `Shift::new()` and empty patient list.

Build the `ShiftManager` from these pieces: `shift`, the rebuilt `patients` HashMap, `active_patient_id: None`, the driver, the bus, the store.

###### A7. Print the welcome banner
Show the version, the database path, the count of patients reloaded (if any), and the list of available commands. Make it obvious whether this is a fresh shift or a resumed one.

###### A8. Enter the command loop
Loop forever (until the user types `quit`):
1. Print a prompt — something like `> ` or `[patient name]> ` if an active patient is set.
2. Read one line from stdin using `std::io::stdin().read_line(...)`.
3. If EOF (Ctrl-D), treat it like `quit`.
4. Trim whitespace.
5. If the line is empty, loop back.
6. Parse the line into a `Command` enum (see section B).
7. Dispatch to the appropriate handler.
8. Print the handler's result — either a success message or the formatted error.

###### A9. On quit
Before exiting:
1. If an encounter is in progress, call `manager.stop_encounter()` to flush audio and transcript cleanly. Ignore errors here but log them — we're shutting down.
2. Serialize the current `Shift` and `Vec<PatientRecord>` to `./shift.json` so the next run can resume.
3. Drop the manager (which drops the SQLite connection — WAL files get checkpointed).
4. Exit with code 0.

##### B. The `Command` enum and parsing

Define a `Command` enum with one variant per supported command:

```
Command::New(chief_complaint: String)
Command::List
Command::Switch(index: usize)
Command::Start(mode: CaptureMode)
Command::Stop
Command::Lab { name, value, unit }
Command::Mdm
Command::Review
Command::Sign
Command::Note
Command::Export
Command::Reload
Command::Status
Command::Quit
Command::Unknown(line: String)  // for error messages
```

Parsing happens in one function — `parse_command(line: &str) -> Command`. The logic:

1. Split the line by whitespace into a `Vec<&str>`.
2. Match on the first token:
   - `"new"` → `Command::New(rest_joined_by_space)`. Empty rest → return an error variant or a `Command::Unknown` with a helpful message.
   - `"list"` → `Command::List`.
   - `"switch"` → parse second token as `usize`. On failure, `Command::Unknown`.
   - `"start"` → parse second token: `"ambient"` (default if missing) → `CaptureMode::Ambient`; `"dictation"` → `CaptureMode::Dictation`. Anything else → unknown.
   - `"stop"` → `Command::Stop`.
   - `"lab"` → expect 3 args (name, value, unit). Parse value as `f64`. Missing or unparseable → unknown.
   - `"mdm"`, `"review"`, `"sign"`, `"note"`, `"export"`, `"reload"`, `"status"`, `"quit"` → their respective variants, no args.
   - Anything else → `Command::Unknown`.

Keep parsing dumb. It produces a `Command` or an error variant; the dispatcher decides what to do with it. No business logic in the parser.

##### C. Per-command handler logic

Each handler takes `&mut ShiftManager` and the command's payload. Each prints a one-line success message or a formatted error.

###### `new <chief-complaint>`
1. Call `manager.register_patient(chief_complaint)` — returns the new `PatientId`.
2. Print: `"Registered patient <uuid> with chief complaint '<cc>'"`.
3. Print: `"Tip: type 'switch <index>' or 'list' to see the patient index"`.

Does NOT auto-set the patient as active. Forces an explicit `switch` step, which is consistent with the plan's "switching context and starting recording are separate actions" philosophy.

###### `list`
1. Call `manager.list_patients()` — returns the `Vec<(PatientId, String, NotePhase)>`.
2. For each entry, print a numbered row: `"1. <chief complaint> [<phase>] (<short uuid>)"`.
3. If the patient at that row is the active patient, prefix with `"*"` or append `"(active)"`.
4. If the list is empty, print `"No patients registered. Use 'new <chief complaint>' to register one."`.

The 1-based index is the index used by `switch`. Cache the index→patient_id mapping somewhere the CLI can reach for the next `switch` call — either by re-running `list_patients` inside `switch` and matching by position, or by storing the last shown index list in a small struct.

###### `switch <index>`
1. Call `manager.list_patients()` again to get the current order (or use the cached list from the last `list` call).
2. Convert the 1-based index to 0-based. Bounds-check.
3. Pull the `PatientId` from row `index - 1`.
4. Call `manager.set_active_patient(patient_id)?`.
5. Print: `"Active patient: <chief complaint> (<short uuid>)"`.

Out-of-range index → print "No patient at index N. Use 'list' to see valid indices."

###### `start [ambient|dictation]`
1. Default mode to `CaptureMode::Ambient` if not provided.
2. Call `manager.start_encounter(mode)?` — returns `EncounterId`.
3. Print: `"Started <mode> encounter <short uuid>"`.

Errors bubble up naturally — `NoActivePatient`, `encounter already in progress`, etc.

###### `stop`
1. Call `manager.stop_encounter()?` — returns the `EncounterId` that was just closed.
2. Print: `"Stopped encounter <short uuid>. Transcript captured."`.

The mock transcriber returns instantly, so the publish chain (AudioStopped → TranscriptReady) is fast. In Phase 3 this print will arrive after a multi-second pause; that's fine.

###### `lab <name> <value> <unit>`
This is the most parsing-heavy command. The handler:
1. Assemble a `ClinicalResult` with `source: ResultSource::Manual`, `id: Uuid::now_v7()`, `patient_id: <active id from manager>`, `timestamp: Utc::now()`, and `result_type: ResultType::Lab(LabResult { display_name: name, value, unit, ... })` with sensible defaults for the optional fields (`fhir_id: None`, `loinc_code: None`, `reference_range: None`, `is_abnormal: false`).
2. Call `manager.add_lab(result, Utc::now())?`.
3. Print: `"Added lab: <name> = <value> <unit>"`.

Getting the active patient id for step 1 is slightly awkward because the manager doesn't currently expose it. Two options:
- Add a `manager.active_patient_id() -> Option<PatientId>` accessor.
- Have the CLI pass a dummy id and let the manager replace it (worse — couples the manager's internals to the CLI's assumptions).

The accessor is cleaner. Add it.

###### `mdm`
1. Call `manager.trigger_mdm()?`.
2. Print: `"MDM triggered. Note phase: Finalizing."`.

###### `review`
1. Call `manager.review_ready()?`.
2. Print: `"Note marked ready for review. Phase: PendingSignOff."`.

###### `sign`
1. Call `manager.sign()?`.
2. Print: `"Note signed. Phase: Signed (immutable)."`.

###### `note`
1. Get the active patient id (or print "No active patient" and return).
2. Call `manager.get_note(patient_id)?` — returns `Option<PatientNote>`.
3. If `None`, print "No note found." (shouldn't happen for a registered patient, but defensive).
4. If `Some(note)`, print a formatted multi-line summary: phase, encounter count, transcript snippets, current HPI/Exam/MDM sections, lab results, audit log size.

For Phase 2 you can keep formatting minimal — `Debug` output or a hand-rolled formatter. The `export` command is where pretty formatting actually lives.

###### `export`
1. Get active patient id.
2. Call `manager.get_note(patient_id)?`.
3. Pass the note to the existing `export::format_note()` function (or whatever the function is called in `src/export.rs`).
4. Print the formatted text. Optionally also copy it to the clipboard via a library like `arboard`, but the plan defers that — print to stdout is fine for Phase 2.

This is the Epic-paste workflow: clinician runs `export`, selects the text, pastes it into Epic.

###### `reload`
This is the crash-simulation feature. Logic:
1. Get active patient id (or print "No active patient" and return).
2. Remove the patient's note from the store: `manager.store.remove(&patient_id)`. This drops the in-memory copy without touching SQLite. The DashMap is now missing that entry.
3. Call `load_patient_note(repo, patient_id)` to reconstruct from SQLite (snapshot fast-path + delta replay, or full replay if no snapshot).
4. Insert the reconstructed note back into the store.
5. Print: `"Reloaded patient <short uuid> from SQLite. <N> events replayed."`.

This requires the manager to expose either the store and repo references or a method that does the reload internally. The cleaner option: add `manager.reload_active_patient() -> AppResult<()>` that does steps 2-4 internally. The CLI just calls it.

The whole point is the reloaded note should be byte-identical to the one we just dropped. If the export looks the same after `reload`, persistence is working. If not, you've found a bug.

###### `status`
1. Print summary stats:
   - Current shift id.
   - Number of patients registered.
   - Active patient (if any) and their phase.
   - Number of events in the SQLite events table (run a `SELECT COUNT(*)`).
   - Number of snapshots written.
   - Whether an encounter is currently in progress (`manager.driver.has_active_encounter()`).

Most of these need accessors on the manager. Don't reach into private fields from `main.rs`. Add `manager.shift_id()`, `manager.event_count()`, `manager.snapshot_count()` as needed.

###### `quit`
Logic was already covered in section A9. From the dispatcher's perspective, return a sentinel that breaks the command loop.

###### `Command::Unknown(line)`
Print: `"Unknown command: '<line>'. Type 'help' to see commands."` Don't crash — the CLI is for experimentation and typos are inevitable.

(Consider adding a `help` command that re-prints the welcome banner's command list.)

##### D. Error handling style

Each handler returns `AppResult<()>`. The dispatcher does the printing:

- `Ok(())` → handlers already printed their success message inside. The dispatcher prints nothing.
- `Err(e)` → print `"Error: {e}"` and continue the loop.

The errors from `AppError` already have `Display` implementations via `thiserror` (`#[derive(Debug, thiserror::Error)]` in `src/error.rs`), so the `{e}` formatter gives you human-readable messages like `"patient not found: <uuid>"` or `"no active patient: ..."`.

Never panic in a handler. Never `.unwrap()` on user input. The CLI should be a robust REPL.

Startup failures (sections A3–A6) are different: those exit the process with `std::process::exit(1)` because there's no usable state to fall back to. Only command-loop failures keep looping.

##### E. What state lives where

This is the part that often trips people up when writing the CLI, so worth being explicit:

| Lives in | What | 
|----------|------|
| `ShiftManager` (in-memory) | `Shift`, `HashMap<PatientId, PatientRecord>`, `active_patient_id`, `EncounterDriver`, references to `EventBus` and `PatientNoteStore` |
| `EncounterDriver` (in-memory, owned by manager) | `audio`, `transcriber`, `active_encounter` (the in-flight recording state) |
| `PatientNoteStore` (DashMap) | All `PatientNote` aggregates, keyed by `PatientId`. Rebuilt from SQLite on startup. |
| SQLite `events` table | Every event ever published. The source of truth. |
| SQLite `snapshots` table | Latest snapshot per patient (Idle / Finalizing / Signed transitions only). |
| `shift.json` on disk | `Shift` + `Vec<PatientRecord>`. The non-event-sourced metadata. |
| Lost on quit | `active_patient_id`, `active_encounter` (the driver's current recording). |

The CLI's job is to wire user input into the manager's methods and let the manager handle all the persistence machinery. Resist the temptation to bypass the manager for "convenience" — every command should go through the official API. That's what makes the integration test in Step 7 trustworthy.

##### F. What this is NOT doing

To stay within Phase 2's scope:

- No interactive line editing (no rustyline). `std::io::stdin().read_line` is enough.
- No multi-line input. Each command fits on one line.
- No background tasks. Everything is synchronous on the main thread.
- No clipboard integration. `export` prints to stdout.
- No tab completion, no history, no Ctrl-C handling beyond the default (SIGINT kills the process).
- No JSON output mode, no machine-readable status. This is for the developer to manually exercise the pipeline.

These all become relevant in Phase 4 when SwiftUI replaces the CLI. The CLI is a developer tool, not a product.

##### G. Suggested order to implement

The CLI is straightforward but has many moving parts. Build incrementally:

1. Startup + welcome banner + command loop with only `quit` working.
2. Add `new` and `list` — these are pure manager calls, no audio.
3. Add `switch` — exercises the manager's active-patient logic.
4. Add `start` and `stop` — first audio-pipeline integration. Verify the event log grows in SQLite.
5. Add `note` — prints the current state, useful for debugging the rest.
6. Add `mdm`, `review`, `sign` — verify state machine transitions.
7. Add `lab` — verify non-audio events.
8. Add `export` — wires in the existing formatter.
9. Add `reload` — the persistence test. This is the milestone command.
10. Add `status` — observability polish.
11. Add `shift.json` save/load on quit/startup — cross-session persistence.

Each step ends with a manual verification: type a few commands, see the right output, peek at the SQLite database with `sqlite3 er_notes.db "SELECT * FROM events;"` to confirm events are landing.

##### H. Accessor methods to add to `ShiftManager`

The CLI logic above assumes a handful of read-only accessors on `ShiftManager` that don't exist yet. Add them as you implement the commands that need them:

| Accessor | Used by | Returns |
|----------|---------|---------|
| `active_patient_id()` | `lab`, `note`, `export`, `reload` | `Option<PatientId>` |
| `shift_id()` | `status` | `Uuid` (or whatever `Shift::id` is) |
| `event_count()` | `status` | `usize` — runs `SELECT COUNT(*) FROM events` |
| `snapshot_count()` | `status` | `usize` — runs `SELECT COUNT(*) FROM snapshots` |
| `has_active_encounter()` | `status` | `bool` — delegates to `driver.has_active_encounter()` |
| `reload_active_patient()` | `reload` | `AppResult<usize>` — returns event count replayed |

All take `&self` (not `&mut self`) — they're pure reads. Keep them simple; no business logic.

---

### Step 7: Crash-recovery integration test

**Purpose:** Prove that the entire pipeline — EncounterDriver → EventBus → SQLite → load_patient_note — survives a simulated crash. This is the most important test in Phase 2 because it validates the event-sourcing guarantee: if it was published, it can be reconstructed.

**File:** `src/tests/crash_recovery.rs` (new file, added to `src/tests/mod.rs`)

**Test: `full_lifecycle_survives_simulated_crash`**

```
Setup:
  1. Open in-memory SQLite
  2. Create SqliteRepository, EventBus, PatientNoteStore, EncounterDriver<MockAudioCapture, MockTranscriber>

Drive a multi-encounter visit:
  3. driver.start_encounter(patient_id, Ambient)
  4. driver.stop_encounter()  — publishes AudioStarted, AudioStopped, TranscriptReady
  5. driver.start_encounter(patient_id, Dictation)
  6. driver.stop_encounter()  — second encounter
  7. Publish TriggerMDM, ReviewReady (not via driver — these are manual commands)

Simulate crash (drop in-memory state):
  8. drop(store)  — the DashMap is gone
  9. Create a new empty PatientNoteStore

Reload from SQLite:
  10. loaded = load_patient_note(repo, patient_id)

Verify:
  11. assert loaded.phase == PendingSignOff
  12. assert loaded.encounter_count == 2
  13. assert loaded.encounters.len() == 2
  14. assert loaded.encounters[0].capture_mode == Ambient
  15. assert loaded.encounters[1].capture_mode == Dictation
  16. assert loaded.encounters[0].transcript.is_some()
  17. assert loaded.encounters[1].transcript.is_some()
  18. assert loaded.encounters[0].ended_at.is_some()
  19. assert loaded.encounters[1].ended_at.is_some()
```

**Why this test matters more than unit tests:**

- Unit tests for `EncounterDriver` verify that the right methods are called in the right order. This integration test verifies that the data survives the full round trip: struct → event → JSON → SQLite → JSON → event → struct. Any serialization mismatch, lost field, or wrong event ordering will surface here.

**Test: `switch_patient_mid_encounter_persists_both`**

```
1. Start encounter for patient A
2. driver.switch_patient(patient_B, Ambient)  — stops A's encounter, starts B's
3. driver.stop_encounter()  — stops B's encounter
4. Simulate crash + reload
5. Verify patient A: phase == Idle, encounter_count == 1, has transcript
6. Verify patient B: phase == Idle, encounter_count == 1, has transcript
```

This validates the atomicity of the switch operation: both patients' events are fully persisted even though the switch happened mid-stream.

---

### Step 8: Multi-encounter summarization wiring (optional stretch goal)

**Purpose:** Wire the `Summarizer` trait into the pipeline so that after transcription, the note sections get updated. This is where the mock transcript becomes a mock HPI/Exam.

**This step is optional for Phase 2.** The roadmap doesn't explicitly call for it, but the wiring is natural: after `TranscriptReady` is published, call `summarizer.summarize_encounter()` and update `note.note_sections`. The question is whether summarization should be:

(a) **Synchronous in `stop_encounter`** — simple, but blocks the pipeline during API calls in production
(b) **A separate event-driven step** — add a `SummarizationComplete` event variant, publish it after the summarizer returns
(c) **Deferred to Phase 3** — keep Phase 2 focused on audio/STT wiring only

**Recommendation: Option (c) — defer.** Summarization is an async operation (the real `ClaudeSummarizer` makes HTTP calls). Wiring it in Phase 2 would require deciding on the async story (`tokio::spawn` vs. blocking), which is a Phase 3/4 concern. Phase 2 should prove that events flow correctly; summarization adds complexity without validating the core pipeline.

**If you choose option (b) for Phase 2, here's the work:**

- Add `NoteEvent::SummarizationComplete { patient_id, sections: NoteSections, clinical_occurred_at }` variant
- In `EncounterDriver::stop_encounter`, after publishing TranscriptReady, call `summarizer.summarize_encounter()` and publish `SummarizationComplete`
- In `apply_event`, handle `SummarizationComplete` by updating `note.note_sections`
- This adds one event variant, one `apply_event` branch, and one more event in the replay chain

---

### Step 9: Cleanup and documentation

**Changes to existing files:**

| File | Change | Why |
|------|--------|-----|
| `src/lib.rs` | Add `pub mod encounter_driver;` and `pub mod shift_manager;` | Register new modules |
| `src/tests/mod.rs` | Add `mod crash_recovery;` | Register new test module |
| `CLAUDE.md` | Update "What's Working" to include encounter driver and CLI, update "What's Stubbed" | Keep docs accurate (learned from Phase 1 audit) |
| `Cargo.toml` | No new dependencies expected | MockAudioCapture + MockTranscriber require nothing new |

**No changes to:**

| File | Why unchanged |
|------|---------------|
| `src/events.rs` | NoteEvent variants and EventBus are complete from Phase 1. No new event types (unless Step 8 option b is chosen) |
| `src/repository.rs` | SqliteRepository is complete. EncounterDriver publishes through EventBus, which handles persistence |
| `src/state.rs` | `apply_event`, `replay`, `load_patient_note` are complete. EncounterDriver uses them indirectly through EventBus |
| `src/audio.rs` | `MockAudioCapture` is sufficient for Phase 2. The trait is unchanged |
| `src/stt.rs` | `MockTranscriber` is sufficient for Phase 2. The trait is unchanged |
| `src/models/*` | Domain types are complete for Phase 2's needs |

---

## 4. Acceptance Criteria

Phase 2 is complete when all of the following are true:

1. **`cargo check` emits zero warnings.**
2. **`cargo test --lib` passes all tests** (29 existing + new unit/integration tests).
3. **EncounterDriver unit tests pass:** `start_encounter` publishes `AudioStarted`, `stop_encounter` publishes `AudioStopped` then `TranscriptReady`, double-start returns error, double-stop returns error.
4. **`switch_patient` test passes:** old encounter is closed with transcript, new encounter is opened, events are in correct order.
5. **Crash-recovery integration test passes:** full lifecycle through EncounterDriver → drop DashMap → reload from SQLite → all fields match.
6. **Multi-patient crash-recovery test passes:** switch_patient between two patients → crash → reload both → both intact.
7. **CLI runs interactively:** `cargo run` starts the CLI, commands work, `reload` reconstructs from SQLite.
8. **CLI `new → start → stop → mdm → review → sign → export` produces a formatted note** (using MockSummarizer output or raw transcript).
9. **`CLAUDE.md` is updated** with Phase 2 completion status.

---

## 5. Files Changed

| File | Status | Lines (est.) |
|------|--------|-------------|
| `src/encounter_driver.rs` | **NEW** | ~150 |
| `src/shift_manager.rs` | **NEW** | ~200 |
| `src/main.rs` | **REWRITE** | ~250 |
| `src/tests/crash_recovery.rs` | **NEW** | ~120 |
| `src/tests/mod.rs` | EDIT | +1 line |
| `src/lib.rs` | EDIT | +2 lines |
| `CLAUDE.md` | EDIT | ~10 lines |
| **Total new code** | | **~725 lines** |

---

## 6. Risks and Decisions

### Risk 1: Synchronous transcription blocks the CLI

`MockTranscriber` returns instantly, but real Whisper (Phase 3) takes seconds. If `stop_encounter` blocks on transcription, the CLI hangs.

**Decision for Phase 2:** Accept the synchronous design. The mock is fast. Phase 3 introduces `tokio::task::spawn_blocking` for Whisper, which is a self-contained change inside `EncounterDriver::stop_encounter`. The event publishing order doesn't change.

### Risk 2: File-based SQLite path conflicts

If two CLI instances open the same `er_notes.db`, SQLite in WAL mode allows concurrent reads but only one writer. This shouldn't happen in normal use (one clinician, one device), but could cause confusing errors during development.

**Decision:** Use a path with the shift ID or a timestamp in the filename (e.g., `er_notes_{shift_id}.db`). Or just document "don't run two instances" and move on.

### Risk 3: `PatientRecord` not event-sourced

The `shift.json` file approach is a pragmatic shortcut. If the JSON file is lost but the SQLite database survives, we can replay events but we don't know which patients exist or their chief complaints.

**Decision:** Accept for Phase 2. The event stream contains `patient_id` in every event, so we could discover patients by scanning events, but we'd lose demographics. Phase 6 (FHIR integration) will event-source patient registration properly.

### Risk 4: Transcript overwrite vs. append

Currently, `apply_event` for `TranscriptReady` sets `enc.transcript = Some(text.clone())`, which overwrites. If two `TranscriptReady` events arrive for the same encounter (chunked transcription in Phase 3), the second overwrites the first.

**Decision for Phase 2:** Not a problem — `MockTranscriber` produces one transcript per encounter. Phase 3 must address this by either: (a) concatenating transcripts (`enc.transcript = Some(format!("{} {}", existing, new))`), or (b) using a `Vec<String>` of chunks on the `Encounter`. Flag this as a Phase 3 pre-work item.

---

## 7. What Phase 2 Does NOT Do

| Not in scope | Why | When |
|-------------|-----|------|
| Real microphone capture | Phase 2 uses `MockAudioCapture` | Phase 4 (iOS) |
| Real speech-to-text | Phase 2 uses `MockTranscriber` | Phase 3 (whisper-rs) |
| Real Claude summarization in pipeline | Phase 2 produces notes from mock/raw transcripts | Phase 3 or later |
| Async pipeline (`tokio::spawn`) | Mocks are fast enough for synchronous calls | Phase 3 |
| Apple Watch UI | Phase 2 tests `switch_patient` at the Rust level | Phase 5 |
| FHIR integration | Phase 2 uses manual `lab` command | Phase 6 |
| `SummarizationComplete` event variant | Optional stretch goal, recommended to defer | Phase 3 |
| Event compaction for PHI minimization | Crosscutting concern, not a single phase | Before production |
| Addendum model (Signed → Amended) | Requires state machine changes | Before production |

---

## Implementation Order

These steps are dependency-ordered. Complete one, run the tests, then move to the next.

```
Step  Action                                              Gate
────  ──────────────────────────────────────────────────  ──────────────
1     Create EncounterDriver struct + new()               cargo check
2     Implement start_encounter                           cargo check
2a      Add unit test: start publishes AudioStarted       test passes
2b      Add unit test: double-start returns error         test passes
3     Implement stop_encounter                            cargo check
3a      Add unit test: stop publishes AudioStopped +      test passes
        TranscriptReady in order
3b      Add unit test: stop-without-start returns error   test passes
4     Implement switch_patient                            cargo check
4a      Add unit test: switch stops old, starts new       test passes
5     Create ShiftManager struct + commands               cargo check
5a      Add unit test: register + start + stop lifecycle  test passes
6     Rewrite main.rs as interactive CLI                  cargo run works
6a      Test: new → start → stop → note                  manual verify
6b      Test: reload command reconstructs from SQLite     manual verify
7     Add crash_recovery integration tests                cargo test passes
8     (Optional) Wire summarization                       tests pass
9     Update CLAUDE.md + cleanup                          docs accurate
```

---

*End of Phase 2 plan. Begin implementation when ready.*
