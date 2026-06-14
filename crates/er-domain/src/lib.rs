pub mod audit;
pub mod diagnosis;
pub mod disposition;
pub mod encounter;
pub mod error;
pub mod events;
pub mod medication;
pub mod note;
pub mod patient;
pub mod replay;
pub mod result;
pub mod shift;
pub mod state;

// Re-export core types for convenience.
pub use audit::{AuditAction, AuditEntry};
pub use diagnosis::Diagnosis; 
pub use disposition::Disposition;
pub use encounter::{CaptureMode, Encounter, EncounterId};
pub use error::{DomainError, DomainResult};
pub use events::NoteEvent;
pub use medication::{Medication, MedicationKind};
pub use note::{NotePhase, NoteSections, PatientNote};
pub use patient::{PatientId, PatientRecord};
pub use result::{ClinicalResult, LabResult, ResultSource, ResultType};
pub use shift::{Shift, ShiftId};
