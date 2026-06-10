# Phase 3 — AI Note Generation & One-Button MDM: Implementation Plan

*2026-06-08 | Depends on: Phase 2 (complete) | Implements: `doctor-feedback-gap-analysis.md` (P0, G1–G6) | Feeds into: Phase 4 (iOS), Phase 6 (FHIR)*

---

## What This Phase Accomplishes

Phase 2 proved that audio → STT → events → SQLite/DashMap flows correctly. But the note that comes out the other end is **empty of prose** — it accumulates transcripts and lab results, yet `note.note_sections.hpi/ros/exam/mdm` are never populated. The AI layer (`summarize_encounter`, `generate_mdm`) exists with good prompts but runs **only** in `src/bin/spike_summarizer.rs`; it is not connected to the live pipeline. See `doctor-feedback-gap-analysis.md` §1.

Phase 3 connects the AI generation layer to the event-sourced core so that a note actually drafts itself, and it delivers Dr. Joey's headline ask: **a single button that generates the MDM from the existing note plus the objective chart data.**

```
                        ┌─────────────────────────── Phase 3 wiring ───────────────────────────┐
stop_encounter ─▶ TranscriptReady ─▶ [de-identify] ─▶ summarize_encounter ─▶ EncounterSummarized ─▶ note.hpi/ros/exam
                                                                                                          │
lab / med / dx / dispo commands ─▶ ResultReceived / MedicationRecorded / DiagnosisAssigned / DispositionSet ─▶ note.{results,meds,dx,disposition}
                                                                                                          │
                                                                                                          ▼
mdm command ─▶ assemble objective data ─▶ [de-identify] ─▶ generate_mdm ─▶ MdmGenerated ─▶ note.mdm ─▶ TriggerMDM (Idle→Finalizing)
                                                                                                          │
                                                                                                          ▼
                                                                                              export ─▶ complete ED note
```

**Which Dr. Joey feedback this implements** (see `doctor-feedback-gap-analysis.md` §2):

| Feedback | What Phase 3 delivers |
|---|---|
| **F3** — single workflow: scribe → HPI, then auto-ingest objective data → MDM | HPI/Exam auto-populate on `stop`; MDM auto-generates on `mdm` from accumulated data |
| **F8** — *the game-changer*: AI generates MDM from the note + objective data, meds, results, disposition | Step 6: `mdm` assembles results + meds + diagnoses + disposition and generates a billing-aware MDM, persisted to the note |
| **F9** — "click a button in the MDM section" | The `mdm` command becomes a real one-button generator, not a state transition |
| **F10** — "could it work as a standalone prototype before full integration?" | Step 7: `mdm-from-text` — paste an existing note + chart data, get an MDM, **no FHIR required** |
| **F1** — objective chart data (meds, diagnoses, disposition) feeds the MDM | Steps 3–4: disposition, medication, and diagnosis capture |

**Non-negotiables preserved.** The re-evaluation flow (multi-encounter merging) and patient-switching are **untouched and reinforced**. `summarize_encounter` already merges encounter *N* into prior sections (`src/claude_summarizer.rs:120-141`); Phase 3 finally persists that merge. Dr. Joey's skepticism about re-eval consistency (F5/F6) is answered not by removing it but by making the **MDM independent of disciplined re-eval capture** — the MDM pulls from objective data that exists regardless of how many encounters were recorded.

**Why this phase, now:** every other roadmap item (FHIR read/write, iOS) assumes the note generates real prose. Phase 3 is the smallest cohesive change that turns the engine into a product a physician would actually want, and it does so without EMR-vendor buy-in.

---

## Table of Contents

