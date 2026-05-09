use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::patient::PatientId;

pub type ShiftId = Uuid;

/// Top-level container for a clinical shift.
/// All patients and notes belong to a shift. Data retention timers start at shift end.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Shift {
    pub id: ShiftId,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub patient_ids: Vec<PatientId>,
}

impl Shift {
    pub fn new() -> Self {
        Self {
            id: Uuid::now_v7(),
            started_at: Utc::now(),
            ended_at: None,
            patient_ids: Vec::new(),
        }
    }

    pub fn add_patient(&mut self, patient_id: PatientId) {
        if !self.patient_ids.contains(&patient_id) {
            self.patient_ids.push(patient_id);
        }
    }

    pub fn end_shift(&mut self) {
        self.ended_at = Some(Utc::now());
    }

    pub fn is_active(&self) -> bool {
        self.ended_at.is_none()
    }
}

impl Default for Shift {
    fn default() -> Self {
        Self::new()
    }
}
