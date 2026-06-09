use crate::models::{PatientRecord, Shift};

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ShiftSnapshot {
    pub shift: Shift,
    pub patients: Vec<PatientRecord>,
}