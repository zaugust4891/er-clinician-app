# Creative Improvements to the Persistence & Replay Plan

*Generated 2026-03-13 from codebase analysis + plan review*

---

## Context

The persistence/replay implementation plan is thorough and well-reasoned. The recommended choices (StoredEvent wrapper, full replay with event schema fix, phase-transition snapshots) are all sound. These creative improvements don't replace those recommendations — they address blind spots and structural opportunities the plan hasn't considered.

Each improvement is tagged with urgency:
- **NOW** — should be folded into the current implementation phases
- **BEFORE PRODUCTION** — must happen before real patient data enters the system
- **FUTURE** — architecturally valuable, can wait

---

## 1. Embed Clinical Timestamps in Events (NOW)

### The Problem

The plan acknowledges timestamp imprecision during replay as "acceptable for Phase 1":

> When replaying AudioStarted, the new Encounter's started_at will be set to Utc::now() (replay time), not the original recording time.

For a medical legal document, this is not a Phase 1 deferral — it's a structural defect. The time a clinician started recording is clinically meaningful. If a patient codes at 14:32 and the encounter shows as starting at 14:47 (when replay ran), that's a documentation integrity issue.

### The Fix

Add `occurred_at: DateTime<Utc>` to every NoteEvent variant. This is the *clinical timestamp* — when the thing actually happened. StoredEvent.occurred_at is the *persistence timestamp* — when the event was written to SQLite. These are different concepts that happen to be equal on first write but diverge on replay.

```rust
pub enum NoteEvent {
    AudioStarted {
        patient_id: PatientId,
        capture_mode: CaptureMode,
        encounter_id: Uuid,
        occurred_at: DateTime<Utc>,  // clinical timestamp
    },
    AudioStopped {
        patient_id: PatientId,
        encounter_id: Uuid,
        occurred_at: DateTime<Utc>,
    },
    // ... same pattern for all variants
}
```

During replay, `apply_event` uses `event.occurred_at` for `Encounter.started_at` and `ended_at`, not `Utc::now()`. The diff is small. The payoff is correctness.

### Why Now

Adding this later requires either:
1. An event migration for all stored events (painful)
2. A fallback to StoredEvent.occurred_at when the event field is missing (complexity)

Since there are zero production events, adding the field now costs one line per variant. Adding it later costs a migration strategy.

---

## 2. Separate Command Events from Fact Events (NOW)

### The Problem

NoteEvent conflates two fundamentally different kinds of events:

**Command events** drive the state machine:
- AudioStarted, AudioStopped, TriggerMDM, ReviewReady, Sign

**Fact events** carry domain data:
- TranscriptReady, ResultReceived

The plan's encounter_id problem is a symptom of this conflation. AudioStarted is a command ("start recording") that also needs to establish an encounter identity for subsequent facts to reference. The command and the data creation are coupled in one event.

### The Insight

In the current state machine, `handle_audio_started()` increments `encounter_count` but doesn't create an `Encounter`. The Encounter is created... nowhere in the state machine. The plan proposes creating it in `apply_event`, but this mixes replay mechanics with domain construction.

### The Fix

Instead of adding `encounter_id` to AudioStarted, introduce an `EncounterCreated` fact event:

```rust
pub enum NoteEvent {
    // Commands (drive state machine)
    AudioStarted { patient_id: PatientId, capture_mode: CaptureMode, occurred_at: DateTime<Utc> },
    AudioStopped { patient_id: PatientId, occurred_at: DateTime<Utc> },
    TriggerMDM { patient_id: PatientId, occurred_at: DateTime<Utc> },
    ReviewReady { patient_id: PatientId, occurred_at: DateTime<Utc> },
    Sign { patient_id: PatientId, occurred_at: DateTime<Utc> },

    // Facts (carry domain data, always emitted alongside or after a command)
    EncounterCreated {
        patient_id: PatientId,
        encounter_id: Uuid,
        capture_mode: CaptureMode,
        encounter_number: u32,
        occurred_at: DateTime<Utc>,
    },
    EncounterEnded {
        patient_id: PatientId,
        encounter_id: Uuid,
        occurred_at: DateTime<Utc>,
    },
    TranscriptReady { patient_id: PatientId, encounter_id: Uuid, text: String, occurred_at: DateTime<Utc> },
    ResultReceived { patient_id: PatientId, result: ClinicalResult, occurred_at: DateTime<Utc> },
}
```

