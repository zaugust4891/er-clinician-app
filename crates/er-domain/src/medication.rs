use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::PatientId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Medication {
    pub id: Uuid,
    pub patient_id: PatientId,
    pub name: String,
    pub dose: String,                // "324 mg", "1 g"
    pub route: Option<String>,       // "PO", "IV"
    pub rxnorm_code: Option<String>, // None = manually entered
    pub kind: MedicationKind,
    pub administered_at: DateTime<Utc>,
    pub frequency: String, // "2x per day"
}

impl Medication {
    /// A compact, clinically readable one-line summary for notes/export,
    /// e.g. `Aspirin 324mg PO (administered in ED)` or `Lisinopril (home medication)`.
    pub fn summary_line(&self) -> String {
        let mut line = self.name.clone();
        if !self.dose.is_empty() {
            line.push(' ');
            line.push_str(&self.dose);
        }
        if let Some(route) = &self.route {
            line.push(' ');
            line.push_str(route);
        }
        line.push_str(&format!(" ({})", self.kind));
        line
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MedicationKind {
    Home,
    AdministeredInEd,
}

impl fmt::Display for MedicationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MedicationKind::Home => write!(f, "home medication"),
            MedicationKind::AdministeredInEd => write!(f, "administered in ED"),
        }
    }
}

impl fmt::Display for Medication {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let route = self.route.as_deref().unwrap_or("route not specified");
        let rxnorm_code = self.rxnorm_code.as_deref().unwrap_or("no RxNorm code");

        write!(
            f,
            "{} {} {} via {} ({}, {}, given at {}, medication id {}, patient id {})",
            self.name,
            self.dose,
            self.frequency,
            route,
            self.kind,
            rxnorm_code,
            self.administered_at,
            self.id,
            self.patient_id,
        )
    }
}
