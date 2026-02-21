use std::sync::Arc;

use chrono::Utc;
use dashmap::DashMap;

use crate::error::{AppError, Result};
use crate::events::NoteEvent;
use crate::models::audit::{AuditAction, AuditEntry};
use crate::models::note::NotePhase;
use crate::models::patient::PatientId;
use crate::models::result::ClinicalResult;
use crate::models::PatientNote;

/// Concurrent store for all active patient notes.
/// Uses DashMap for lock-free reads — the common case is reading note state for UI.
pub type PatientNoteStore = Arc<DashMap<PatientId, PatientNote>>;

/// Create a new empty store.
pub fn new_store() -> PatientNoteStore {
    Arc::new(DashMap::new())
}

/// Apply a state transition to a patient note.
///
/// Returns `Ok(())` if the transition was valid, `Err` otherwise.
/// The note's version counter and audit log are updated atomically.
pub fn transition(note: &mut PatientNote, event: &NoteEvent) -> Result<()> {
    let valid = match (&note.phase, event) {
        // From Registered
        (NotePhase::Registered, NoteEvent::AudioStarted { .. }) => {
            note.phase = NotePhase::Encountering;
            note.encounter_count += 1;
            true
        }
        // From Encountering
        (NotePhase::Encountering, NoteEvent::AudioStopped { .. }) => {
            note.phase = NotePhase::Idle;
            true
        }
        // From Idle — the critical cycle
        (NotePhase::Idle, NoteEvent::AudioStarted { .. }) => {
            note.phase = NotePhase::Encountering;
            note.encounter_count += 1;
            true
        }
        (NotePhase::Idle, NoteEvent::TriggerMDM { .. }) => {
            note.phase = NotePhase::Finalizing;
            true
        }
        // From Finalizing — can return to encounter if clinician re-enters room
        (NotePhase::Finalizing, NoteEvent::AudioStarted { .. }) => {
            note.phase = NotePhase::Encountering;
            note.encounter_count += 1;
            true
        }
        (NotePhase::Finalizing, NoteEvent::ReviewReady { .. }) => {
            note.phase = NotePhase::PendingSignOff;
            true
        }
        // Sign off — terminal
        (NotePhase::PendingSignOff, NoteEvent::Sign { .. }) => {
            note.phase = NotePhase::Signed;
            true
        }
        _ => false,
    };

    if valid {
        note.version += 1;
        note.updated_at = Utc::now();
        note.audit_log.push(AuditEntry::new(
            AuditAction::PhaseTransition {
                from: format!("{:?}", note.phase),
                to: format!("{:?}", note.phase),
            },
            format!("event: {:?}", std::mem::discriminant(event)),
        ));
        Ok(())
    } else {
        Err(AppError::InvalidTransition {
            from: note.phase.clone(),
            event: format!("{:?}", std::mem::discriminant(event)),
        })
    }
}

/// Ingest a clinical result into a note. Not a state transition — results
/// can arrive in any non-terminal phase.
pub fn ingest_result(note: &mut PatientNote, result: ClinicalResult) -> Result<()> {
    if note.phase == NotePhase::Signed {
        return Err(AppError::NoteImmutable);
    }
    note.results.push(result);
    note.version += 1;
    note.updated_at = Utc::now();
    note.audit_log
        .push(AuditEntry::new(AuditAction::ResultIngested, ""));
    Ok(())
}
