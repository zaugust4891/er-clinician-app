use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Immutable audit log entry — append-only, never modified.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub action: AuditAction,
    pub details: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuditAction {
    PatientCreated,
    EncounterStarted,
    EncounterStopped,
    TranscriptGenerated,
    ResultIngested,
    NoteSectionUpdated,
    NoteSectionGenerated,
    NoteSectionVersion,
    MdmTriggered,
    MdmGenerated,
    DispositionSet,
    DiagnosisAssigned,
    DataReviewed,
    MedicationRecorded,
    NoteReviewed,
    NoteSigned,
    NoteExported,
    PhaseTransition { from: String, to: String },
}

impl AuditEntry {
    pub fn new(action: AuditAction, details: impl Into<String>) -> Self {
        Self::with_timestamp(action, details, Utc::now())
    }

    /// Build an entry stamped with an explicit time. Used on the replay/projection
    /// path so the audit trail reflects when the event actually occurred
    /// (`clinical_occurred_at`) rather than when it was replayed.
    pub fn with_timestamp(
        action: AuditAction,
        details: impl Into<String>,
        timestamp: DateTime<Utc>,
    ) -> Self {
        Self {
            id: Uuid::now_v7(),
            timestamp,
            action,
            details: details.into(),
        }
    }
}
