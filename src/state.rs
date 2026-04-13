use std::sync::Arc;

use chrono::Utc;
use dashmap::DashMap;

use crate::error::{AppError, AppResult};
use crate::events::NoteEvent;
use crate::models::audit::{AuditAction, AuditEntry};
use crate::models::note::NotePhase;
use crate::models::patient::PatientId;
use crate::models::result::ClinicalResult;
use crate::models::{Encounter, PatientNote};
use crate::repository::Repository;
// ---------------------------------------------------------------------------
// NoteState trait — the core of the state pattern
// ---------------------------------------------------------------------------

/// Each note phase is a struct implementing this trait.
/// Transitions consume `self: Box<Self>` and return the next state.
/// Invalid transitions use the default impl (return `self`, no-op).
///
/// `Send + Sync` bounds are required because `PatientNote` lives in a
/// `DashMap` shared across threads.
pub trait NoteState: Send + Sync {
    /// Default: no-op transition — return a clone of the current state.
    /// We use `clone_state()` instead of returning `self` because
    /// `Box<Self> -> Box<dyn NoteState>` requires `Self: Sized`, which
    /// would make the method non-dispatchable through `dyn NoteState`.
    fn on_audio_started(self: Box<Self>, _note: &mut PatientNote) -> Box<dyn NoteState> {
        self.clone_state()
    }

    fn on_audio_stopped(self: Box<Self>, _note: &mut PatientNote) -> Box<dyn NoteState> {
        self.clone_state()
    }

    fn on_trigger_mdm(self: Box<Self>, _note: &mut PatientNote) -> Box<dyn NoteState> {
        self.clone_state()
    }

    fn on_review_ready(self: Box<Self>, _note: &mut PatientNote) -> Box<dyn NoteState> {
        self.clone_state()
    }

    fn on_sign(self: Box<Self>, _note: &mut PatientNote) -> Box<dyn NoteState> {
        self.clone_state()
    }

    /// Can results be ingested in this state?
    fn can_ingest_results(&self) -> bool {
        true
    }

    /// Name for display and logging.
    fn phase_name(&self) -> &'static str;

    /// Convert back to the serialization enum.
    fn to_phase(&self) -> NotePhase;

    /// Clone this state into a new Box.
    fn clone_state(&self) -> Box<dyn NoteState>;
}

// ---------------------------------------------------------------------------
// State structs — one per NotePhase variant
// ---------------------------------------------------------------------------

pub struct Registered;
pub struct Encountering;
pub struct Idle;
pub struct Finalizing;
pub struct PendingSignOff;
pub struct Signed;

impl NoteState for Registered {
    fn on_audio_started(self: Box<Self>, note: &mut PatientNote) -> Box<dyn NoteState> {
        note.encounter_count += 1;
        Box::new(Encountering)
    }

    fn phase_name(&self) -> &'static str {
        "Registered"
    }
    fn to_phase(&self) -> NotePhase {
        NotePhase::Registered
    }
    fn clone_state(&self) -> Box<dyn NoteState> {
        Box::new(Registered)
    }
}

impl NoteState for Encountering {
    fn on_audio_stopped(self: Box<Self>, _note: &mut PatientNote) -> Box<dyn NoteState> {
        Box::new(Idle)
    }

    fn phase_name(&self) -> &'static str {
        "Encountering"
    }
    fn to_phase(&self) -> NotePhase {
        NotePhase::Encountering
    }
    fn clone_state(&self) -> Box<dyn NoteState> {
        Box::new(Encountering)
    }
}

impl NoteState for Idle {
    fn on_audio_started(self: Box<Self>, note: &mut PatientNote) -> Box<dyn NoteState> {
        note.encounter_count += 1;
        Box::new(Encountering)
    }

    fn on_trigger_mdm(self: Box<Self>, _note: &mut PatientNote) -> Box<dyn NoteState> {
        Box::new(Finalizing)
    }

    fn phase_name(&self) -> &'static str {
        "Idle"
    }
    fn to_phase(&self) -> NotePhase {
        NotePhase::Idle
    }
    fn clone_state(&self) -> Box<dyn NoteState> {
        Box::new(Idle)
    }
}

impl NoteState for Finalizing {
    fn on_audio_started(self: Box<Self>, note: &mut PatientNote) -> Box<dyn NoteState> {
        note.encounter_count += 1;
        Box::new(Encountering)
    }

