use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for a patient within a shift.
pub type PatientId = Uuid;

/// Minimal patient record — enough for note attribution without full EMR data.
/// In the no-integration MVP, all fields are manually entered by the clinician.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatientRecord {
    pub id: PatientId,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub age: Option<u8>,
    pub chief_complaint: String,
    pub room: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl PatientRecord {
    pub fn new(chief_complaint: impl Into<String>) -> Self {
        Self {
            id: Uuid::now_v7(),
            first_name: None,
            last_name: None,
            age: None,
            chief_complaint: chief_complaint.into(),
            room: None,
            created_at: Utc::now(),
        }
    }
}
