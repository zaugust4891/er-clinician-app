# ED Progressive Note Builder: Rust Mobile Architecture Analysis & Build Roadmap

> **Last updated:** 2026-02-20
> **Status:** Pre-implementation design document
> **Target:** Single-clinician iOS MVP for ED shift testing

---



## Project Structure

                                                                              
  src/                                                                         
  ├── lib.rs                  # Library root — 10 module declarations          
  ├── main.rs                 # Binary entry — creates shift, patient, note    
  store, verifies DB                                                           
  ├── error.rs                # AppError enum (9 variants) via thiserror       
  ├── models/                                                             
  │   ├── mod.rs              # Re-exports all types                           
  │   ├── patient.rs          # PatientId, PatientRecord                       
  │   ├── note.rs             # PatientNote, NotePhase (6 states), NoteSections
  │   ├── encounter.rs        # Encounter, CaptureMode (Ambient/Dictation)     
  │   ├── result.rs           # ClinicalResult, LabResult, ImagingResult,      
  VitalSign                                                               
  │   ├── disposition.rs      # Disposition (Discharge/Admit/Transfer/AMA/LWBS)
  │   ├── shift.rs            # Shift lifecycle                           
  │   └── audit.rs            # AuditEntry, AuditAction                        
  ├── state.rs                # FSM transitions (with Idle↔Encountering        
  cycles) + PatientNoteStore                                                   
  ├── events.rs               # NoteEvent enum + broadcast EventBus            
  ├── audio.rs                # AudioCapture trait + MockAudioCapture (incl.
  patient switching)
  ├── stt.rs                  # Transcriber trait + MockTranscriber
  ├── summarizer.rs           # Summarizer trait + MockSummarizer (async)
  ├── deidentify.rs           # Deidentifier with regex PHI stripping (3 tests)
  ├── db.rs                   # SQLite schema (6 tables, 5 indexes, WAL mode)
  (2 tests)
  └── export.rs               # Plain-text note formatter for Epic paste (1
  test)


## 1. Problem Decomposition in ED Terms

### What "Progressive Note Building" Actually Means in the ED

Unlike ambulatory visits (one encounter, linear narrative, note written at end), an ED note is a **living document assembled from temporally scattered fragments** across a shift:

```
Time  →  00:00  00:15  00:30  01:00  01:45  02:30  03:00  04:00
─────────────────────────────────────────────────────────────────
Pt A  →  [HPI]         [RAs]         [CBC↑]        [MDM] [Dispo]
Pt B  →         [HPI]         [XR↓]         [RAs]         [Dispo]
Pt C  →                [HPI]                      [CT↑] [MDM]
```

**Three distinct event types drive the note:**

| Event Class | Source | Timing | Note Impact |
|---|---|---|---|
| Encounter audio | Clinician speech | Any time | HPI, RFV, exam, plan snippets |
| Clinical data | Labs/imaging/vitals/meds | Async, unpredictable | MDM data points, reassessment context |
| Clinician intent | Manual tags/dispo buttons | Discrete moments | Note section routing, MDM triggers |

### What Data Arrives When

```
T+0  min:  Registration → Chief complaint, basic demographics
T+5  min:  Triage vitals → First data anchor
T+10 min:  First encounter → HPI, exam findings (your audio)
T+30 min:  POC results → iStat, glucometer (fast)
T+45 min:  CBC/BMP/UA → Basic labs (moderate)
T+60 min:  XR read → Radiology report text (variable)
T+90 min:  CT read → Typically the longest wait
T+?? min:  Reassessment → You re-enter the room, audio again
T+?? min:  MDM crystallizes → You know the dispo
```

### The Core ED Unsolved Problem

Current tools like Abridge assume **one session per visit**. The ED reality:
- You enter a room for 4 minutes at T+0
- Leave, see 3 other patients
- Come back at T+45 when labs return
- That 4-minute snippet + lab context = one logical note, built across 2 hours

The system must **understand patient context continuity** across temporal gaps and **pull external data into the right note slot** when it arrives.

### Capture Modes: Ambient vs. Dictation

ED clinicians don't always speak in the room. Two distinct capture modes exist:

| Mode | Context | Example |
|---|---|---|
| **Ambient** | In the room with the patient | "Tell me about the chest pain... when did it start?" |
| **Dictation** | Hallway, nursing station, workroom | Clinician privately narrates: "44yo M, STEMI negative, plan dispo home" |

Both produce note content, but ambient capture includes patient speech (potential noise, consent implications) while dictation is single-speaker and more structured. The app must support **explicit mode selection** — the clinician taps "In Room" vs "Dictate" — so the summarizer can use the right extraction strategy.

**The hallway problem:** ED docs constantly discuss patients with nurses, consultants, and residents at the nursing station. The app must **never auto-capture**. Recording is always explicit (tap to start, tap to stop). There is no always-on listening.

### Shift Lifecycle

A shift is the top-level container. Without it, notes accumulate indefinitely.

```
Shift Start (explicit) → Patient Board active → ... 8-12 hours ...
  → Shift End (explicit) → Unsigned notes flagged
  → All signed notes archived → Shift data eligible for purge
```

Key lifecycle events:
- **Start shift**: creates a new shift context, empty patient board
- **End shift**: flags any unsigned notes, exports shift summary, starts data retention timer
- **Between shifts**: device holds encrypted data for configurable retention period (default: 72 hours post-sign, then purge)

---

## 2. MVP Architecture: Rust Concurrency Model

### What Can Run in Parallel

