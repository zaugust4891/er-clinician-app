
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::PatientId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnosis {
    pub id: Uuid,
    pub patient_id: PatientId,
    pub text: String, // "Acute pulmonary embolism"
    pub icd10_code: Option<String>,
    pub is_primary: bool,
}
