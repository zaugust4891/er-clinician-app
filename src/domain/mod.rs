pub mod audit;
pub mod disposition;
pub mod encounter;
pub mod events;
pub mod note;
pub mod patient;
pub mod result;
pub mod shift;

// Re-export core types for convenience.
pub use audit::{AuditAction, AuditEntry};
pub use disposition::Disposition;
pub use encounter::{CaptureMode, Encounter, EncounterId};
pub use events::NoteEvent;
pub use note::{NotePhase, NoteSections, PatientNote};
pub use patient::{PatientId, PatientRecord};
pub use result::{ClinicalResult, LabResult, ResultSource, ResultType};
pub use shift::{Shift, ShiftId};