```
┌─────────────────────────────────────────────────────────────┐
│                    TOKIO RUNTIME                            │
│                                                             │
│  ┌─────────────┐   ┌─────────────┐   ┌─────────────────┐  │
│  │  Audio Task  │   │  STT Task   │   │  Sync/Poll Task │  │
│  │  (capture)   │──▶│  (whisper   │   │  (lab results,  │  │
│  │  always-on   │   │   local or  │   │   FHIR later)   │  │
│  │  per patient │   │   cloud)    │   │                 │  │
│  └─────────────┘   └──────┬──────┘   └────────┬────────┘  │
│                            │                   │           │
│  ┌─────────────────────────▼───────────────────▼─────────┐ │
│  │            EVENT BUS  (tokio::sync::broadcast)        │ │
│  │  AudioChunk | Transcript | LabResult | VitalUpdate    │ │
│  └───────────────────────────┬───────────────────────────┘ │
│                               │                             │
│  ┌────────────────────────────▼──────────────────────────┐  │
│  │          NOTE STATE MACHINE  (sequential/locked)      │  │
│  │  PatientNoteStore: Arc<Mutex<HashMap<PatientId,Note>>> │  │
│  └───────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

**Parallelizable workloads:**
- Audio ring-buffer capture (dedicated thread, real-time priority)
- STT inference (CPU-bound, `tokio::task::spawn_blocking`)
- LLM summarization of snippets (I/O-bound, fully async)
- Polling for lab/result updates (async HTTP, interval timer)
- Audit log writes (fire-and-forget channel)
- UI rendering (separate thread, event-driven)

**Must be sequential/transactional:**
- Patient context switching (requires lock on active patient state)
- Note section mutations (append-only with version counter)
- Encounter ID assignment (monotonic, collision-free)
- Dispo/MDM finalization (explicit commit gate, not auto-generated)

### Rust Crate Selection

```toml
[dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }

# Audio capture
# NOTE: cpal iOS support is experimental. Fallback plan: capture audio in
# Swift (AVAudioEngine) and pass PCM buffers to Rust via FFI.
cpal = "0.15"
# Ring buffer for audio chunks
ringbuf = "0.4"

# STT - local first
whisper-rs = "0.12"   # whisper.cpp bindings (check latest before starting)

# LLM calls (Claude API for summarization)
reqwest = { version = "0.12", features = ["json", "stream"] }

# State / event sourcing
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["v4"] }

# Persistence (SQLite, on-device, WAL mode for crash resilience)
rusqlite = { version = "0.32", features = ["bundled"] }

# Encryption at rest (pick ONE — aes-gcm is simpler for our use case)
aes-gcm = "0.10"  # AES-256-GCM for SQLite-level or field-level encryption
# NOTE: For production, prefer SQLCipher (C library) via rusqlite feature
# flag rather than app-level encryption. SQLCipher encrypts the entire DB
# file transparently. For MVP spike, aes-gcm on sensitive fields is faster
# to implement.

# Mobile FFI bridge
uniffi = "0.28"   # generates Swift/Kotlin bindings from Rust

# Channels / concurrency (via tokio::sync: broadcast, mpsc, oneshot)
dashmap = "6"     # concurrent HashMap for read-heavy note lookups

# Time
chrono = { version = "0.4", features = ["serde"] }

# De-identification
regex = "1"       # PHI stripping before LLM calls
```

**Crate version policy:** Pin exact versions in `Cargo.lock`, use `~semver` in `Cargo.toml`. Before starting each phase, run `cargo outdated` and update. The versions above are approximate — verify against crates.io at build time.

**Why rusqlite over sqlx:** For a single-user mobile app, synchronous SQLite via rusqlite is simpler, has fewer dependencies, and avoids async-in-SQLite overhead. Use `spawn_blocking` for any DB call that might exceed 1ms.

### Core State Machine

```rust
// Patient note phase — must support CYCLES, not just linear progression.
// An ED patient is seen, left, seen again, left again — potentially 4-5 times.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
enum NotePhase {
    Registered,           // chart open, no audio yet
    Encountering,         // clinician in room or dictating, audio live
    Idle,                 // between encounters, waiting on data / other patients
    Finalizing,           // clinician triggered MDM/dispo, assembling final note
    PendingSignOff,       // draft complete, awaiting clinician review
    Signed,               // terminal state — immutable after this
}

