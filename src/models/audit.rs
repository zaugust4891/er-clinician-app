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
    MdmTriggered,
    NoteReviewed,
    NoteSigned,
    NoteExported,
    PhaseTransition { from: String, to: String },
}

impl AuditEntry {
    pub fn new(action: AuditAction, details: impl Into<String>) -> Self {
        Self {
            id: Uuid::now_v7(),
            timestamp: Utc::now(),
            action,
            details: details.into(),
        }
    }
}