    fn on_review_ready(self: Box<Self>, _note: &mut PatientNote) -> Box<dyn NoteState> {
        Box::new(PendingSignOff)
    }

    fn phase_name(&self) -> &'static str {
        "Finalizing"
    }
    fn to_phase(&self) -> NotePhase {
        NotePhase::Finalizing
    }
    fn clone_state(&self) -> Box<dyn NoteState> {
        Box::new(Finalizing)
    }
}

impl NoteState for PendingSignOff {
    fn on_sign(self: Box<Self>, _note: &mut PatientNote) -> Box<dyn NoteState> {
        Box::new(Signed)
    }

    fn phase_name(&self) -> &'static str {
        "PendingSignOff"
    }
    fn to_phase(&self) -> NotePhase {
        NotePhase::PendingSignOff
    }
    fn clone_state(&self) -> Box<dyn NoteState> {
        Box::new(PendingSignOff)
    }
}

impl NoteState for Signed {
    fn can_ingest_results(&self) -> bool {
        false
    }

    fn phase_name(&self) -> &'static str {
        "Signed"
    }
    fn to_phase(&self) -> NotePhase {
        NotePhase::Signed
    }
    fn clone_state(&self) -> Box<dyn NoteState> {
        Box::new(Signed)
    }
}

// ---------------------------------------------------------------------------
// Reconstruction helper — used by hydrate() and repository deserialization
// ---------------------------------------------------------------------------

/// Reconstruct a state trait object from the serialization enum.
pub fn state_from_phase(phase: &NotePhase) -> Box<dyn NoteState> {
    match phase {
        NotePhase::Registered => Box::new(Registered),
        NotePhase::Encountering => Box::new(Encountering),
        NotePhase::Idle => Box::new(Idle),
        NotePhase::Finalizing => Box::new(Finalizing),
        NotePhase::PendingSignOff => Box::new(PendingSignOff),
        NotePhase::Signed => Box::new(Signed),
    }
}

/// Reconstruct a PatientNote from a sequence of events, starting from
/// a fresh Registered state. Events must be in order (caller guarantees
/// this by querying with ORDER BY sequence_id ASC).
pub fn replay(patient_id: PatientId, events: &[NoteEvent]) -> PatientNote {
    let mut note = PatientNote::new(patient_id);
    for event in events {
        apply_event(&mut note, event);
    }
    note
}

// ---------------------------------------------------------------------------
// Patient note store
// ---------------------------------------------------------------------------

/// Concurrent store for all active patient notes.
/// Uses DashMap for lock-free reads — the common case is reading note state for UI.
pub type PatientNoteStore = Arc<DashMap<PatientId, PatientNote>>;

/// Create a new empty store.
pub fn new_store() -> PatientNoteStore {
    Arc::new(DashMap::new())
}

// ---------------------------------------------------------------------------
// Operations that use the state pattern
// ---------------------------------------------------------------------------

/// Ingest a clinical result into a note.
/// Delegates to the current state's `can_ingest_results()` method.
pub fn ingest_result(note: &mut PatientNote, result: ClinicalResult) -> AppResult<()> {
    let can_ingest = note
        .state
        .as_ref()
        .map(|s| s.can_ingest_results())
        .unwrap_or(false);

    if !can_ingest {
        return Err(AppError::NoteImmutable);
    }

    note.results.push(result);
    note.version += 1;
    note.updated_at = Utc::now();
    note.audit_log
        .push(AuditEntry::new(AuditAction::ResultIngested, ""));
    Ok(())
}

