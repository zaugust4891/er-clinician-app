//! Command validation is separate from tolerant historical event replay.
use crate::{AppError, AppResult};
use er_domain::{NotePhase, PatientNote};
pub fn editable(note: &PatientNote) -> AppResult<()> {
    if note.phase == NotePhase::Signed {
        Err(AppError::Domain(er_domain::DomainError::NoteImmutable))
    } else {
        Ok(())
    }
}
pub fn capture_phase(note: &PatientNote) -> AppResult<()> {
    if matches!(
        note.phase,
        NotePhase::Registered | NotePhase::Idle | NotePhase::Finalizing
    ) {
        Ok(())
    } else {
        Err(AppError::Unsupported)
    }
}