1. [Prerequisites and Current State](#1-prerequisites-and-current-state)
2. [Architecture Overview](#2-architecture-overview)
3. [Step-by-Step Implementation](#3-step-by-step-implementation)
   - Step 0: Async runtime bridge + Summarizer/Deidentifier on ShiftManager (P0)
   - Step 1: Section-persistence events (G1)
   - Step 2: Unstub `lab` / `note` / `export` (G6)
   - Step 3: Disposition capture (G4)
   - Step 4: Objective-data models — medications + diagnoses (G3)
   - Step 5: HPI/Exam auto-summarization wiring (G2, part 1)
   - Step 6: One-button MDM end-to-end (G2, part 2)
   - Step 7: Standalone `mdm-from-text` (G5)
   - Step 8: Tests
   - Step 9: Cleanup and documentation
4. [Acceptance Criteria](#4-acceptance-criteria)
5. [Files Changed](#5-files-changed)
6. [Risks and Decisions](#6-risks-and-decisions)
7. [What Phase 3 Does NOT Do](#7-what-phase-3-does-not-do)
8. [Open Design Questions](#8-open-design-questions)

---

```
Order of Operations

Step  Action                                                  Gate
────  ──────────────────────────────────────────────────────  ──────────────
0     Add tokio runtime + S: Summarizer + Deidentifier to     cargo check
      ShiftManager; construct in main.rs
0a      Unit test: block_on(MockSummarizer) round-trips       test passes
1     Add EncounterSummarized/MdmGenerated/DispositionSet     cargo check
      events + apply_event arms + merge_sections helper
1a      Unit test: EncounterSummarized merges, never clobbers test passes
        a prior MDM
1b      Unit test: MdmGenerated sets mdm; replay round-trips  test passes
2     Unstub lab / note / export handlers                     cargo run works
2a      Manual: lab → note shows the result                   manual verify
3     Add `dispo` command + set_disposition + DispositionSet  cargo check
3a      Unit test: dispo discharge sets note.disposition      test passes
4     Add Medication + Diagnosis models + events + med/dx cmds cargo check
4a      Unit test: med/dx events replay onto the note         test passes
5     Wire stop_encounter → summarize → EncounterSummarized   cargo check
5a      Integration: stop populates HPI (MockSummarizer)      test passes
6     Rework trigger_mdm → assemble + generate_mdm →           cargo check
      MdmGenerated → TriggerMDM
6a      Integration: mdm populates note.mdm, phase Finalizing test passes
6b      Manual (real Claude): full note end-to-end            manual verify
7     Add `mdm-from-text` standalone command/bin              cargo run works
7a      Manual: paste note + data → MDM printed               manual verify
8     Full unit + integration test pass                       cargo test passes
9     Update CLAUDE.md + gap-analysis status + cleanup        docs accurate
```

---

## 1. Prerequisites and Current State

### Phase 2 delivered (verified this session):

- `EncounterDriver` orchestrates `start_encounter` / `stop_encounter` / `switch_patient` (`src/encounter_driver.rs`)
- `ShiftManager` owns the shift, patients, active-patient context, and delegates to the driver (`src/shift_manager.rs`)
- Interactive CLI with `new/list/switch/start/stop/lab/mdm/review/sign/note/export/reload/status` (`src/main.rs`, `src/cli.rs`)
- `EventBus::publish` persists-first, projects to DashMap, snapshots on Idle/Finalizing/Signed (`src/events.rs:138-193`)
- `apply_event` / `replay` / `load_patient_note` reconstruct a `PatientNote` from events (`src/state.rs:247-344`)
- Zero warnings (cleaned this session)

### The disconnect Phase 3 closes (from gap analysis §1):

| Symptom | Root cause | File |
|---|---|---|
| Note never gets HPI/Exam | `stop_encounter` publishes `TranscriptReady` but nothing calls `summarize_encounter` | `src/encounter_driver.rs:67-86` |
| `mdm` generates nothing | `trigger_mdm` only publishes the `TriggerMDM` state-transition event | `src/shift_manager.rs:125-133` |
| Generated text can't reach the note | No event carries sections/MDM back onto the aggregate | `src/events.rs:11-54` (no such variant) |
| `lab` / `note` / `export` error out | Handlers were stubbed in Phase 2 (accessors didn't exist yet — they do now) | `src/cli.rs:223-226, 246-254` |
| Disposition never set | No event/command sets `note.disposition` | `src/models/disposition.rs` exists, never written |
| No meds/orders/dx | Not modeled at all | `ResultType` = Lab/Imaging/Vital only (`src/models/result.rs:31-36`) |

### Existing pieces Phase 3 wires together:

| Piece | File | Current state | Phase 3 use |
|---|---|---|---|
| `Summarizer` trait | `src/summarizer.rs:9-27` | `summarize_encounter`, `generate_mdm` (async RPITIT) | Injected into `ShiftManager` |
| `ClaudeSummarizer` | `src/claude_summarizer.rs` | Real client, billing-aware MDM prompt (2021 E/M) | Default summarizer in `main.rs` |
| `MockSummarizer` | `src/summarizer.rs:30-56` | Returns template text, no network | Default summarizer in tests |
| `Deidentifier` | `src/deidentify.rs:11-63` | Strips name/MRN/phone/date/SSN, stable aliases | Runs before every Claude call |
| `NoteSections` | `src/models/note.rs:67-74` | `hpi/ros/exam/mdm/disposition_text` | Payload of new events |
| `NoteSectionVersion` | `src/models/note.rs:77-84` | Versioned section + `source_encounter_ids` | Audit trail for generated sections |
| `Disposition` | `src/models/disposition.rs:5-22` | 5 variants, never set | Step 3 sets it |
| `export::format_note` | `src/export.rs:5-49` | Already renders HPI/ROS/Exam/Results/MDM/Dispo | Step 2 calls it |

### Map: gap-analysis item → Phase 3 step

| Gap item | Step | Complexity (from gap analysis) |
|---|---|---|
| P0 — async bridge | Step 0 | S–M |
| G1 — section-persistence events | Step 1 | M |
| G6 — unstub lab/note/export | Step 2 | S |
| G4 — disposition capture | Step 3 | S |
| G3 — meds + diagnoses (scoped per Q4) | Step 4 | M (L if orders/consults added) |
| G2 — one-button MDM + HPI wiring | Steps 5–6 | M |
| G5 — standalone MDM-from-text | Step 7 | S–M |
| G7 (compaction), G8 (exam templates), full FHIR | **Deferred** — see §7 | — |

---

## 2. Architecture Overview

### Where the async bridge lives

`main()` is synchronous and the CLI loop reads stdin line-by-line. The `Summarizer` trait is `async` via return-position-`impl Trait` (`src/summarizer.rs:12-26`), which is **not `dyn`-compatible**. Two consequences drive the design:

1. **Summarizer is a generic, not a trait object.** `ShiftManager<A, T>` becomes `ShiftManager<A, T, S: Summarizer>`, mirroring how `A: AudioCapture` and `T: Transcriber` are already threaded. No boxing.
2. **The CLI stays synchronous; only the two AI commands block.** `ShiftManager` owns a `tokio::runtime::Runtime` and calls `runtime.block_on(future)` at exactly two points: after a transcript is ready (summarize) and when `mdm` is triggered (generate MDM). Everything else stays as it was.

```
┌──────────────────────────────────────────────────────────────────────────┐
│                          ShiftManager<A, T, S>                            │
│  Phase 2 fields: shift, patients, active_patient_id, driver, bus, store,  │
│                  repo                                                      │
│  Phase 3 adds:   summarizer: S          ← the AI layer (generic)          │
│                  runtime: Arc<Runtime>  ← sync↔async bridge               │
│                  deidentifier: Deidentifier ← PHI stripping before Claude │
│                                                                            │
│  stop_encounter():                                                         │
│    1. driver.stop_encounter()    → AudioStopped + TranscriptReady          │
│    2. read fresh transcript from store                                     │
│    3. deidentify(transcript, active PatientRecord)                         │
│    4. runtime.block_on(summarizer.summarize_encounter(...))   ← BLOCKS     │
│    5. bus.publish(EncounterSummarized { sections, ... })                   │
│                                                                            │
│  trigger_mdm():                                                            │
│    1. read note from store                                                 │
│    2. assemble objective_data (results + meds + dx) + disposition strings  │
│    3. deidentify(objective_data)                                           │
│    4. runtime.block_on(summarizer.generate_mdm(...))          ← BLOCKS     │
│    5. bus.publish(MdmGenerated { mdm_text, ... })                          │
│    6. bus.publish(TriggerMDM { ... })   → Idle → Finalizing                │
└──────────────────────────────────────────────────────────────────────────┘
```

### Why `ShiftManager` orchestrates summarization (not `EncounterDriver`)

`EncounterDriver` has a single responsibility: turn audio into `AudioStarted/Stopped/TranscriptReady` events. It knows nothing about patients, prior note sections, or de-identification. Summarization needs all three (the active `PatientRecord` for de-id, the prior `NoteSections` to merge into, the encounter number). That data lives in `ShiftManager`. Keeping the summarizer out of the driver preserves the Phase 2 separation and keeps the driver summarizer-free for the iOS FFI path in Phase 4.

### Why de-identify in the loop (Dr. Joey F4)

Dr. Joey's discoverability concern (the "saved and discoverable narrative") is partly addressed by never sending raw PHI to Claude. `Deidentifier::deidentify(text, patient)` (`src/deidentify.rs:34`) replaces the patient's name with `Patient_A`, and strips MRN/phone/date/SSN, before any network call. The returned HPI reads naturally ("44yo M with chest pain") because ED prose doesn't use patient names anyway. *Persisted* transcript minimization (the `TranscriptCompacted` tombstone from `creative-improvements.md:204-216`) is **G7, deferred** — see §7 and Open Question Q6.

### Event flow: two new generation events + three new data events

```
Phase 2 events (unchanged):   AudioStarted, AudioStopped, TranscriptReady,
                              ResultReceived, TriggerMDM, ReviewReady, Sign

Phase 3 generation events:    EncounterSummarized { sections, source_encounter_ids }
                              MdmGenerated { mdm_text }

Phase 3 objective-data events: DispositionSet { disposition }
                              MedicationRecorded { medication }
                              DiagnosisAssigned { diagnosis }
```

All five are folded in `apply_event` (`src/state.rs:247`), so replay and snapshot/restore stay correct with zero changes to `EventBus` or `SqliteRepository`.

---

## 3. Step-by-Step Implementation

### Step 0: Async runtime bridge + Summarizer/Deidentifier on `ShiftManager` (P0)

**Purpose:** Give `ShiftManager` the ability to call the async Claude client from the synchronous CLI, and to de-identify before doing so. Nothing generates yet — this step only makes generation *possible* and keeps everything compiling.

**File:** `src/shift_manager.rs` (edit), `src/main.rs` (edit)

**Struct change:**

```rust
use std::sync::Arc;
use tokio::runtime::Runtime;
use crate::summarizer::Summarizer;
use crate::deidentify::Deidentifier;

pub struct ShiftManager<A: AudioCapture, T: Transcriber, S: Summarizer> {
    shift: Shift,
    patients: HashMap<PatientId, PatientRecord>,
    active_patient_id: Option<PatientId>,
    driver: EncounterDriver<A, T>,
    bus: Arc<EventBus>,
    store: PatientNoteStore,
    repo: Arc<dyn Repository>,
    // Phase 3 additions:
    summarizer: S,
    runtime: Arc<Runtime>,
    deidentifier: Deidentifier,
}
```

**Constructor signature change:**

```rust
pub fn new(
    shift: Shift,
    patients: HashMap<PatientId, PatientRecord>,
    driver: EncounterDriver<A, T>,
    bus: Arc<EventBus>,
    store: PatientNoteStore,
    repo: Arc<dyn Repository>,
    summarizer: S,
    runtime: Arc<Runtime>,
) -> Self
```

`Deidentifier::new()` is constructed internally (no args). `cli::handle_command<A, T>` becomes `handle_command<A, T, S>` and gains the `S: Summarizer` bound (no body changes except the new arms in later steps).

**`main.rs` wiring:**

```
1. dotenvy::dotenv().ok();                          // load ANTHROPIC_API_KEY
2. let runtime = Arc::new(tokio::runtime::Runtime::new()?);  // current-thread is fine
3. let summarizer = build_summarizer();             // see Decision below
4. pass summarizer + runtime.clone() into ShiftManager::new(...)
```

**Decision — which `Summarizer` does `main` use?**
`ClaudeSummarizer::new()` (`src/claude_summarizer.rs:49`) fails if `ANTHROPIC_API_KEY` is unset. For an interactive backend tester you usually want real Claude, but you also want to run offline. Because the trait is not `dyn`-compatible, a runtime choice between Claude and Mock needs a hand-written enum that *is* a `Summarizer`:

```rust
pub enum AppSummarizer { Claude(ClaudeSummarizer), Mock(MockSummarizer) }

impl Summarizer for AppSummarizer {
    async fn summarize_encounter(&self, t: &str, p: &NoteSections, n: u32) -> AppResult<NoteSections> {
        match self {
            AppSummarizer::Claude(c) => c.summarize_encounter(t, p, n).await,
            AppSummarizer::Mock(m)   => m.summarize_encounter(t, p, n).await,
        }
    }
    async fn generate_mdm(&self, s: &NoteSections, r: &str, d: &str) -> AppResult<String> {
        match self {
            AppSummarizer::Claude(c) => c.generate_mdm(s, r, d).await,
            AppSummarizer::Mock(m)   => m.generate_mdm(s, r, d).await,
        }
    }
}
```

`main` builds `AppSummarizer::Claude` when the key is present, else logs a warning and falls back to `AppSummarizer::Mock`. This keeps generics (no boxing) while allowing offline runs. *(Recommended; see Q2.)*

**Why `Arc<Runtime>` and `block_on`, not `#[tokio::main]`:**
- A `#[tokio::main] async fn main` would force `handle_command` and the whole stdin loop to become async, and blocking `stdin().read_line` inside an async runtime is its own foot-gun. Storing a runtime and calling `block_on` only at the two AI call sites keeps the REPL synchronous and the blast radius tiny.
- `Runtime::new()` defaults to multi-thread; a `current_thread` runtime (`tokio::runtime::Builder::new_current_thread().enable_all().build()`) is lighter and sufficient — `reqwest` needs the IO driver, which `enable_all` provides.

**Test (Step 0a):** construct a `ShiftManager<_, _, MockSummarizer>` and assert `runtime.block_on(manager.summarizer.summarize_encounter("x", &NoteSections::default(), 1))` returns the mock HPI. Proves the bridge compiles and runs.

---

### Step 1: Section-persistence events (G1)

**Purpose:** Add the events that carry generated text and objective data back onto the `PatientNote`. This is the foundation — every later step persists through these.

**File:** `src/events.rs` (edit), `src/state.rs` (edit `apply_event`)

**New `NoteEvent` variants:**

```rust
/// AI produced/updated note prose from an encounter transcript.
EncounterSummarized {
    patient_id: PatientId,
    encounter_id: EncounterId,
    sections: NoteSections,          // hpi/ros/exam populated; mdm/dispo always None here
    source_encounter_ids: Vec<EncounterId>,
    clinical_occurred_at: DateTime<Utc>,
},
/// AI produced the MDM from accumulated data.
MdmGenerated {
    patient_id: PatientId,
    mdm_text: String,
    source_encounter_ids: Vec<EncounterId>,
    clinical_occurred_at: DateTime<Utc>,
},
/// Clinician set the disposition.
DispositionSet {
    patient_id: PatientId,
    disposition: Disposition,
    clinical_occurred_at: DateTime<Utc>,
},
```

Add arms to `extract_patient_id` (`src/events.rs:57`) and `event_type` (`src/events.rs:69`), and convenience constructors mirroring the existing ones (`src/events.rs:81-107`):

```rust
pub fn encounter_summarized(patient_id, encounter_id, sections, source_encounter_ids) -> Self // fills clinical_occurred_at: Utc::now()
pub fn mdm_generated(patient_id, mdm_text, source_encounter_ids) -> Self
pub fn disposition_set(patient_id, disposition) -> Self
```

`extract_encounter_id` (`src/events.rs:109`) returns `Some` for `EncounterSummarized`, `None` for the others.

**`apply_event` arms** (add to the `match` at `src/state.rs:247-307`):

```rust
NoteEvent::EncounterSummarized { sections, source_encounter_ids, .. } => {
    merge_sections(&mut note.note_sections, sections);   // field-wise, see helper
    record_section_versions(note, sections, source_encounter_ids);
    note.version += 1;
    note.updated_at = Utc::now();
    note.audit_log.push(AuditEntry::new(AuditAction::NoteSectionGenerated, ""));
}
NoteEvent::MdmGenerated { mdm_text, source_encounter_ids, .. } => {
    note.note_sections.mdm = Some(mdm_text.clone());
    note.section_history.push(NoteSectionVersion {
        section: "mdm".into(), content: mdm_text.clone(),
        version: note.version + 1, generated_at: Utc::now(),
        source_encounter_ids: source_encounter_ids.clone(),
    });
    note.version += 1;
    note.updated_at = Utc::now();
    note.audit_log.push(AuditEntry::new(AuditAction::MdmGenerated, ""));
}
NoteEvent::DispositionSet { disposition, .. } => {
    // Mirror ingest_result's immutability guard: Signed notes reject mutation.
    let can_mutate = note.state.as_ref().map(|s| s.can_ingest_results()).unwrap_or(false);
    if can_mutate {
        note.disposition = Some(disposition.clone());
        note.version += 1;
        note.updated_at = Utc::now();
        note.audit_log.push(AuditEntry::new(AuditAction::DispositionSet, ""));
    }
}
```

**The `merge_sections` helper (correctness-critical):**

```rust
/// Fold an EncounterSummarized delta into the note's sections.
/// Only HPI/ROS/Exam are touched, and only when the incoming field is Some.
/// MDM and disposition_text are NEVER modified here — they are owned by
/// MdmGenerated / DispositionSet respectively.
fn merge_sections(dst: &mut NoteSections, src: &NoteSections) {
    if src.hpi.is_some()  { dst.hpi  = src.hpi.clone(); }
    if src.ros.is_some()  { dst.ros  = src.ros.clone(); }
    if src.exam.is_some() { dst.exam = src.exam.clone(); }
    // dst.mdm and dst.disposition_text intentionally untouched.
}
```

**Why field-wise merge, not `note.note_sections = sections`:**
- `summarize_encounter` already performs the *clinical* merge (encounter 2 augments encounter 1) and returns the **full** HPI/Exam with `mdm: null` (the prompt forces null MDM, `src/claude_summarizer.rs:130,139`). If `apply_event` blindly replaced `note.note_sections`, then summarizing an encounter that happens **after** the MDM was generated — which is a legal transition: `Finalizing → Encountering` exists (`src/state.rs:130-133`) — would wipe the MDM with `None`. Field-wise merge makes the two generation paths compose safely regardless of order.
- This directly protects the re-eval flow (non-negotiable): a clinician can re-enter a room after triggering MDM, record, and the new HPI merges without destroying prior work.

**New `AuditAction` variants:** add `SectionsGenerated`, `MdmGenerated`, `DispositionSet`, (and in Step 4) `MedicationRecorded`, `DiagnosisAssigned` to the enum in `src/models/audit.rs`.

**Snapshot interaction (no code change, but verify):** `EventBus::publish` snapshots when the resulting phase is Idle/Finalizing/Signed (`src/events.rs:164-169`). `EncounterSummarized` is published while the note is `Idle` (post-`stop`) → snapshotted. `MdmGenerated` is published while `Idle` (just before `TriggerMDM`) → snapshotted. `DispositionSet` in `Idle` → snapshotted. No `EventBus` changes needed.

**Tests (1a, 1b):**
- `EncounterSummarized` with `hpi: Some, mdm: None` applied to a note whose `mdm` is already `Some("...")` → asserts `mdm` is preserved (regression guard for the merge).
- Full replay: `audio_started → audio_stopped → encounter_summarized → mdm_generated → trigger_mdm → review_ready → sign` reconstructs a note with populated HPI **and** MDM, phase `Signed`. (Extends `replay_full_lifecycle_reaches_signed`, `src/state.rs:432`.)

---

### Step 2: Unstub `lab` / `note` / `export` (G6)

**Purpose:** Make the three Phase 2 stubs real. They are prerequisites for feeding and observing the MDM, and they are nearly free — the blockers cited in the stub messages ("needs an active patient id accessor") no longer exist: `active_patient_id()` (`src/shift_manager.rs:170`), `add_lab()` (`:108`), and `get_note()` (`:153`) are all implemented.

**File:** `src/cli.rs` (edit handlers at `:223`, `:246`, `:251`)

**`lab <name> <value> <unit>`** (replace the stub at `src/cli.rs:223-226`):

```
1. let patient_id = manager.active_patient_id().ok_or(NoActivePatient)?;
2. let value: f64 = value.parse()?;                  // already a String from parsing
3. Build ClinicalResult {
       id: Uuid::now_v7(), patient_id, source: ResultSource::Manual,
       timestamp: Utc::now(),
       result_type: ResultType::Lab(LabResult {
           fhir_id: None, loinc_code: None, display_name: name,
           value, unit, reference_range: None, is_abnormal: false }),
   }
4. manager.add_lab(result, Utc::now())?;
5. Print: "Added lab: {name} = {value} {unit}"
```

*(Note: `Command::Lab` currently parses `value` as `String` at `src/cli.rs:88-92`; either parse to `f64` in the parser or in the handler. Parse in the handler to keep the parser dumb, consistent with `phase2.md` §B.)*

**`note`** (replace the stub at `src/cli.rs:246-249`):

```
1. let patient_id = manager.active_patient_id().ok_or(NoActivePatient)?;
2. let note = manager.get_note(patient_id).ok_or("no note")?;
3. Print a structured summary:
     Phase, encounter_count, #results, #medications, #diagnoses
     HPI / ROS / Exam / MDM (each: the text, or "<not generated>")
     Disposition (or "<not set>")
     audit_log length
```

**`export`** (replace the stub at `src/cli.rs:251-254`):

```
1. let patient_id = manager.active_patient_id().ok_or(NoActivePatient)?;
2. let note = manager.get_note(patient_id).ok_or("no note")?;
3. let patient = manager.patient_record(patient_id)?;   // add a small accessor
4. let results_text = format_results(&note.results);    // "Troponin: 0.04 ng/mL\n..."
5. let text = export::format_note(&patient, &note.note_sections,
                                  note.disposition.as_ref(), Some(&results_text));
6. Print text.
```

Add `ShiftManager::patient_record(&self, id) -> AppResult<PatientRecord>` (clone from the `patients` map) — `format_note` needs the `PatientRecord` for chief complaint and name (`src/export.rs:5-18`).

**Why now, before generation:** these commands are how you verify Steps 5–6. `note` shows whether sections populated; `export` shows the assembled document. Cheap, unblock-everything.

---

### Step 3: Disposition capture (G4)

**Purpose:** Let the clinician set the disposition so it can feed the MDM (Dr. Joey F1/F8) and the export. Uses the `DispositionSet` event from Step 1.

**File:** `src/cli.rs` (new `Command::Dispo`), `src/shift_manager.rs` (new `set_disposition`)

**New command grammar:**

```
dispo discharge "<instructions>" ["<follow-up>"]
dispo admit <service> [<bed>]
dispo transfer <destination> ["<reason>"]
dispo ama
dispo lwbs
```

**Parser** (`src/cli.rs`, new arm): match the second token to a `Disposition` variant. Quoted free-text spans (instructions, reason) are joined from the remaining tokens; keep it forgiving (a simple `args[2..].join(" ")` for the free-text tail is acceptable for a dev CLI — full quote parsing is not worth it here).

**`ShiftManager::set_disposition`:**

```rust
pub fn set_disposition(&mut self, disposition: Disposition) -> AppResult<()> {
    let patient_id = self.check_active_patient_id()?;
    self.bus.publish(NoteEvent::disposition_set(patient_id, disposition), &self.store)?;
    Ok(())
}
```

**Handler print:** `"Disposition set: Discharge (follow-up: Cardiology 1wk)"` etc.

**Test (3a):** `dispo discharge "..." "..."` → `manager.get_note(id).disposition` is `Some(Disposition::Discharge { .. })`; a `DispositionSet` published after `sign` is a no-op (immutability guard).

**Why disposition is an event, not a field write:** consistency with the event-sourced core — disposition belongs on the medico-legal timeline ("dispo decided at 16:15"), survives `reload`, and snapshots like everything else. `export::format_note` already accepts `Option<&Disposition>` (`src/export.rs:8`), so once the event sets `note.disposition`, export "just works."

---

### Step 4: Objective-data models — medications + diagnoses (G3)

**Purpose:** Model the objective inputs Dr. Joey names most insistently — *"medications administered"* and *"diagnosis"* — so the MDM can cite them. Per gap-analysis Q4, scope Phase 3 to **medications + diagnoses**; treat orders/consults as free-text "data reviewed" (below) and defer full modeling.

**Files:** `src/models/medication.rs` (new), `src/models/diagnosis.rs` (new), `src/models/mod.rs` (edit), `src/models/note.rs` (edit), `src/events.rs` (edit), `src/state.rs` (edit), `src/cli.rs` (edit)

**`src/models/medication.rs`** — FHIR-`MedicationAdministration`-shaped so Phase 6 is a mapping layer (same philosophy as `result.rs:21`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Medication {
    pub id: Uuid,
    pub patient_id: PatientId,
    pub name: String,
    pub dose: Option<String>,        // "324 mg", "1 g"
    pub route: Option<String>,       // "PO", "IV"
    pub rxnorm_code: Option<String>, // None = manually entered
    pub kind: MedicationKind,
    pub administered_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MedicationKind { Home, AdministeredInEd }
```

**`src/models/diagnosis.rs`:**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnosis {
    pub id: Uuid,
    pub patient_id: PatientId,
    pub text: String,            // "Acute pulmonary embolism"
    pub icd10_code: Option<String>,
    pub is_primary: bool,
}
```

**`PatientNote` additions** (`src/models/note.rs:90-110`, plus the manual `Clone` at `:116` and `Debug` at `:141`):

```rust
pub medications: Vec<Medication>,
pub diagnoses: Vec<Diagnosis>,
pub data_reviewed: Vec<String>,   // free-text orders/consults/results-reviewed for the MDM
```

Initialize empty in `PatientNote::new` (`:167`). **Remember to update the manual `Clone` impl** — it does not derive, so new fields must be added there or they silently won't clone.

**New events + `apply_event` arms:**

```rust
MedicationRecorded { patient_id, medication: Medication, clinical_occurred_at }
DiagnosisAssigned  { patient_id, diagnosis: Diagnosis,   clinical_occurred_at }
DataReviewed       { patient_id, note: String,           clinical_occurred_at } // orders/consults
```

`apply_event`: push onto the respective `Vec`, gated by the same `can_ingest_results()` immutability check as results (`src/state.rs:228-245`), bump version, audit.

**New CLI commands:**

```
med home <name> [dose] [route]          → MedicationKind::Home
med given <name> [dose] [route]         → MedicationKind::AdministeredInEd
dx <text> [--primary] [icd10]           → Diagnosis
data <free text>                        → DataReviewed (e.g., "data CTA chest ordered", "data Cardiology consulted: rec admit")
```

**ShiftManager methods:** `record_medication`, `assign_diagnosis`, `record_data_reviewed` — each builds the struct, publishes the event (same shape as `add_lab`, `src/shift_manager.rs:108-123`).

**Why a generic `data` command for orders/consults instead of full models:**
- Dr. Joey lists orders and consultations as MDM inputs, but their *structured* value (vs. free text) is low for MDM prose — the MDM narrates "CTA chest was ordered and negative for PE; Cardiology was consulted and recommended admission." A `Vec<String>` of reviewed-data notes captures that with a fraction of the modeling cost, and Phase 6 FHIR (`ServiceRequest`, `Encounter`) can replace it later. Meds and diagnoses get real structs because they recur as discrete, codeable facts (RxNorm/ICD-10) the billing layer will eventually want.

**Test (4a):** `med given Aspirin 324mg PO`, `dx "Acute PE" --primary`, `data "CTA chest: PE in RLL"` → all three appear on `get_note(id)`; replay round-trips them.

---

### Step 5: HPI/Exam auto-summarization wiring (G2, part 1)

**Purpose:** Make the note draft its own HPI/Exam the moment a recording stops — the first half of Dr. Joey's single workflow (F3). This is also the live home of multi-encounter merging.

**File:** `src/shift_manager.rs` (rework `stop_encounter`)

**Current `stop_encounter`** (`src/shift_manager.rs:73-76`) just delegates to the driver. Rework it to orchestrate summarization:

```rust
pub fn stop_encounter(&mut self) -> AppResult<EncounterId> {
    // 1. Driver publishes AudioStopped + TranscriptReady (state → Idle, snapshot).
    let encounter_id = self.driver.stop_encounter()?;

    // 2. Summarize and persist. Best-effort: a summarization failure must NOT
    //    lose the encounter — the transcript is already durably persisted.
    if let Err(e) = self.summarize_encounter(encounter_id) {
        tracing::warn!(error = %e, %encounter_id, "summarization failed; transcript retained");
    }
    Ok(encounter_id)
}

fn summarize_encounter(&mut self, encounter_id: EncounterId) -> AppResult<()> {
    let patient_id = self.check_active_patient_id()?;

    // Read fresh transcript + prior sections + encounter number from the store.
    let (transcript, prior, encounter_number) = {
        let note = self.store.get(&patient_id).ok_or(/* no note */)?;
        let enc  = note.encounters.iter().find(|e| e.id == encounter_id);
        let transcript = enc.and_then(|e| e.transcript.clone()).unwrap_or_default();
        (transcript, note.note_sections.clone(), note.encounter_count)
    };
    if transcript.trim().is_empty() { return Ok(()); }   // nothing to summarize

    // De-identify before the network call (F4 / HIPAA).
    let patient = self.patients.get(&patient_id).cloned().ok_or(/* ... */)?;
    let safe_transcript = self.deidentifier.deidentify(&transcript, &patient);

    // Bridge sync → async.
    let sections = self.runtime.block_on(
        self.summarizer.summarize_encounter(&safe_transcript, &prior, encounter_number)
    )?;

    // Persist via the new event (Step 1).
    self.bus.publish(
        NoteEvent::encounter_summarized(patient_id, encounter_id, sections, vec![encounter_id]),
        &self.store,
    )?;
    Ok(())
}
```

**Why best-effort (don't fail `stop` if Claude fails):**
- The transcript is already persisted by the driver before summarization runs. If the network is down or the API errors, the clinician's words are safe; the HPI can be regenerated later (a future `resummarize` command, or on `reload`). Failing `stop` would strand the encounter and break patient-switching (the next `start` would error with "encounter already in progress"). Best-effort keeps the non-negotiable switch flow robust.

**Why this preserves re-eval/merge (non-negotiable):**
- `prior` is the note's current `note_sections`; `encounter_number` is `encounter_count`. `summarize_encounter` is explicitly told to merge encounter ≥2 into prior sections (`src/claude_summarizer.rs:140`). Each `stop` therefore augments the HPI rather than replacing it — exactly the re-eval behavior Dr. Joey values, now actually persisted (it never was before Phase 3).

**Integration test (5a):** with `MockSummarizer`, `register → switch → start → stop` → `get_note(id).note_sections.hpi == Some("[mock HPI from encounter transcript]")`; a second `start → stop` keeps HPI populated (merge path runs).

**Decision — inline vs. explicit command (Q3):** Phase 3 summarizes **inline on `stop`** (closest to Dr. Joey's "seamless"). The cost is one API call per encounter (`infrastructure-cost-model.md:127` flags this). If that proves expensive in pilots, a later phase can move it behind an explicit `summarize` command or batch it at `mdm` time — the event/persistence design is identical either way.

---

### Step 6: One-button MDM end-to-end (G2, part 2) — *the headline*

**Purpose:** Make the `mdm` command actually generate the MDM from the existing note + objective data and persist it. This is Dr. Joey's game-changer (F8/F9).

**File:** `src/shift_manager.rs` (rework `trigger_mdm`), `src/cli.rs` (richer `mdm` print)

**Current `trigger_mdm`** (`src/shift_manager.rs:125-133`) only publishes `TriggerMDM`. Rework:

```rust
pub fn trigger_mdm(&mut self) -> AppResult<()> {
    let patient_id = self.check_active_patient_id()?;

    // 1. Snapshot the note's current clinical picture.
    let (sections, objective_data, disposition_str, source_ids) = {
        let note = self.store.get(&patient_id).ok_or(/* ... */)?;
        let sections = note.note_sections.clone();
        let objective_data = assemble_objective_data(&note);   // see helper
        let disposition_str = note.disposition.as_ref()
            .map(format_disposition_for_mdm)
            .unwrap_or_else(|| "Not yet determined".into());
        let source_ids = note.encounters.iter().map(|e| e.id).collect::<Vec<_>>();
        (sections, objective_data, disposition_str, source_ids)
    };

    // 2. De-identify the objective-data blob (sections already de-identified at summarize time).
    let patient = self.patients.get(&patient_id).cloned().ok_or(/* ... */)?;
    let safe_objective = self.deidentifier.deidentify(&objective_data, &patient);

    // 3. Generate (sync → async bridge).
    let mdm = self.runtime.block_on(
        self.summarizer.generate_mdm(&sections, &safe_objective, &disposition_str)
    )?;

    // 4. Persist the MDM, THEN drive the state transition.
    self.bus.publish(NoteEvent::mdm_generated(patient_id, mdm, source_ids), &self.store)?;
    self.bus.publish(NoteEvent::trigger_mdm(patient_id), &self.store)?;  // Idle → Finalizing
    Ok(())
}
```

**`assemble_objective_data` helper** — the function that realizes "pull from the note, objective data, meds, results, and disposition":

```rust
/// Render the note's structured facts into the plain-text "data reviewed"
/// blob that generate_mdm() consumes as `results_summary`.
fn assemble_objective_data(note: &PatientNote) -> String {
    let mut out = String::new();
    if !note.results.is_empty() {
        out.push_str("Results:\n");
        for r in &note.results { out.push_str(&format!("- {}\n", format_result_line(r))); }
    }
    if !note.medications.is_empty() {
        out.push_str("Medications:\n");
        for m in &note.medications {
            let tag = match m.kind { MedicationKind::Home => "home", MedicationKind::AdministeredInEd => "given" };
            out.push_str(&format!("- [{tag}] {} {}\n", m.name, m.dose.clone().unwrap_or_default()));
        }
    }
    if !note.diagnoses.is_empty() {
        out.push_str("Diagnoses:\n");
        for d in &note.diagnoses {
            out.push_str(&format!("- {}{}\n", d.text, if d.is_primary { " (primary)" } else { "" }));
        }
    }
    if !note.data_reviewed.is_empty() {
        out.push_str("Other data reviewed (orders/consults):\n");
        for s in &note.data_reviewed { out.push_str(&format!("- {s}\n")); }
    }
    out
}
```

**Why publish `MdmGenerated` BEFORE `TriggerMDM`:**
- We want the MDM text on the note *before* the `Idle → Finalizing` transition that semantically means "draft assembled." Publishing `MdmGenerated` while still `Idle` snapshots the note **with** the MDM; the subsequent `TriggerMDM` snapshots again at `Finalizing`. If the order were reversed, a crash between the two events could leave a `Finalizing` note with no MDM. Persist the content, then advance the state.

**Why reuse `generate_mdm`'s existing signature** `(sections, results_summary, disposition)` (`src/claude_summarizer.rs:163-168`):
- The prompt is already billing-aware (2021 E/M three-element MDM, `:169-204`). Rather than change the signature, we widen what flows through `results_summary` — it becomes the full objective-data blob (results + meds + dx + orders/consults). One small prompt addition is worth making: append a line to the system prompt instructing it to cite administered medications and assigned diagnoses explicitly (`src/claude_summarizer.rs:177-182`). Minimal change, maximal alignment with F8.

**Guard — require something to work with:** if `sections.hpi`, `objective_data`, and `disposition` are all empty, return a friendly error ("Nothing to generate an MDM from yet — record an encounter or add results first") instead of calling Claude with nothing. Prevents a wasted API call and a hallucinated MDM.

**Richer `mdm` handler print** (`src/cli.rs:228-232`):

```
"MDM generated and note moved to Finalizing.
 ── MDM ──────────────────────────────────────
 {first ~400 chars of note.note_sections.mdm}…
 (run 'note' or 'export' to see the full note)"
```

**Integration test (6a):** seed a note (HPI via MockSummarizer, a lab, a disposition), call `trigger_mdm` with `MockSummarizer` → `get_note(id).note_sections.mdm == Some("[mock MDM: …]")` and `phase == Finalizing`. **Manual (6b):** with a real `ANTHROPIC_API_KEY`, run the full transcript from `spike_summarizer` through the CLI and confirm `export` yields a complete, clinically coherent note.

---

### Step 7: Standalone `mdm-from-text` (G5) — fastest validation of the thesis

**Purpose:** Answer Dr. Joey's F10 directly — *"could it work as a standalone prototype before full integration?"* — with a path that needs **no audio, no event sourcing, no FHIR**: paste an existing note + objective data, get an MDM. This is the cheapest thing to put in front of a physician and the strongest near-term demo.

**File:** `src/cli.rs` (new `Command::MdmFromText`) **or** `src/bin/mdm_standalone.rs` (new bin). Recommend a **bin** — it stands alone, can be scripted, and doesn't depend on shift state.

**`src/bin/mdm_standalone.rs`:**

```rust
#[tokio::main]
async fn main() -> AppResult<()> {
    dotenvy::dotenv().ok();
    // Read the existing note + objective data from stdin (or a file path arg).
    //   - everything before a "---DATA---" separator = the existing HPI/exam text
    //   - everything after = objective data (results, meds, dx, dispo)
    let (note_text, data_text, dispo) = read_input()?;     // simple split
    let summarizer = ClaudeSummarizer::new()?;
    let sections = NoteSections { hpi: Some(note_text), ros: None, exam: None, mdm: None, disposition_text: None };
    let mdm = summarizer.generate_mdm(&sections, &data_text, &dispo).await?;
    println!("\n===== GENERATED MDM =====\n{mdm}");
    Ok(())
}
```

Add `[[bin]] name = "mdm_standalone"` to `Cargo.toml` (mirrors the existing `spike_summarizer` bin, `Cargo.toml:36-38`).

**Why a separate bin and not just the CLI command:** it makes the standalone value proposition tangible and independently shippable — a clinician (or a sales demo) can pipe a de-identified chart excerpt in and see the MDM, with zero of the app's machinery. It's also the natural seed for a future thin web endpoint if the standalone product (Q5) gets greenlit.

**Manual test (7a):** pipe the chest-pain note + "Troponin negative; CTA negative for PE; ASA 324 given; dx: non-cardiac chest pain; dispo: discharge" → a coherent MDM prints.

---

### Step 8: Tests

**Unit (in the relevant module `#[cfg(test)]` blocks):**
- `merge_sections` preserves a prior MDM when an `EncounterSummarized` with `mdm: None` arrives (Step 1 regression).
- `apply_event` arms for all five new events (sections merge, mdm set, disposition set, med push, dx push).
- `DispositionSet`/`MedicationRecorded` after `Sign` are no-ops (immutability via `can_ingest_results`).
- `AppSummarizer` enum dispatches to both arms.

**Integration (`src/tests/`, new `generation_integration.rs`, registered in `src/tests/mod.rs`):**
- `stop_encounter` with `MockSummarizer` populates HPI; second encounter keeps it (merge path).
- `trigger_mdm` with `MockSummarizer` sets `note.mdm` and moves to `Finalizing`.
- **Crash-recovery:** full generation lifecycle → drop the DashMap entry → `load_patient_note` → HPI, MDM, disposition, meds, diagnoses all survive replay/snapshot. (Extends the Phase 2 crash-recovery pattern; this is the proof that the new events persist correctly.)
- `mdm-from-text` bin: covered by a thin unit test on `read_input`'s split logic (the Claude call itself is manual-only).

**Test summarizer:** all automated tests use `MockSummarizer` — no network, deterministic. Real-Claude runs are manual (6b, 7a).

---

### Step 9: Cleanup and documentation

| File | Change | Why |
|---|---|---|
| `src/lib.rs` | `pub mod` for any new modules; export `Medication`, `Diagnosis` | Register new types |
| `src/models/mod.rs` | `pub mod medication; pub mod diagnosis;` + re-exports | Convention (`src/models/mod.rs:9-16`) |
| `src/tests/mod.rs` | `mod generation_integration;` | Register integration tests |
| `Cargo.toml` | `[[bin]] mdm_standalone` | Step 7 |
| `CLAUDE.md` | Move HPI/MDM wiring, disposition, meds/dx from "What's Stubbed" → "What's Working"; add Phase 3 summary | Keep docs honest (the gap analysis exists because they drifted before) |
| `doctor-feedback-gap-analysis.md` | Flip F3/F8/F9 and G1–G6 status to ✅/🟡-built; leave G7/G8 as deferred | Close the loop on the analysis |

**No changes to:** `EventBus` (`src/events.rs:121-200`, snapshot logic already handles the new events), `SqliteRepository` (`src/repository.rs`, stores any `NoteEvent` as JSON), `db.rs` (schema unchanged), `audio.rs`/`stt.rs` (mocks still fine).

---

## 4. Acceptance Criteria

Phase 3 is complete when all of the following hold:

1. **`cargo check` emits zero warnings.**
2. **`cargo test` passes** — all Phase 2 tests plus the new unit + integration tests.
3. **`stop` populates HPI:** after `new → switch → start → stop`, the `note` command shows a non-empty HPI (MockSummarizer in tests, real Claude manually).
4. **Multi-encounter merge persists:** a second `start → stop` augments, never blanks, the HPI.
5. **One-button MDM works:** `mdm` generates an MDM from results + meds + diagnoses + disposition, sets `note.note_sections.mdm`, and moves the phase to `Finalizing`.
6. **Disposition, meds, diagnoses capture:** `dispo`, `med`, `dx`, `data` commands persist onto the note and survive `reload`.
7. **Crash-recovery covers generation:** dropping the DashMap entry and reloading from SQLite reconstructs HPI, MDM, disposition, meds, and diagnoses identically.
8. **Standalone path runs:** `mdm_standalone` produces an MDM from pasted text with no shift/DB state.
9. **Full CLI flow** `new → switch → start → stop → lab → med given → dx → dispo discharge → mdm → review → sign → export` **produces a complete, signable ED note.**
10. **De-identification runs before every Claude call** (verified by a unit test asserting the patient name never appears in the string passed to the summarizer).
11. **`CLAUDE.md` and `doctor-feedback-gap-analysis.md` updated.**

---

## 5. Files Changed

| File | Status | Lines (est.) |
|---|---|---|
| `src/shift_manager.rs` | **REWRITE** (generics + summarize/trigger_mdm + dispo/med/dx methods + helpers) | ~180 changed/added |
| `src/events.rs` | EDIT (5 new variants + constructors + match arms) | ~90 |
| `src/state.rs` | EDIT (5 `apply_event` arms + `merge_sections` + tests) | ~120 |
| `src/cli.rs` | EDIT (unstub 3 + add `dispo`/`med`/`dx`/`data` + generic param) | ~150 |
| `src/main.rs` | EDIT (runtime + summarizer construction) | ~25 |
| `src/models/medication.rs` | **NEW** | ~40 |
| `src/models/diagnosis.rs` | **NEW** | ~25 |
| `src/models/note.rs` | EDIT (3 fields + Clone/Debug/new) | ~20 |
| `src/models/audit.rs` | EDIT (5 `AuditAction` variants) | ~10 |
| `src/models/mod.rs` | EDIT (modules + re-exports) | ~6 |
| `src/claude_summarizer.rs` | EDIT (one MDM-prompt line for meds/dx) | ~3 |
| `src/bin/mdm_standalone.rs` | **NEW** | ~60 |
| `src/tests/generation_integration.rs` | **NEW** | ~180 |
| `src/tests/mod.rs` / `src/lib.rs` / `Cargo.toml` | EDIT | ~6 |
| `CLAUDE.md` / `doctor-feedback-gap-analysis.md` | EDIT | ~30 |
| **Total new/changed** | | **~900 lines** |

---

## 6. Risks and Decisions

### Risk 1: Synchronous Claude call blocks the CLI for seconds
`runtime.block_on(generate_mdm)` freezes the REPL during the HTTP round-trip (1–5s for MDM).
**Decision for Phase 3:** Accept it. This is a developer backend tester (`src/main.rs:107`), not the clinician UI; a brief blocking pause is fine and makes the control flow trivial to reason about. Print "Generating MDM…" before the call so the pause is explained. Phase 4 (iOS) owns the real async UX (spinner, background task).

### Risk 2: De-identification placeholders leak into the note
`Deidentifier` replaces dates with `[DATE]` and the name with `Patient_A` (`src/deidentify.rs:57-60`). If the transcript said "started 3 days ago on 6/5" the MDM may read "[DATE]," and timing nuance is lost.
**Decision:** Accept for Phase 3; ED HPIs rarely need absolute dates, and the clinician reviews before signing. Re-identification (mapping placeholders back after generation) is a known enhancement — flag as Q-follow-up, not Phase 3 scope. The regex de-id is explicitly MVP-grade (`src/deidentify.rs:9-10`); production needs a certified service regardless.

### Risk 3: MDM generated from a thin note hallucinates
If the clinician triggers `mdm` before recording or adding data, the model may invent findings.
**Decision:** The Step 6 guard refuses to generate when HPI, objective data, and disposition are all empty. Beyond that, every MDM is a *draft for review* — consistent with the safety boundary in `ARCHITECTURE.md:789-797` ("DRAFT — clinician review required"). Consider prefixing the persisted MDM with that banner.

### Risk 4: `summarize_encounter` returns `mdm`/`disposition_text` non-null and clobbers
The prompt tells the model to return `mdm: null` (`src/claude_summarizer.rs:130`), but models occasionally disobey.
**Decision:** `merge_sections` (Step 1) structurally ignores `mdm`/`disposition_text` from `EncounterSummarized` — it only copies HPI/ROS/Exam. Even a misbehaving model cannot overwrite the MDM through the summarize path. This is why the merge is field-wise, not a struct assignment.

### Risk 5: Cost — one API call per `stop`, one per `mdm`
`infrastructure-cost-model.md:127` flags continuous regeneration as a cost driver.
**Decision:** Phase 3 summarizes inline on `stop` for UX (Q3). If pilots show cost pressure, move summarization behind an explicit command or batch at `mdm` time — the event design is unchanged. Model routing (Haiku for HPI drafts, Sonnet for MDM, per `infrastructure-cost-model.md:225`) is a `ClaudeSummarizer` config change, not an architecture change.

### Decision 6: Scope G3 to meds + diagnoses (orders/consults as free text)
Full order/consult modeling is deferred (Q4). Meds and diagnoses get real, codeable structs; orders/consults flow through `data_reviewed: Vec<String>`. Phase 6 FHIR (`ServiceRequest`, `Encounter`) supersedes the free-text path.

---

## 7. What Phase 3 Does NOT Do

| Not in scope | Why | When |
|---|---|---|
| EMR/FHIR read or write | No vendor buy-in needed for Phase 3's value; standalone path (Step 7) proves the thesis without it | Phases 6–8 (`roadmap-and-next-phase-040826.md:197-224`) |
| Transcript compaction / ephemeral mode (G7) | Crosscutting PHI-retention concern; tensions with the immutable-audit-trail selling point | Before production; gated on Q6 (`creative-improvements.md:193-216`) |
| Structured/normal-exam templates (G8, Dr. Joey F7) | Mostly a UI concern; minimal backend value until iOS exists | Phase 4/5 (`ARCHITECTURE.md:765-769`) |
| Full order/consultation models | Free-text `data_reviewed` is sufficient for MDM prose now | Phase 6 (FHIR `ServiceRequest`) |
| PMH/PSH/home-med auto-pull | Requires FHIR read | Phase 6 (Q7) |
| Async UI / non-blocking generation | CLI is a dev tester; blocking is acceptable | Phase 4 (iOS) |
| Real microphone + on-device STT | Still `MockAudioCapture`/`MockTranscriber` | Phase 4 / whisper-rs |
| Re-identification of de-id placeholders | Known enhancement, not required for a review-before-sign draft | Post-Phase 3 |
| Removing or de-emphasizing re-eval | **Non-negotiable** — re-eval and patient-switching stay and are reinforced | Never |

---

## 8. Open Design Questions

Carried from `doctor-feedback-gap-analysis.md` §6 — decide before/at the relevant step:

- **Q1 (Step 0)** — Store a `tokio::runtime::Runtime` and `block_on`, or make `main` async? *Recommendation: store a runtime; keep the REPL sync.*
- **Q2 (Step 0)** — Inject summarizer as a generic `S` (matches `A`/`T`), with the `AppSummarizer` enum for Claude/Mock selection? *Recommendation: yes — avoids the non-`dyn` RPITIT problem with no boxing.*
- **Q3 (Step 5)** — Summarize inline on `stop`, or behind an explicit command? *Recommendation: inline for "seamless"; revisit if cost bites.*
- **Q4 (Step 4)** — Which objective-data types are real models first? *Recommendation: medications + diagnoses; orders/consults as free text.*
- **Q5 (Step 7)** — Is the standalone `mdm-from-text` a real near-term **product**, or internal validation only? *This is the central product call Dr. Joey is asking you to make (F10).*
- **Q6 (deferred G7)** — Offer ephemeral/compacted transcripts to address the "saved and discoverable" concern (F4), knowing it tensions with the immutable-audit-trail pitch (`product-summary.md:76`)?
- **Q7 (deferred)** — Where do PMH/PSH/home-meds come from before FHIR — manual entry, paste (Step 7), or skip until Phase 6?

---

## Implementation Order

Dependency-ordered. Complete a step, run its gate, then proceed.

```
Step  Action                                                  Gate
────  ──────────────────────────────────────────────────────  ──────────────
0     Runtime + S: Summarizer + Deidentifier on ShiftManager  cargo check
0a      block_on(MockSummarizer) round-trips                  test passes
1     5 new events + apply_event arms + merge_sections        cargo check
1a      merge never clobbers a prior MDM                      test passes
1b      full-lifecycle replay populates HPI + MDM             test passes
2     Unstub lab / note / export                              cargo run works
2a      lab → note shows the result                           manual verify
3     dispo command + set_disposition + DispositionSet        cargo check
3a      dispo discharge sets note.disposition                 test passes
4     Medication + Diagnosis models/events + med/dx/data cmds  cargo check
4a      med/dx/data replay onto the note                      test passes
5     stop_encounter → summarize → EncounterSummarized        cargo check
5a      stop populates HPI; 2nd encounter merges              test passes
6     trigger_mdm → assemble + generate_mdm → MdmGenerated     cargo check
6a      mdm sets note.mdm, phase Finalizing (MockSummarizer)  test passes
6b      full note end-to-end (real Claude)                    manual verify
7     mdm_standalone bin                                      cargo run works
7a      paste note + data → MDM printed                       manual verify
8     Full unit + integration + crash-recovery                cargo test passes
9     Update CLAUDE.md + gap-analysis + cleanup               docs accurate
```

---

*End of Phase 3 plan. Begin implementation when ready. This phase implements P0 and G1–G6 from `doctor-feedback-gap-analysis.md`; G7 (transcript compaction) and G8 (exam templates) are deferred to later phases as noted in §7.*
