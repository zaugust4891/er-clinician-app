use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::patient::PatientId;

pub type ResultId = Uuid;

/// How a result entered the system.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ResultSource {
    /// Clinician manually entered the value.
    Manual,
    /// Pulled via FHIR API (future).
    FhirPull,
    /// Received via HL7 feed (future).
    Hl7Feed,
}

/// A clinical result — lab, imaging, or vital sign.
/// Field shapes mirror FHIR R4 Observation so future integration is a mapping layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClinicalResult {
    pub id: ResultId,
    pub patient_id: PatientId,
    pub source: ResultSource,
    pub timestamp: DateTime<Utc>,
    pub result_type: ResultType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResultType {
    Lab(LabResult),
    Imaging(ImagingResult),
    Vital(VitalSign),
}

/// A lab result with FHIR-ready fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabResult {
    /// FHIR resource ID, if pulled from EMR. None = manually entered.
    pub fhir_id: Option<String>,
    /// LOINC code for the lab test (e.g., "2160-0" for creatinine).
    pub loinc_code: Option<String>,
    pub display_name: String,
    pub value: f64,
    pub unit: String,
    pub reference_range: Option<(f64, f64)>,
    pub is_abnormal: bool,
}

/// Free-text imaging result (radiology report excerpt).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagingResult {
    pub modality: String, // "XR", "CT", "US", "MRI"
    pub body_part: String,
    pub impression: String,
    pub is_critical: bool,
}

/// A vital sign measurement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VitalSign {
    pub vital_type: VitalType,
    pub value: f64,
    pub unit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VitalType {
    HeartRate,
    BloodPressureSystolic,
    BloodPressureDiastolic,
    RespiratoryRate,
    Temperature,
    SpO2,
    Pain,
}
