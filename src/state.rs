use std::sync::Arc;

use chrono::Utc;
use dashmap::DashMap;

use crate::error::{AppError, AppResult};
use crate::events::NoteEvent;
use crate::models::audit::{AuditAction, AuditEntry};
use crate::models::note::NotePhase;
use crate::models::patient::PatientId;
use crate::models::result::ClinicalResult;
use crate::models::PatientNote;

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

pub fn replay(events: &[NoteEvent]) -> PatientNote {
    todo!()
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
            patient_id, 
            encounter_id,
            capture_mode,
            clinical_occurred_at,
        } => {
            // Drive the state machine first so the encounter_count bumps
            // handle_audio_started calls self.apply_transition()
            // which takes the state out, appl
            note.handle_audio_started();
            
        
        
        
        
        }
    }
}





// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
