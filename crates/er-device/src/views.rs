//! Plain read models. No domain state objects or raw events cross the phone boundary.
use crate::{ConsentStatus, ContextCategory, DefinitionKind, RecorderStatus};

#[derive(Clone, Debug)]
pub struct ShiftView {
    pub id: String,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub selected_patient_id: Option<String>,
    pub is_open: bool,
}
#[derive(Clone, Debug)]
pub struct ConsentView {
    pub patient_id: String,
    pub status: ConsentStatus,
    pub recorded_at_ms: Option<i64>,
    pub policy_version: Option<String>,
    pub revision: u64,
}
#[derive(Clone, Debug)]
pub struct RecordingView {
    pub patient_id: String,
    pub solo: bool,
    pub source: String,
    pub segment_id: Option<String>,
    pub status: RecorderStatus,
    pub boundary_at_ms: Option<i64>,
    pub reason: Option<String>,
    pub hardware_available: bool,
}
#[derive(Clone, Debug)]
pub struct PatientView {
    pub id: String,
    pub complaint: String,
    pub room: Option<String>,
    pub phase: ClinicalPhase,
    pub position: u32,
    pub ambient_eligible: bool,
    pub solo_eligible: bool,
    pub consent: ConsentView,
    pub recording: RecordingView,
}
#[derive(Clone, Debug)]
pub struct NoteView {
    pub patient_id: String,
    pub phase: ClinicalPhase,
    pub hpi: Option<String>,
    pub ros: Option<String>,
    pub exam: Option<String>,
    pub mdm: Option<String>,
    pub disposition: Option<String>,
    pub disposition_text: Option<String>,
    pub objective_items: Vec<ObjectiveView>,
    pub results: Vec<String>,
    pub medications: Vec<String>,
    pub diagnoses: Vec<String>,
    pub data_reviewed: Vec<String>,
    pub encounter_count: u32,
    pub audit_count: u64,
    pub projection_version: u64,
    pub recovery_required: bool,
    pub manual_draft: Option<String>,
}
#[derive(Clone, Debug)]
pub struct ContextView {
    pub id: String,
    pub patient_id: String,
    pub version: u64,
    pub category: ContextCategory,
    pub text: String,
    pub source: String,
    pub clinical_at_ms: Option<i64>,
    pub entered_at_ms: i64,
    pub reviewed: bool,
}
#[derive(Clone, Debug)]
pub struct SectionView {
    pub section: String,
    pub instructions: String,
}
#[derive(Clone, Debug)]
pub struct DefinitionView {
    pub id: String,
    pub version: u64,
    pub name: String,
    pub kind: DefinitionKind,
    pub instructions: String,
    pub sections: Vec<SectionView>,
    pub archived: bool,
    pub is_default: bool,
}
#[derive(Clone, Debug)]
pub struct DeviceView {
    pub revision: u64,
    pub shift: Option<ShiftView>,
    pub patients: Vec<PatientView>,
    pub definitions: Vec<DefinitionView>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClinicalPhase {
    Registered,
    Encountering,
    Idle,
    Finalizing,
    PendingSignOff,
    Signed,
}
impl From<&er_domain::NotePhase> for ClinicalPhase {
    fn from(p: &er_domain::NotePhase) -> Self {
        match p {
            er_domain::NotePhase::Registered => Self::Registered,
            er_domain::NotePhase::Encountering => Self::Encountering,
            er_domain::NotePhase::Idle => Self::Idle,
            er_domain::NotePhase::Finalizing => Self::Finalizing,
            er_domain::NotePhase::PendingSignOff => Self::PendingSignOff,
            er_domain::NotePhase::Signed => Self::Signed,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ObjectiveView {
    pub id: String,
    pub category: ContextCategory,
    pub summary: String,
    pub source: String,
    pub clinical_at_ms: Option<i64>,
}