The publish flow becomes:
1. Caller publishes `AudioStarted` (drives state machine transition)
2. Caller publishes `EncounterCreated` (carries the encounter data)
3. On stop: publish `AudioStopped` (state transition) + `EncounterEnded` (data)

`apply_event` cleanly separates: command events call `handle_*` methods, fact events mutate domain data. No mixing.

### Trade-off

More event types. More events per action (2 instead of 1 for start/stop). But the replay logic becomes trivially correct — each event does exactly one thing. And the encounter_id problem disappears because EncounterCreated is a fact event that *carries* the id, not a command event that needs one smuggled in.

### Alternative (Simpler)

If the dual-event approach feels heavy, at minimum keep the plan's approach (encounter_id on AudioStarted) but recognize that `apply_event` should NOT call `handle_audio_started()` AND create an Encounter in the same match arm. Instead, have the state transition methods themselves create the Encounter when they bump encounter_count. This moves domain construction into the state machine where it belongs:

```rust
// In Registered's on_audio_started:
fn on_audio_started(self: Box<Self>, note: &mut PatientNote) -> Box<dyn NoteState> {
    note.encounter_count += 1;
    // State machine creates the encounter — not apply_event
    // But it needs the encounter_id... which means the state transition
    // needs event data. This is the tension the dual-event model resolves.
    Box::new(Encountering)
}
```

The tension is real: the state machine doesn't have access to event data. The dual-event model resolves it cleanly. The plan's approach (apply_event does both) works but mixes concerns.

---

## 3. The Addendum Problem (BEFORE PRODUCTION)

### The Problem

`Signed` is terminal in the state machine. But in real ED practice:

- Late lab results arrive after sign-off (troponin at 4 hours, cultures at 48 hours)
- Consulting physicians add findings
- The patient returns to the ED and the prior note needs annotation
- Radiology over-reads change a preliminary reading

The doctor feedback document mentions these patterns. The state machine has no path for them.

### Why This Matters for Persistence

If `Signed` is truly terminal, then ResultReceived events for a signed patient have nowhere to go. The EventBus will persist them (the repo doesn't check phase), but replay will silently drop them (the state machine's `can_ingest_results()` returns false for Signed).

This means: events exist in the database that replay ignores. The event stream and the materialized state diverge. This violates the core event-sourcing guarantee.

### The Fix (Two Options)

**Option A: Addendum as a new phase**

```
Signed ──AddendumReceived──> Amended ──Sign──> Signed
```

An addendum reopens the note in a restricted mode. Only data-carrying events (ResultReceived, new TranscriptReady for an addendum dictation) are accepted. A second Sign re-seals it. The audit log captures each cycle.

**Option B: Addendum as a parallel stream**

Don't reopen the state machine. Instead, addenda are first-class objects that reference the original note:

```rust
pub struct Addendum {
    pub id: Uuid,
    pub note_id: Uuid,
    pub patient_id: PatientId,
    pub reason: String,        // "Late lab result", "Consulting note"
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub signed_at: Option<DateTime<Utc>>,
}
```

Addenda have their own events and their own mini-state-machine (Draft -> Signed). The original note remains immutable.

**Recommendation:** Option B. It matches how addenda work in real EMRs — they're separate signed documents, not modifications to the original. And it doesn't require touching the existing state machine.

**For now:** At minimum, add an `AddendumAdded` event variant so that late-arriving data has a place in the event stream that replay can process correctly.

---

## 4. Event Compaction for PHI Minimization (BEFORE PRODUCTION)