pub fn apply_event(note: &mut PatientNote, event: &NoteEvent) {
    match event {
        NoteEvent::AudioStarted { 
            encounter_id,
            capture_mode,
            clinical_occurred_at,
            ..
        } => {
            // Drive the state machine first so the encounter_count bumps
            // handle_audio_started calls self.apply_transition()
            // which takes the state out, appl
            note.handle_audio_started();

            note.encounters.push(Encounter {
                id: *encounter_id,
                patient_id: note.patient_id,
                capture_mode: capture_mode.clone(),
                started_at: *clinical_occurred_at,
                ended_at: None,
                transcript: None,
                encounter_number: note.encounter_count,
            });
    
        }

        NoteEvent::AudioStopped { 
            encounter_id,
            clinical_occurred_at,
            .. 
        } => {
            note.handle_audio_stopped();
            if let Some(enc) = note.encounters.iter_mut().find(|e| e.id == *encounter_id) {
                    enc.ended_at = Some(*clinical_occurred_at);
            }
            // If no matching encounter: silently ignore. Replay tolerates
            // events referencing missing encounters (should never happen
            // in a healthy stream, but panicking on bad data is worse).
        }

        NoteEvent::TranscriptReady { 
            encounter_id, 
            text,
            .. 
        } => {
            if let Some(enc) = note.encounters.iter_mut().find(|e| e.id == *encounter_id) {
                enc.transcript = Some(text.clone());
            }
        }

        NoteEvent::ResultReceived { 
            result,
            ..
        } => {
            let _ = ingest_result(note, result.clone());
        }
        
        NoteEvent::TriggerMDM { .. } => note.handle_trigger_mdm(),
        NoteEvent::ReviewReady { .. } => note.handle_review_ready(),
        NoteEvent::Sign { .. } => note.handle_sign(),    
     }
}

/// Load a PatientNote from the repository. Uses the snapshot fast-path
/// when available, falls back to full event replay when not. Returns
/// None if no events exist for this patient (fresh/unknown patient).
pub fn load_patient_note(repo: &dyn Repository, patient_id: PatientId) -> AppResult<Option<PatientNote>> {
    // Fast path: snapshot exists and checksum validates.
    if let Some((sequence_id, mut note)) = repo.get_latest_snapshot(patient_id)? {
        let delta = repo.get_events_since(patient_id, sequence_id)?;
        for stored in &delta {
            apply_event(&mut note, &stored.event);
        }
        return Ok(Some(note));
    }

    // Slow path: no snapshot (or checksum mismatch -> Ok(None) from repo).
    // Full replay from the event stream.
    let events = repo.get_events(patient_id)?;
    if events.is_empty() {
        return Ok(None);
    }

    let event_refs: Vec<&NoteEvent> = events.iter().map(|se| &se.event).collect();
    // replay() takes a slice of NoteEvent, not &NoteEvent - collect into owned.
    let owned: Vec<NoteEvent> = event_refs.into_iter().cloned().collect();
    Ok(Some(replay(patient_id, &owned)))

    /* NOTE:
    The `clone` is a minor inefficiency. If it becomes a hot path (it shouldn't — this runs at cold start, not per-event),
    switch `replay` to take `&[&NoteEvent]` or an iterator.

    Dependency:vThis function must be importable without a circular dep. `state.rs` 
    already depends on `events.rs` for `NoteEvent`
    and will now additionally depend on `repository::{Repository, StoredEvent}`. 
    That's fine — `repository.rs` depends on `state` (via `PatientNote`), 
    but only for types, not for this function. The dependency graph stays acyclic.
    */
}


// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::models::CaptureMode;
    use crate::models::{LabResult, ResultSource, ResultType};

    use super::*;

    fn build_test_lab_result(patient_id: PatientId) -> ClinicalResult {
        ClinicalResult {
            id: Uuid::new_v4(),
            patient_id,
            source: ResultSource::Manual,
            timestamp: Utc::now(),
            result_type: ResultType::Lab(LabResult {
                fhir_id: None,
                loinc_code: None,
                display_name: "Troponin".into(),
                value: 0.04,
                unit: "ng/mL".into(),
                reference_range: Some((0.0, 0.04)),
                is_abnormal: false,
            }),
        }
    }

    #[test]
    fn apply_event_audio_started_creates_encounter() {
        let mut note = PatientNote::new(Uuid::new_v4());
        let encounter_id = Uuid::new_v4();
        let event = NoteEvent::audio_started(
            note.patient_id,
            encounter_id,
            CaptureMode::Ambient,
        );

        apply_event(&mut note, &event);

        assert_eq!(note.phase, NotePhase::Encountering);
        assert_eq!(note.encounters.len(), 1);
        assert_eq!(note.encounters[0].id, encounter_id);
        assert_eq!(note.encounter_count, 1);
    }

    #[test]
    fn apply_event_audio_stopped_closes_encounter() {
        let mut note = PatientNote::new(Uuid::new_v4());
        let patient_id = note.patient_id;
        let encounter_id = Uuid::new_v4();
        apply_event(&mut note, &NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient));
        apply_event(&mut note, &NoteEvent::audio_stopped(patient_id, encounter_id));

        assert_eq!(note.phase, NotePhase::Idle);
        assert!(note.encounters[0].ended_at.is_some());
    }

    #[test]
    fn apply_event_transcript_attaches_to_encounter() {
        let mut note = PatientNote::new(Uuid::new_v4());
        let patient_id = note.patient_id;
        let encounter_id = Uuid::new_v4();
        apply_event(&mut note, &NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient));
        apply_event(&mut note, &NoteEvent::transcript_ready(
            patient_id,
            encounter_id,
            "chest pain".into(),
            Utc::now(),
        ));

        assert_eq!(note.encounters[0].transcript.as_deref(), Some("chest pain"));
    }

    #[test]
    fn replay_empty_events_returns_registered() {
        let note = replay(Uuid::new_v4(), &[]);
        assert_eq!(note.phase, NotePhase::Registered);
        assert_eq!(note.encounter_count, 0);
    }

    #[test]
    fn replay_full_lifecycle_reaches_signed() {
        let patient_id = Uuid::new_v4();
        let encounter_id = Uuid::new_v4();
        let events = vec![
            NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient),
            NoteEvent::transcript_ready(patient_id, encounter_id, "chest pain".into(), Utc::now()),
            NoteEvent::audio_stopped(patient_id, encounter_id),
            NoteEvent::trigger_mdm(patient_id),
            NoteEvent::review_ready(patient_id),
            NoteEvent::sign(patient_id),
        ];

        let note = replay(patient_id, &events);

        assert_eq!(note.phase, NotePhase::Signed);
        assert_eq!(note.encounter_count, 1);
        assert_eq!(note.encounters.len(), 1);
        assert_eq!(note.encounters[0].transcript.as_deref(), Some("chest pain"));
    }

    #[test]
    fn replay_multi_encounter_increments_count() {
        let patient_id = Uuid::new_v4();
        let e1 = Uuid::new_v4();
        let e2 = Uuid::new_v4();
        let events = vec![
            NoteEvent::audio_started(patient_id, e1, CaptureMode::Ambient),
            NoteEvent::audio_stopped(patient_id, e1),
            NoteEvent::audio_started(patient_id, e2, CaptureMode::Ambient),
            NoteEvent::audio_stopped(patient_id, e2),
        ];

        let note = replay(patient_id, &events);

        assert_eq!(note.encounter_count, 2);
        assert_eq!(note.encounters.len(), 2);
        assert_eq!(note.encounters[0].id, e1);
        assert_eq!(note.encounters[1].id, e2);
    }

    #[test]
    fn apply_event_result_received_after_sign_is_noop() {
        let patient_id = Uuid::new_v4();
        let encounter_id = Uuid::new_v4();
        let events = vec![
            NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient),
            NoteEvent::audio_stopped(patient_id, encounter_id),
            NoteEvent::trigger_mdm(patient_id),
            NoteEvent::review_ready(patient_id),
            NoteEvent::sign(patient_id),
        ];
        let mut note = replay(patient_id, &events);
        let initial_results_len = note.results.len();

        let late_result = NoteEvent::result_received(
            patient_id,
            build_test_lab_result(patient_id),
            Utc::now(),
        );
        apply_event(&mut note, &late_result);

        // Signed state rejects ingestion — results are unchanged.
        assert_eq!(note.results.len(), initial_results_len);
        assert_eq!(note.phase, NotePhase::Signed);
    }
    #[test]
    fn registered_to_encountering_on_audio_start() {
        let mut note = PatientNote::new(uuid::Uuid::new_v4());
        assert_eq!(note.phase, NotePhase::Registered);
        assert_eq!(note.encounter_count, 0);

        note.handle_audio_started();

        assert_eq!(note.phase, NotePhase::Encountering);
        assert_eq!(note.encounter_count, 1);
    }

    #[test]
    fn encountering_to_idle_on_audio_stop() {
        let mut note = PatientNote::new(uuid::Uuid::new_v4());
        note.handle_audio_started();
        note.handle_audio_stopped();

        assert_eq!(note.phase, NotePhase::Idle);
    }

    #[test]
    fn idle_encountering_cycle() {
        let mut note = PatientNote::new(uuid::Uuid::new_v4());

        // First encounter
        note.handle_audio_started();
        note.handle_audio_stopped();
        assert_eq!(note.phase, NotePhase::Idle);
        assert_eq!(note.encounter_count, 1);

        // Second encounter — the critical cycle
        note.handle_audio_started();
        assert_eq!(note.phase, NotePhase::Encountering);
        assert_eq!(note.encounter_count, 2);

        note.handle_audio_stopped();
        assert_eq!(note.phase, NotePhase::Idle);
    }

    #[test]
    fn idle_to_finalizing_to_signed() {
        let mut note = PatientNote::new(uuid::Uuid::new_v4());
        note.handle_audio_started();
        note.handle_audio_stopped();

        note.handle_trigger_mdm();
        assert_eq!(note.phase, NotePhase::Finalizing);

        note.handle_review_ready();
        assert_eq!(note.phase, NotePhase::PendingSignOff);

        note.handle_sign();
        assert_eq!(note.phase, NotePhase::Signed);
    }

    #[test]
    fn finalizing_can_return_to_encountering() {
        let mut note = PatientNote::new(uuid::Uuid::new_v4());
        note.handle_audio_started();
        note.handle_audio_stopped();
        note.handle_trigger_mdm();
        assert_eq!(note.phase, NotePhase::Finalizing);

        // Clinician re-enters room
        note.handle_audio_started();
        assert_eq!(note.phase, NotePhase::Encountering);
    }

    #[test]
    fn signed_rejects_results() {
        let mut note = PatientNote::new(uuid::Uuid::new_v4());
        note.handle_audio_started();
        note.handle_audio_stopped();
        note.handle_trigger_mdm();
        note.handle_review_ready();
        note.handle_sign();

        let result = ClinicalResult {
            id: uuid::Uuid::new_v4(),
            patient_id: note.patient_id,
            source: crate::models::ResultSource::Manual,
            timestamp: Utc::now(),
            result_type: crate::models::ResultType::Lab(crate::models::LabResult {
                fhir_id: None,
                loinc_code: None,
                display_name: "Troponin".into(),
                value: 0.04,
                unit: "ng/mL".into(),
                reference_range: Some((0.0, 0.04)),
                is_abnormal: false,
            }),
        };

        let err = ingest_result(&mut note, result);
        assert!(err.is_err());
    }

    #[test]
    fn invalid_transition_is_noop() {
        let mut note = PatientNote::new(uuid::Uuid::new_v4());
        // Registered → TriggerMDM is invalid — should stay Registered
        note.handle_trigger_mdm();
        assert_eq!(note.phase, NotePhase::Registered);
    }

    #[test]
    fn clone_preserves_state() {
        let mut note = PatientNote::new(uuid::Uuid::new_v4());
        note.handle_audio_started();
        note.handle_audio_stopped();
        assert_eq!(note.phase, NotePhase::Idle);

        let cloned = note.clone();
        assert_eq!(cloned.phase, NotePhase::Idle);
        assert!(cloned.state.is_some());
        assert_eq!(cloned.state.as_ref().unwrap().phase_name(), "Idle");
    }

    #[test]
    fn debug_output_includes_state() {
        let note = PatientNote::new(uuid::Uuid::new_v4());
        let debug = format!("{:?}", note);
        assert!(debug.contains("Registered"));
    }

    #[test]
    fn hydrate_reconstructs_state() {
        let mut note = PatientNote::new(uuid::Uuid::new_v4());
        note.handle_audio_started();
        note.handle_audio_stopped();

        // Simulate deserialization (state gets dropped).
        let json = serde_json::to_string(&note).unwrap();
        let mut deserialized: PatientNote = serde_json::from_str(&json).unwrap();

        // Before hydrate: state is None.
        assert!(deserialized.state.is_none());

        // After hydrate: state is reconstructed from the phase enum.
        deserialized.hydrate();
        assert!(deserialized.state.is_some());
        assert_eq!(deserialized.state.as_ref().unwrap().phase_name(), "Idle");

        // And it works — can transition.
        deserialized.handle_audio_started();
        assert_eq!(deserialized.phase, NotePhase::Encountering);
    }
}
