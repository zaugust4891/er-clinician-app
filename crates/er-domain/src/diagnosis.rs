use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::PatientId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnosis {
    pub id: Uuid,
    pub patient_id: PatientId,
    pub text: String, // "Acute pulmonary embolism"
    pub icd10_code: Option<String>,
    pub is_primary: bool,
}

impl Diagnosis {
    /// A one-line summary for notes/export, e.g. `Acute PE (primary) [I26.99]`.
    pub fn summary_line(&self) -> String {
        let mut line = self.text.clone();
        if self.is_primary {
            line.push_str(" (primary)");
        }
        if let Some(code) = &self.icd10_code {
            line.push_str(&format!(" [{code}]"));
        }
        line
    }
}