### The Problem

`TranscriptReady` events contain raw patient speech: names, dates of birth, medications, family history, sexual history, substance use — the most sensitive categories of PHI. The persistence plan stores these indefinitely in the events table as JSON payloads.

After summarization, the transcript's clinical value is captured in `note_sections`. The raw transcript becomes a liability:
- HIPAA minimum necessary principle: don't retain PHI beyond what's needed
- Right to deletion requests (state privacy laws)
- Breach surface area: the events table becomes the highest-value target

### The Fix

Introduce event compaction as a post-sign lifecycle:

```
[Signed] ──after retention period──> [Compacted]
```

Compaction replaces `TranscriptReady` payloads with a tombstone:

```rust
TranscriptCompacted {
    patient_id: PatientId,
    encounter_id: Uuid,
    original_sequence_id: u64,
    summary_hash: String,  // SHA-256 of the note_sections at sign time
    compacted_at: DateTime<Utc>,
}
```

The event stream remains structurally complete (sequence_ids unbroken, event_type preserved) but the raw PHI is gone. Replay after compaction produces a PatientNote with encounters that have no transcript field — but the note_sections (populated from the snapshot) have all the clinical content.

### Implementation

This doesn't need to be built in the current phase. But the schema should be designed to allow it:
- Don't assume TranscriptReady.text is always present — make the replay tolerant of empty/tombstoned transcripts
- Consider storing transcripts in a separate table with a foreign key to events.sequence_id, so compaction is a DELETE on the transcripts table rather than an UPDATE on events (append-only events stay append-only)

---

## 5. Snapshot Integrity Verification (NOW)

### The Problem

The plan mentions corrupted snapshots as a "defer if too complex" failure mode test. For a medical records system, snapshot corruption is not deferrable — it's the primary failure mode you need to handle.

The risk: `serde_json::from_str` fails on a corrupted snapshot. Without a fallback, `load_patient_note` returns `Err(Serialization(...))`. The patient's note is unrecoverable via the normal path, even though the event stream is perfectly intact.

### The Fix

Add a checksum to snapshots:

```sql
CREATE TABLE IF NOT EXISTS snapshots (
    patient_id      TEXT PRIMARY KEY,
    sequence_id     INTEGER NOT NULL,
    snapshot        TEXT NOT NULL,
    checksum        TEXT NOT NULL,      -- SHA-256 of snapshot text
    created_at      TEXT NOT NULL
);
```

`load_patient_note` becomes:

```rust
pub fn load_patient_note(repo: &dyn Repository, patient_id: PatientId) -> AppResult<Option<PatientNote>> {
    match repo.get_latest_snapshot(patient_id)? {
        Some((seq_id, note)) => {
            // Snapshot loaded and deserialized successfully
            let delta = repo.get_events_since(patient_id, seq_id)?;
            let mut note = note;
            for stored in &delta {
                apply_event(&mut note, &stored.event);
            }
            Ok(Some(note))
        }
        None => {
            // No valid snapshot — full replay
            let events = repo.get_events(patient_id)?;
            if events.is_empty() {
                return Ok(None);
            }
            let event_refs: Vec<&NoteEvent> = events.iter().map(|se| &se.event).collect();
            Ok(Some(replay(patient_id, &event_refs)))
        }
    }
}
```

The key: `get_latest_snapshot` should verify the checksum internally and return `None` (not `Err`) if the snapshot is corrupt. This triggers full replay as a natural fallback. The corruption is logged but doesn't block the clinician.

### Why Now

This is ~5 lines in `save_snapshot` (compute hash) and ~3 lines in `get_latest_snapshot` (verify hash, return None on mismatch). Small investment, critical safety net.

---

## 6. Replay Validation Event (FUTURE)

### The Concept

After replay, you have a PatientNote. But how do you know it's correct? The state machine silently absorbs invalid transitions. If events are out of order or corrupted, you get *a* PatientNote — just not the right one.

Add a `ReplayValidated` meta-event that replay emits after successful reconstruction:

