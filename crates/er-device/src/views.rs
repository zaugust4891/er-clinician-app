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

/// Where the phone's audio chunks are. Counts only: no audio, file names or URLs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioView {
    /// Directory the recorder writes chunk files into.
    pub directory: String,
    /// Recorded and owned by the core, waiting for the network.
    pub awaiting_upload: u64,
    /// Accepted by the server; the local file is kept until its transcript arrives.
    pub awaiting_transcript: u64,
    /// Transcribed by the server; local file deleted.
    pub processed: u64,
    /// Refused locally or by the server. Needs attention; never silently dropped.
    pub rejected: u64,
    /// Finished files waiting behind an earlier chunk that has not arrived yet.
    pub not_yet_collected: u64,
    /// Files in the directory that belong to no known segment. Left untouched.
    pub unclaimed_files: u64,
    /// Temporary files left by a recorder that stopped mid-chunk. Left untouched.
    pub unfinished_files: u64,
    /// Recorder positions that never arrived in a stopped segment.
    pub missing_chunks: u64,
    /// Stopped segments with no usable audio at all.
    pub segments_without_audio: u64,
}