// The valid transition graph (note the cycles):
//
//   Registered ──AudioStart──▶ Encountering
//                                  │
//                              AudioStop
//                                  │
//                                  ▼
//                                Idle ◀───────────────────┐
//                               ╱    ╲                    │
//                     AudioStart      TriggerMDM     AudioStop
//                             ╱            ╲              │
//                            ▼              ▼             │
//                     Encountering      Finalizing ──▶ (can return
//                                          │           to Encountering
//                                       ReviewReady    if clinician
//                                          │           re-enters room)
//                                          ▼
//                                    PendingSignOff
//                                          │
//                                        Sign
//                                          │
//                                          ▼
//                                        Signed
//
// Key insight: Idle ↔ Encountering can cycle indefinitely.
// Results (labs, imaging) can arrive in ANY phase except Signed.

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PatientNote {
    id: PatientId,
    phase: NotePhase,
    encounter_count: u32,                // how many times clinician has seen this patient
    encounters: Vec<Encounter>,          // append-only
    results: Vec<ClinicalResult>,        // append-only, arrive in any phase
    note_sections: NoteSections,         // HPI, exam, MDM, dispo
    disposition: Option<Disposition>,    // Admit | Discharge | Transfer | AMA | LWBS
    version: u64,                        // monotonic, for optimistic locking
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum Disposition {
    Discharge { instructions: String },
    Admit { service: String, bed: Option<String> },
    Transfer { destination: String },
    AMA,       // against medical advice
    LWBS,      // left without being seen
}

// Transitions are explicit — no ad-hoc mutations
impl PatientNote {
    fn transition(&mut self, event: NoteEvent) -> Result<(), TransitionError> {
        let valid = match (&self.phase, &event) {
            // From Registered
            (NotePhase::Registered, NoteEvent::AudioStarted { .. }) => {
                self.phase = NotePhase::Encountering;
                self.encounter_count += 1;
                true
            }
            // From Encountering
            (NotePhase::Encountering, NoteEvent::AudioStopped) => {
                self.phase = NotePhase::Idle;
                true
            }
            // From Idle — the critical cycle
            (NotePhase::Idle, NoteEvent::AudioStarted { .. }) => {
                self.phase = NotePhase::Encountering;
                self.encounter_count += 1;
                true
            }
            (NotePhase::Idle, NoteEvent::TriggerMDM) => {
                self.phase = NotePhase::Finalizing;
                true
            }
            // From Finalizing — can go back if needed
            (NotePhase::Finalizing, NoteEvent::AudioStarted { .. }) => {
                self.phase = NotePhase::Encountering;
                self.encounter_count += 1;
                true
            }
            (NotePhase::Finalizing, NoteEvent::ReviewReady) => {
                self.phase = NotePhase::PendingSignOff;
                true
            }
            // Sign off
            (NotePhase::PendingSignOff, NoteEvent::Sign) => {
                self.phase = NotePhase::Signed;
                true
            }
            _ => false,
        };

        if valid {
            self.version += 1;
            self.updated_at = Utc::now();
            self.log_transition(&event);
            Ok(())
        } else {
            Err(TransitionError::InvalidTransition {
                from: self.phase.clone(),
                event: event.clone(),
            })
        }
    }

    // Results can arrive in ANY non-terminal phase — not a state transition
    fn ingest_result(&mut self, result: ClinicalResult) -> Result<(), TransitionError> {
        if self.phase == NotePhase::Signed {
            return Err(TransitionError::NoteImmutable);
        }
        self.results.push(result);
        self.version += 1;
        self.updated_at = Utc::now();
        Ok(())
    }
}
```

### Concurrency Pattern: Encounter Audio Pipeline

```
  ┌──────────────────────────────────────────────────────────┐
  │  Audio thread (cpal callback, real-time)                 │
  │  Writes PCM chunks to lockless ring buffer               │
  └───────────────────┬──────────────────────────────────────┘
                      │ ringbuf::Consumer
  ┌───────────────────▼──────────────────────────────────────┐
  │  STT worker (spawn_blocking, whisper.cpp)                │
  │  Reads 30s windows*, produces Transcript { text, ts }   │
  └───────────────────┬──────────────────────────────────────┘
                      │ mpsc::Sender<Transcript>
  ┌───────────────────▼──────────────────────────────────────┐
  │  Encounter accumulator (async task)                      │
  │  Buffers transcripts per PatientId + EncounterId        │
  │  Flushes when: silence > 30s OR explicit stop            │
  └───────────────────┬──────────────────────────────────────┘
                      │ mpsc::Sender<EncounterChunk>
  ┌───────────────────▼──────────────────────────────────────┐
  │  Summarizer (async, Claude API call)                     │
  │  Prompt: "Extract HPI/exam/plan from ED encounter"       │
  │  Returns structured NoteSection                          │
  └───────────────────┬──────────────────────────────────────┘
                      │ mpsc::Sender<NoteSection>
  ┌───────────────────▼──────────────────────────────────────┐
  │  Note state writer (Arc<Mutex<PatientNoteStore>>)        │
  │  Appends section, increments version, writes to SQLite   │
  └──────────────────────────────────────────────────────────┘
```

**\*Audio chunk size rationale (30 seconds):**
- **Too short (5-10s):** Whisper loses sentence context, produces fragmented transcripts, higher overhead from repeated model warm-up
- **30s:** Good balance — enough context for complete sentences, Whisper processes in ~10s on-device (0.3x real-time), live transcript lag is acceptable
- **Too long (60s+):** Latency to first transcript exceeds 20s, feels unresponsive; also, if the clinician stops recording after 15s, the system waits for nothing
- **Adaptive approach (Phase 2):** Use Voice Activity Detection (VAD) to segment on silence gaps >2s rather than fixed windows. This produces more natural transcript boundaries aligned with speech patterns.

### Multi-Encounter Merge Strategy

When a patient has been seen 3 times, the summarizer holds 3 separate transcript chunks. How do they become one note?

**Strategy: Incremental append with periodic re-synthesis.**

```
Encounter 1 transcript → LLM → HPI v1, Exam v1
  (stored)

Encounter 2 transcript → LLM → HPI delta, Exam delta
  Merge: HPI v2 = LLM("Given prior HPI: {v1}, and new encounter: {enc2}, produce updated HPI")
  (stored, v1 kept for audit)

Encounter 3 transcript → same pattern → HPI v3
```

- Each encounter produces a **delta**, not a full rewrite
- A merge step combines the delta with the prior version
- The LLM prompt explicitly includes the prior section content + new encounter
- All versions are retained (append-only) — the UI shows the latest, but older versions are reviewable
- **At MDM time**, the system does one final re-synthesis across ALL encounters + ALL results to produce the complete note

**Why not re-summarize everything each time?** Cost and latency. A 3-encounter patient with labs would require re-processing ~5 minutes of transcript on every update. Incremental deltas keep each LLM call small (~30s of new content + ~200 tokens of prior context).

### Crash Recovery and Data Integrity

The app will crash. The phone will die. The clinician will force-kill it. Design for this.

- **SQLite WAL mode**: All writes use Write-Ahead Logging. Partial transactions are automatically rolled back on restart.
- **Audio buffer persistence**: The ring buffer drains to a temporary `.wav` file every 10 seconds. On crash, the last 10 seconds may be lost, but everything before that is on disk.
- **Recovery on launch**: App startup checks for orphaned audio files (recorded but not yet transcribed) and re-queues them for STT processing.
- **Note version pinning**: Every note mutation increments a version counter and writes to SQLite atomically. On crash, the note is at the last committed version — never in a half-updated state.
- **LLM call idempotency**: If a summarization request was sent but the response wasn't stored (crash during API call), the system re-sends the same transcript chunk. Claude produces a slightly different summary, which is fine — the clinician edits the final note anyway.

### Battery and Thermal Budget

Running Whisper inference + continuous audio capture on a phone has real power constraints.

| Component | Estimated Power Draw | Mitigation |
|---|---|---|
| Audio capture (mic + ADC) | Low (~50mW) | Negligible |
| Whisper small inference | High (~2-4W burst) | Only runs on 30s chunks, not continuous |
| Screen on (during recording) | ~500mW | Allow screen dimming during recording; keep waveform indicator minimal |
| Network (LLM API calls) | Moderate (~300mW burst) | Batch where possible; queue when offline |

**Estimated battery impact over 8-hour shift:**
- Active recording: ~30% of shift time (2.5 hours mic active)
- Whisper runs: ~50 chunks × 10s each = ~8 minutes of inference
- LLM calls: ~50 calls × 3s each = ~2.5 minutes of network
- **Total additional drain: ~15-25% of battery over full shift** (acceptable if phone starts at 80%+)

Mitigation: recommend clinician keep phone plugged in at workstation between encounters. The app should show a low-battery warning at 20% remaining.

### Cost Estimation (Claude API)

For an 8-hour ED shift with ~15 patients:

| Operation | Volume | Tokens (est.) | Cost (Sonnet) |
|---|---|---|---|
| Encounter summaries | ~40 calls (15 pts × ~2.5 encounters) | ~1500 in + 500 out each | ~$0.60 |
| MDM generation | ~15 calls | ~3000 in + 1000 out each | ~$0.30 |
| Note re-synthesis | ~15 calls | ~2000 in + 800 out each | ~$0.25 |
| **Total per shift** | | | **~$1.15** |

This is cheap enough for a single clinician to self-fund during pilot testing. At scale (100 clinicians), negotiate volume pricing or evaluate Haiku for the simpler summarization calls (~10x cheaper).

---

## 3. MVP Scope: Two Paths

### Path A — No-Integration MVP (Ship in 6 weeks)

**What you build:**
- Audio capture + local STT (Whisper small model, on-device)
- Manual patient creation: name/MRN stub + chief complaint
- "Simulated lab" injection: clinician taps "CBC back" and manually enters Hgb 7.2, WBC 14.1
- Note builder that merges encounter transcripts + manual lab entries
- Draft note displayed as editable text (HPI / Exam / MDM / Dispo sections)
- Export via iOS Share Sheet (copy to clipboard → paste into Epic). See **Note Export Format** below.

**What you explicitly skip:**
- EMR login, OAuth, SMART on FHIR
- Real lab feeds, HL7 interfaces
- Any cloud storage of PHI
- Fancy diarization (single-speaker assumed)

**Why this is enough to test:** Your friend can use it for a real shift, manually enter labs when they come back, and see whether the progressive note assembly saves time. The workflow friction surfaces immediately.

### Path B — Integration-Ready Design (No extra cost now, hooks ready later)

Design your data model with FHIR R4 resource shapes from day one:

```rust
// Instead of ad-hoc structs, model after FHIR resources
// so future sync is a mapping layer, not a rewrite

#[derive(Serialize, Deserialize)]
struct FhirObservation {
    resource_type: String,         // "Observation"
    id: String,
    status: ObservationStatus,
    code: CodeableConcept,         // LOINC code
    subject: Reference,            // Patient ref
    effective_date_time: DateTime,
    value_quantity: Option<Quantity>,
    // ...
}

// Your internal LabResult wraps or mirrors this
struct LabResult {
    fhir_id: Option<String>,       // None = manually entered
    loinc_code: String,
    display_name: String,
    value: f64,
    unit: String,
    reference_range: Option<(f64, f64)>,
    timestamp: DateTime<Utc>,
    source: ResultSource,          // Manual | FhirPull | HL7Feed
}
```

**FHIR hook points (stub now, implement later):**
- `FhirClient::get_results(patient_id, since: DateTime)` — returns empty vec in MVP
- `FhirClient::push_note_draft(patient_id, note: &str)` — no-op in MVP
- `EhrAuthProvider::get_token()` — returns test token in MVP

This means Phase 3 integration is a ~2-week plumbing job, not a rewrite.

### Note Export Format

The generated note must paste cleanly into Epic's text editor. Epic note fields accept plain text with minimal formatting.

**Default export template:**
```
ED PROVIDER NOTE

Chief Complaint: {cc}

HPI:
{hpi_text}

Review of Systems:
{ros_text | "See HPI"}

Physical Exam:
{exam_text}

Results Reviewed:
{results_list}

Medical Decision Making:
{mdm_text}

Disposition: {disposition}
{dispo_details}

---
Note assisted by AI documentation tool. All content reviewed and approved by signing provider.
```

**Configurability:** In Phase 2+, allow the clinician to customize section headers and ordering — different EDs have different note templates. Store the template as a JSON config on device. For MVP, hardcode the template above and iterate based on feedback.

**What NOT to include in the export:** timestamps, encounter IDs, version history, or any metadata. The export is the clinical note only. Audit trail stays on device.

### Android Consideration

This document assumes iOS for MVP (friend's device). However, the Rust core library + UniFFI architecture also generates Kotlin bindings. An Android port requires:

- Replacing `AVAudioSession` with Android `AudioRecord` API (or `Oboe` via Rust bindings)
- Replacing Secure Enclave with Android Keystore for encryption keys
- Replacing TestFlight with Firebase App Distribution or direct APK sideload
- SwiftUI host → Jetpack Compose host

**Estimated incremental cost to add Android after iOS MVP works: ~3-4 weeks.** The Rust core (state machine, note logic, LLM client, de-identifier, SQLite) is 100% shared.

---

## 4. Phase-by-Phase Roadmap

### Phase 0: Workflow Discovery + UX Prototype (Week 1–2)

**Deliverables:**
- Shadowing session with your friend (1 shift minimum): photograph the whiteboard, watch how he switches patients, note the exact moments he reaches for his phone vs can't
- Hand-drawn or Figma wireframes for: patient list, active encounter view, timeline view, note draft view
- Written "event taxonomy": every event that should update the note
- Decision on STT approach (local Whisper vs cloud API for Phase 1)

**Risks:**
- You discover the UX mental model doesn't match the implementation model → pivot early, cheap
- Clinician can't use any tool hands-free → forces voice-command or wearable design consideration

**Success criteria:**
- Your friend reviews wireframes and says "yes, that's the 3 screens I'd actually use"
- You have a list of the 5 most critical ED events to capture

---

### Phase 1: Core Capture + Encounter Segmentation (Week 3–6)

**Deliverables:**
- iOS app (via Tauri Mobile or pure Rust + UniFFI + Swift shell) with:
  - Patient list (manual create, 3 fields: name, DOB stub, chief complaint)
  - Active patient indicator (large, tappable, one-handed)
  - Record button: tap to start, tap to stop snippet
  - Real-time transcript display (streaming from Whisper)
  - Per-patient encounter log (time-stamped snippets)
- Rust core: `AudioCapture → RingBuffer → WhisperWorker → TranscriptAccumulator`
- SQLite schema: patients, encounters, transcript_chunks, note_sections
- No summarization yet — raw transcripts only

**Technical risks:**
- Whisper small model on iPhone 13: ~0.3x real-time (fine for 30s chunks with 10s latency)
- Background audio capture on iOS requires `AVAudioSession` entitlement + careful power management
- `cpal` iOS support is incomplete → likely need `coreaudio-sys` directly or bridge from Swift

**Success criteria:**
- Record a 2-minute mock encounter, switch to another patient, record another — both transcripts stored cleanly under correct patients
- Latency from stop-recording to transcript: <15 seconds on-device

---

### Phase 2: Progressive Note State + MDM Builder (Week 7–10)

**Deliverables:**
- LLM summarization pipeline (Claude API, streaming):
  - Input: encounter transcript + any results attached to that timestamp
  - Output: structured note sections (HPI bullet points, exam findings, assessment/plan skeleton)
  - Prompt template tuned for ED language and MDM requirements
- Note sections view: shows assembled HPI, Exam, MDM separately, each with the source encounters that contributed
- "MDM Builder" mode: triggered manually when clinician decides dispo is clear
  - Presents: chief complaint + relevant labs + exam findings + disposition
  - LLM generates MDM paragraph
  - Clinician edits inline
- Note state machine enforced: no MDM generation until clinician explicitly triggers it
- Timeline view: scrollable chronological list of [encounter snippet] + [lab result] + [note update] events

**Technical risks:**
- LLM prompt quality: ED notes have specific MDM criteria (3-element MDM for billing)
  - Mitigation: hard-code MDM template structure, LLM fills slots, clinician edits
- Privacy: Claude API call sends transcript text off-device
  - Mitigation in Phase 4; for Phase 2 testing: friend consents, use deidentified test cases only
- Note quality may be poor initially
  - Mitigation: ship the raw transcript too; note quality is a prompt engineering iteration loop

**Success criteria:**
- End-to-end: record encounter → stop → 20 seconds later see HPI bullets populated
- MDM builder produces a draft that needs <3 minutes of editing vs. writing from scratch

---

### Phase 3: Results Ingestion (Week 11–14)

**Deliverables, Path A (Manual + Structured Entry):**
- "Add Result" bottom sheet: searchable list of common ED labs (CBC, BMP, troponin, lactate, UA, lipase, BhCG)
- Numeric entry + auto-flag if outside reference range
- Result attached to patient timeline with timestamp
- Summarizer re-runs when new result arrives: "Update plan given new CBC showing Hgb 7.2"
- "Imaging result" text field: paste the radiology report blurb

**Deliverables, Path B (FHIR stub activated):**
- `FhirClient` implementation against a public sandbox (SMART Health IT sandbox, synthea data)
- OAuth PKCE flow (SMART on FHIR) — connect to sandbox only
- Background poll every 2 minutes for new Observations matching active patient MRNs
- Auto-inject results into patient timeline when poll succeeds
- Full Epic integration deferred (requires vendor agreement, their sandbox access, IT cooperation)

**Assumptions about Epic integration:**
- Epic FHIR R4 APIs exist but access requires: (a) organization agreement, (b) registered app in Epic App Orchard or their dev sandbox, (c) IT department involvement
- A physician can sometimes get "developer sandbox" access through their institution — worth asking Epic rep or CMO
- Do NOT promise Epic integration for the pilot; promise FHIR-ready architecture

**Success criteria:**
- Manual: add 5 results mid-shift simulation, note draft reflects all of them
- FHIR: successfully pull lab results from public Synthea sandbox patient into the note timeline

---

### Phase 4: Reliability, Offline Mode, Privacy/Security Hardening (Week 15–18)

**Deliverables:**
- Full offline operation: all note building works without network (Whisper is local, SQLite is local)
- Queue-and-sync: LLM calls queued when offline, batched when network returns
- Encryption at rest: SQLite database encrypted with SQLCipher (AES-256-GCM), key in iOS Secure Enclave
- Encryption in transit: TLS pinning for all API calls
- Audit log: every note mutation, patient access, and export logged immutably with timestamp + action type
- Session lock: auto-lock after 90 seconds idle, Face ID / biometric unlock
- De-identification for LLM calls: strip name/DOB/MRN before sending to Claude API (replace with stable pseudonyms for context continuity)
- Export control: notes exportable only to clipboard or direct-to-Epic-chart (no email, no photos)
- Consent screen: clear disclosure that AI assists but does NOT make clinical decisions; clinician is solely responsible

**Success criteria:**
- Kill network mid-session: all capture continues, note rebuilds from queue when network returns
- Penetration test (DIY): extract app sandbox, confirm database is unreadable without device unlock
- Audit log review: every action traceable

---

### Phase 5: Pilot Deployment + Feedback Loop (Week 19–24)

**Deliverables:**
- TestFlight distribution (single user, no App Store required for personal testing)
- Structured feedback template: per-shift rating of (1) time saved, (2) note quality, (3) friction points, (4) errors or scary moments
- A/B compare: shifts with tool vs without (weekly cadence)
- Bug triage channel (iMessage or Slack with your friend)
- v1.1, v1.2 iterations based on feedback
- Define go/no-go: "if note quality is rated 7/10 or better and saves >15 min/shift, pursue broader pilot"

---

## 5. Minimal UI/UX Flow for ED Reality

### Screen Architecture (3 core screens + 1 modal)

```
┌─────────────────────────────────────────────────────────────┐
│  SCREEN 1: PATIENT BOARD                                    │
│  ┌───────────────────────────────────────────────────────┐  │
│  │  [● REC]  Rm 4 - Smith, J      CC: Chest pain   [▶] │  │  ← Active patient bar, always visible
│  └───────────────────────────────────────────────────────┘  │
│                                                             │
│  ┌──────────────────────────────────────────────────────┐   │
│  │  Rm 2 - Garcia, M     Abd pain      ● 2 results new  │   │  ← Dot = results waiting
│  │  Rm 5 - Lee, K        SOB           ○ stable         │   │
│  │  Rm 9 - Patel, A      AMS           ◉ MDM ready      │   │  ← MDM ready indicator
│  └──────────────────────────────────────────────────────┘   │
│                                                             │
│  [+ New Patient]                                            │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│  SCREEN 2: ACTIVE ENCOUNTER (tap patient to enter)          │
│                                                             │
│  Smith, J  |  Rm 4  |  CC: Chest pain                      │
│  ──────────────────────────────────────                     │
│                                                             │
│  TIMELINE                                                   │
│  10:03  [🎙 Encounter]  "44yo M, SSCP onset 2h ago..."     │
│  10:47  [🧪 Troponin]   0.04 → NORMAL                      │
│  10:47  [🧪 EKG]        Normal sinus, no ST changes        │
│  11:15  [🎙 Reassess]   "Repeat pain 3/10, tolerating..."  │
│  11:32  [🧪 Troponin 2] 0.05 → NORMAL (delta negative)     │
│                                                             │
│  ─── NOTE DRAFT ────────────────────────────────────────── │
│  HPI: [auto-populated, editable] ▼                         │
│  Exam: [auto-populated, editable] ▼                        │
│  MDM: [tap to generate] ← not auto, always explicit        │
│                                                             │
│  ┌─────────────────────────────────────────────────────┐   │
│  │   [🎙 START SNIPPET]  ← large, center, thumb-reach  │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                             │
│  Quick tags: [Reassess] [Update Plan] [Dispo] [Add Result] │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│  MODAL: RECORDING ACTIVE                                    │
│                                                             │
│  ██████████████████  Recording...  00:00:47                 │
│                                                             │
│  ▪ Patient: Smith, J  (tap to switch without stopping)      │
│                                                             │
│  [▪▪▪ STOP]          [TAG: Reassess ▼]                     │
│                                                             │
│  Live transcript:                                           │
│  "...pain is now 3 out of 10, patient is more comfortable,  │
│   lungs clear bilaterally, will wait for second trop..."   │
└─────────────────────────────────────────────────────────────┘
```

### Key UX Decisions
- **One-handed operation**: record button is always thumb-reachable, large
- **Patient context switching without stopping recording**: tap patient name while recording → context switches, audio continues, now attributed to new patient
- **No auto-MDM**: MDM is always clinician-triggered. This is both a safety requirement and a trust-building feature
- **Result ingestion is non-blocking**: new lab result pops a subtle banner on the patient board, doesn't interrupt current activity
- **Note is always visible and editable**: never a black box; clinician owns the text
- **Never auto-capture**: no always-on listening. Recording requires explicit tap. This avoids hallway conversations, nursing station chatter, and patient privacy violations.

### Quick Exam Templates (Phase 2+)

ED docs often document the exam with templated normal findings. Instead of dictating "lungs clear to auscultation bilaterally, heart regular rate and rhythm, abdomen soft non-tender," offer quick-fill templates:

```
[Normal Cardiac] [Normal Resp] [Normal Neuro] [Normal Abd] [Normal MSK]
```

Each expands to a standard phrase. Clinician taps the normal templates for systems that are unremarkable, then dictates only the abnormal findings. Saves ~60 seconds per patient.

### Disposition Workflow

Dispo is the most important single field in an ED note. Support these explicitly:

| Disposition | Additional Fields Needed |
|---|---|
| **Discharge** | Discharge instructions (template or free-text), follow-up |
| **Admit** | Admitting service, bed request, handoff note |
| **Transfer** | Destination facility, reason, transport mode |
| **AMA** | AMA documentation template (risks discussed, capacity confirmed) |
| **LWBS** | Timestamp, chart notation |

The "Dispo" quick tag on the encounter screen opens a bottom sheet with these options. Once selected, the MDM builder uses the disposition to frame the decision-making narrative appropriately.

---

## 6. Privacy and Safety

### The Safety Boundary (Non-Negotiable)

**The system is a documentation assistant, not a clinical decision support tool.**

This must be explicit in:
- The onboarding consent screen
- The LLM system prompt ("You are transcribing and organizing clinical notes. Do NOT provide diagnostic recommendations, treatment advice, or drug dosing guidance. If asked, redirect to clinical references.")
- The app store description / TestFlight notes
- Every MDM draft: prefixed with "DRAFT — clinician review required"

### HIPAA Design Principles

| Category | Decision | Rationale |
|---|---|---|
| PHI storage | On-device SQLite only | No cloud PHI storage in MVP |
| LLM calls | De-identified before API call | Name → "Patient_A", DOB → age bucket, MRN → UUID |
| Audio storage | Ephemeral: deleted after STT completes | Raw audio is highest-risk PHI artifact |
| Encryption at rest | SQLCipher AES-256, key in Secure Enclave | Standard for healthcare mobile apps |
| Encryption in transit | TLS 1.3, certificate pinning | Prevent MITM on LLM API calls |
| Access control | Face ID / biometric gate, 90s auto-lock | Device theft scenario |
| Audit log | Append-only, signed, stored locally | Required for HIPAA Access logs |
| Export | Copy to clipboard only (or direct EMR) | No email, no files, no photos |
| Multi-device sync | Not in MVP | Dramatically reduces attack surface |

### De-identification Pipeline (Before Claude API Call)

```rust
use regex::Regex;

struct Deidentifier {
    patient_map: HashMap<PatientId, String>,
    counter: u8,
    // Precompiled regexes for PHI patterns
    mrn_re: Regex,       // e.g., 7-10 digit numbers
    phone_re: Regex,     // US phone patterns
    date_re: Regex,      // MM/DD/YYYY, YYYY-MM-DD, etc.
    ssn_re: Regex,       // XXX-XX-XXXX
}

impl Deidentifier {
    fn new() -> Self {
        Self {
            patient_map: HashMap::new(),
            counter: 0,
            mrn_re: Regex::new(r"\b\d{7,10}\b").unwrap(),
            phone_re: Regex::new(r"\(?\d{3}\)?[-.\s]?\d{3}[-.\s]?\d{4}").unwrap(),
            date_re: Regex::new(r"\b\d{1,2}/\d{1,2}/\d{2,4}\b").unwrap(),
            ssn_re: Regex::new(r"\b\d{3}-\d{2}-\d{4}\b").unwrap(),
        }
    }

    fn deidentify(&mut self, text: &str, patient: &PatientRecord) -> String {
        // Assign stable alias (Patient_A, Patient_B, ...)
        let counter = &mut self.counter;
        let alias = self.patient_map
            .entry(patient.id.clone())
            .or_insert_with(|| {
                let label = format!("Patient_{}", (b'A' + *counter) as char);
                *counter += 1;  // BUG FIX: counter must increment
                label
            })
            .clone();

        let mut result = text.to_string();

        // Replace patient name (first, last, and full)
        if let Some(ref name) = patient.last_name {
            result = result.replace(name, &alias);
        }
        if let Some(ref name) = patient.first_name {
            result = result.replace(name, &alias);
        }

        // Strip structured identifiers
        result = self.mrn_re.replace_all(&result, "[MRN]").to_string();
        result = self.phone_re.replace_all(&result, "[PHONE]").to_string();
        result = self.ssn_re.replace_all(&result, "[SSN]").to_string();

        // Dates → relative (preserves clinical meaning)
        // NOTE: This is a simplification. A production de-identifier would
        // need date-shifting logic that preserves intervals.
        result = self.date_re.replace_all(&result, "[DATE]").to_string();

        // Convert absolute age to age bucket for extra safety
        // "44 year old" → "40-49 year old" (HIPAA safe harbor: age < 90 is OK)
        // This is optional for MVP but recommended.

        result
    }
}
```

**De-identification limitations to be explicit about:**
- Regex-based stripping is not a certified de-identification method under HIPAA Safe Harbor (which requires 18 identifiers removed) or Expert Determination
- For MVP personal testing with clinician consent, this is acceptable
- For production: use a certified NLP-based de-identification service (e.g., AWS Comprehend Medical, or Presidio) as a pre-processing step
- Even with de-identification, the clinical narrative itself can be re-identifying in small populations ("the patient who fell off the Bay Bridge on Tuesday") — mitigate by using Anthropic's HIPAA-eligible API tier with a BAA in place

### Data Retention and Deletion

| Data Type | Retention | Trigger for Deletion |
|---|---|---|
| Raw audio (.wav temp files) | Deleted after STT completes | Automatic, immediate |
| Transcript chunks | 72 hours after note is signed | Automatic timer |
| Note versions (all) | 72 hours after note is signed | Automatic timer |
| Final signed note text | 7 days (allows late correction) | Automatic timer |
| Audit log | 30 days | Automatic timer, or manual export first |
| Shift metadata | 30 days | Automatic timer |

**Manual purge:** A "Wipe All Data" button in settings performs `VACUUM` + secure overwrite of the SQLite file. For device trade-in or loss scenarios.

**Why not keep data longer?** This is a documentation *assistant*, not an EHR. The source of truth is Epic. Once the note is pasted into the chart and signed there, the app's copy is redundant and becomes a liability.

### Consent Model

- Non-patient-facing tool (clinician's own device, personal use)
- Clinician must acknowledge: (1) AI produces drafts, not final notes; (2) they are responsible for everything that goes in the chart; (3) do not use for actual patient care until compliant infrastructure is in place
- For formal deployment: BAA with Claude API provider (Anthropic offers BAA for HIPAA-eligible customers)
- TestFlight personal testing: falls under personal use / research exemption with IRB consideration for formal study
- **Patient consent for ambient recording:** If using "In Room" mode, the institution's existing ambient recording policy applies. Many EDs already have consent language for ambient scribes (Abridge, Nuance DAX). For MVP testing, the clinician should only use "Dictate" mode (single-speaker, outside the room) to avoid patient consent complexity entirely.

---

## 7. Technical Spike Plan (First 2 Weeks)

### Week 1: Core Feasibility Experiments

**Spike 1: Whisper on-device latency (Day 1–2)**
```bash
# On the actual target device (or M-series Mac as proxy)
# Download whisper.cpp, compile, run against 30s audio samples
# Measure: time to first token, total latency, accuracy on medical vocabulary
# Success: <15s latency for 30s audio, >85% word accuracy on "troponin", "EKG", "diaphoresis"
```
- If fails: evaluate Whisper via API (OpenAI or Groq) as fallback; accept latency trade-off

**Spike 2: iOS audio capture in Rust (Day 2–3)**
```rust
// Minimal: cpal or coreaudio-sys → capture PCM → print chunk sizes
// Success: 16kHz mono PCM flowing without gaps, background-capable
// Known risk: iOS requires Swift entitlement setup; UniFFI bridge may need Swift host
```

**Spike 3: SQLite note state persistence (Day 3–4)**
```rust
// Create PatientNote, serialize to JSON, store in SQLite, retrieve
// Simulate: 10 patients, 5 encounters each, 20 results each
// Measure: query latency for "get all notes for patient X"
// Success: <50ms for patient note retrieval
```

**Spike 4: Claude API summarization quality (Day 4–5)**
- Write 3 realistic mock ED transcripts (use real clinician language from published sources or your friend dictates them)
- Send to Claude with ED note summarization prompt
- Measure: does output match expected HPI/Exam/MDM structure? Does it hallucinate findings not in transcript?
- Success: <5% hallucination rate on explicitly stated findings; output parseable into note sections

### Week 2: Integration Experiments

**Spike 5: UniFFI Swift bridge (Day 6–7)**
```
Rust library → cargo build for iOS targets (aarch64-apple-ios) → UniFFI generates Swift bindings → minimal SwiftUI host calls Rust function
Success: "Hello from Rust" rendered in SwiftUI label on physical iPhone
```
- If UniFFI is too heavy: evaluate `swift-bridge` crate or manual `extern "C"` FFI with Swift `@_silgen_name`

**Spike 6: Patient switching + audio attribution (Day 8–9)**
```
Scenario: Record 30s for Patient A → tap switch → Record 30s for Patient B → stop
Verify: both audio chunks end up under correct patient in DB
Test boundary: what happens if switch occurs mid-sentence?
```

**Spike 7: End-to-end mini-pipeline (Day 9–10)**
```
Audio → Whisper → transcript stored → Claude API → note section stored → displayed
Full latency measurement end-to-end
```
- Success metric: from "stop recording" to "note section visible in UI" < 30 seconds

### Measurement Framework

| Spike | Success Gate | Fallback if fails |
|---|---|---|
| Whisper latency | <15s for 30s audio on device | Use Groq API (fast cloud) |
| iOS audio Rust | Clean PCM, background OK | Swift audio, Rust for logic |
| SQLite perf | <50ms patient query | No-op, SQLite is fast |
| Claude quality | <5% hallucination | Tighter prompt, add examples |
| UniFFI bridge | Swift calls Rust, runs on phone | Swift host, Rust via XPC |
| Patient switching | Correct attribution | Require stop before switch |
| End-to-end | <30s total latency | Acceptable at <60s for MVP |

---

## 8. What I Would Build First

**The Smallest Testable Prototype:**

A single-screen iOS app that does exactly this:

1. Tap a patient name (hardcoded: "Patient A" and "Patient B")
2. Press the big red button → audio records, live transcript streams
3. Press stop → 20 seconds later, an HPI paragraph appears
4. Tap the other patient, repeat
5. Both patients show their separate HPI paragraphs

That's it. No labs, no MDM, no export, no timeline. Just: **does the audio-to-structured-note pipeline actually work for ED language on a real phone, fast enough to be useful?**

If your friend uses this for one shift and says "I could see myself using this if it also had X," you have product-market fit signal and a concrete next feature. If he says "the transcript quality is garbage" or "it's too slow," you've learned the critical failure mode before building anything else.

**Time to this prototype: ~2 weeks of focused engineering.**

```
Week 1: Audio capture → Whisper pipeline working in Rust on iOS
Week 2: Claude API call → structured note section → displayed in SwiftUI
```

**The one thing you must not skip:** have your friend record 10 minutes of realistic clinical speech (deidentified, scripted scenarios) before you start prompt engineering. The difference between "patient has a headache" and "44-year-old male with thunderclap onset cephalgia, 10/10, nausea, no prior similar" is everything for note quality. Tune the prompts against real ED vocabulary from day one.

---

## Assumptions and Open Questions

| Item | Assumption | What to Validate |
|---|---|---|
| Device | iPhone (friend's personal device) | iOS version, storage for Whisper model (~150MB) |
| Whisper model | `whisper-small` or `whisper-medium` | Medical vocabulary accuracy vs latency trade-off |
| LLM | Claude API (Sonnet 4.6 for quality, Haiku 4.5 for cost) | Cost per shift (~$1.15 Sonnet); BAA requirement if real PHI |
| iOS target | Direct UniFFI bridge, Swift host | UniFFI iOS support maturity (good as of 2024) |
| Audio | Single-speaker (clinician only) | If ambient patient speech causes problems → VAD filter |
| Network | Hospital WiFi or LTE | Offline mode essential if WiFi unreliable in rooms |
| Epic access | No direct access for MVP | Friend can screenshot labs and manually enter |
| Regulatory | Personal research tool, not commercial product | If friend wants to use routinely → IRB consideration |


# Project Structure

                                                                              
  src/                                                                         
  ├── lib.rs                  # Library root — 10 module declarations          
  ├── main.rs                 # Binary entry — creates shift, patient, note    
  store, verifies DB                                                           
  ├── error.rs                # AppError enum (9 variants) via thiserror       
  ├── models/                                                             
  │   ├── mod.rs              # Re-exports all types                           
  │   ├── patient.rs          # PatientId, PatientRecord                       
  │   ├── note.rs             # PatientNote, NotePhase (6 states), NoteSections
  │   ├── encounter.rs        # Encounter, CaptureMode (Ambient/Dictation)     
  │   ├── result.rs           # ClinicalResult, LabResult, ImagingResult,      
  VitalSign                                                               
  │   ├── disposition.rs      # Disposition (Discharge/Admit/Transfer/AMA/LWBS)
  │   ├── shift.rs            # Shift lifecycle                           
  │   └── audit.rs            # AuditEntry, AuditAction                        
  ├── state.rs                # FSM transitions (with Idle↔Encountering        
  cycles) + PatientNoteStore                                                   
  ├── events.rs               # NoteEvent enum + broadcast EventBus            
  ├── audio.rs                # AudioCapture trait + MockAudioCapture (incl.
  patient switching)
  ├── stt.rs                  # Transcriber trait + MockTranscriber
  ├── summarizer.rs           # Summarizer trait + MockSummarizer (async)
  ├── deidentify.rs           # Deidentifier with regex PHI stripping (3 tests)
  ├── db.rs                   # SQLite schema (6 tables, 5 indexes, WAL mode)
  (2 tests)
  └── export.rs               # Plain-text note formatter for Epic paste (1
  test)