```rust
pub struct ReplayResult {
    pub note: PatientNote,
    pub events_replayed: u64,
    pub events_skipped: u64,      // invalid transitions that were no-ops
    pub from_snapshot: bool,
    pub snapshot_sequence_id: Option<u64>,
    pub final_sequence_id: u64,
}
```

If `events_skipped > 0`, something unexpected happened. Log it. Alert on it. For a medical system, silent absorption of "invalid" events should be auditable, not invisible.

---

## 7. The DashMap Race Condition (NOW)

### The Problem

The plan doesn't address how `PatientNoteStore` (DashMap) interacts with the persistence layer. The current architecture has two sources of truth:

1. DashMap — in-memory, for UI reads
2. SQLite events table — persistent, for recovery

But nothing keeps them in sync. The EventBus persists to SQLite and broadcasts. Who updates the DashMap? If a subscriber updates the DashMap, what happens if it crashes between the broadcast and the DashMap write? The SQLite has the event but the DashMap doesn't reflect it.

On restart, `load_patient_note` rebuilds from SQLite. But during normal operation, the DashMap is the live projection. If it diverges from SQLite (missed broadcast, subscriber crash), the clinician sees stale data.

### The Fix

Make the DashMap update *part of* the persist-then-broadcast flow, not a subscriber responsibility:

```rust
pub fn publish(&self, event: NoteEvent, store: &PatientNoteStore) -> AppResult<()> {
    // 1. Persist
    if let Some(repo) = &self.repository {
        repo.append_event(event.extract_patient_id(), event.clone())?;
    }

    // 2. Update in-memory projection (same thread, same transaction boundary)
    let patient_id = event.extract_patient_id();
    store.entry(patient_id)
        .and_modify(|note| apply_event(note, &event))
        .or_insert_with(|| {
            let mut note = PatientNote::new(patient_id);
            apply_event(&mut note, &event);
            note
        });

    // 3. Broadcast (for UI notifications, logging, etc. — not for state updates)
    let _ = self.sender.send(event);
    Ok(())
}
```

Now the DashMap and SQLite are always consistent because they're updated in the same code path. Broadcast becomes purely a notification mechanism, not a state-update channel.

### Trade-off

This couples EventBus to PatientNoteStore. If that coupling feels wrong, extract a `Projector` that EventBus delegates to. But the core insight stands: the in-memory projection must be updated synchronously with persistence, not asynchronously via broadcast.

---

## Summary of Recommendations by Phase

| Plan Phase | Creative Improvement | Urgency |
|------------|---------------------|---------|
| Phase 1 (encounter_id) | Embed `occurred_at` in all event variants | NOW |
| Phase 1 (encounter_id) | Consider command/fact event separation | NOW (or accept the plan's simpler approach) |
| Phase 2 (Repository trait) | Add checksum column to snapshots schema | NOW |
| Phase 3 (SqliteRepository) | Verify checksum in get_latest_snapshot, return None on mismatch | NOW |
| Phase 4 (replay) | Use event's occurred_at for Encounter timestamps, not Utc::now() | NOW |
| Phase 4 (replay) | Return ReplayResult with skip count, not bare PatientNote | FUTURE |
| Phase 5 (integration) | Test DashMap consistency with EventBus persistence | NOW |
| New Phase | Design addendum model (at least the event variant) | BEFORE PRODUCTION |
| New Phase | Design transcript compaction strategy | BEFORE PRODUCTION |
| New Phase | Synchronous DashMap projection in EventBus.publish() | NOW |

---

## The Meta-Insight

The plan is excellent at answering "how do we persist and replay events?" But it's optimized for *correctness of the mechanism* (event sourcing works) rather than *correctness of the domain* (medical documentation is legally defensible, PHI-safe, and handles real clinical workflows).

The creative improvements above bridge that gap: timestamps make it legally defensible, compaction makes it PHI-safe, addenda make it clinically realistic, and DashMap sync makes it operationally correct.
