use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::audit::AuditEntry;
use super::disposition::Disposition;
use super::encounter::Encounter;
use super::patient::PatientId;
use super::result::ClinicalResult;

/// Patient note lifecycle — supports cycles (Idle ↔ Encountering).
///
/// Valid transition graph:
/// ```text
///   Registered ──AudioStart──▶ Encountering
///                                    │
///                                AudioStop
///                                    │
///                                    ▼
///                                  Idle ◀──────────────┐
///                                 ╱    ╲               │
///                       AudioStart    TriggerMDM   AudioStop
///                               ╱          ╲           │
///                              ▼            ▼          │
///                       Encountering    Finalizing ──▶ │
///                                          │
///                                       ReviewReady
///                                          │
///                                          ▼
///                                    PendingSignOff
///                                          │
///                                        Sign
///                                          ▼
///                                        Signed
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NotePhase {
    /// Chart open, no audio yet.
    Registered,
    /// Clinician in room or dictating — audio live.
    Encountering,
    /// Between encounters — waiting on data or seeing other patients.
    Idle,
    /// Clinician triggered MDM/dispo — assembling final note.
    Finalizing,
    /// Draft complete — awaiting clinician review.
    PendingSignOff,
    /// Terminal state — immutable after this.
    Signed,
}

/// The sections that compose an ED note.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NoteSections {
    pub hpi: Option<String>,
    pub ros: Option<String>,
    pub exam: Option<String>,
    pub mdm: Option<String>,
    pub disposition_text: Option<String>,
}

/// A versioned note section snapshot — retained for audit trail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteSectionVersion {
    pub section: String, // "hpi", "exam", "mdm", etc.
    pub content: String,
    pub version: u64,
    pub generated_at: DateTime<Utc>,
    pub source_encounter_ids: Vec<Uuid>,
}

/// The complete patient note — the central domain object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatientNote {
    pub id: Uuid,
    pub patient_id: PatientId,
    pub phase: NotePhase,
    pub encounter_count: u32,
    pub encounters: Vec<Encounter>,
    pub results: Vec<ClinicalResult>,
    pub note_sections: NoteSections,
    pub section_history: Vec<NoteSectionVersion>,
    pub disposition: Option<Disposition>,
    pub version: u64,
    pub audit_log: Vec<AuditEntry>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PatientNote {
    pub fn new(patient_id: PatientId) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            patient_id,
            phase: NotePhase::Registered,
            encounter_count: 0,
            encounters: Vec::new(),
            results: Vec::new(),
            note_sections: NoteSections::default(),
            section_history: Vec::new(),
            disposition: None,
            version: 0,
            audit_log: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }
}
