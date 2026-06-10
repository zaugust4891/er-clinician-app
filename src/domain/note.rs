use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::audit::{AuditAction, AuditEntry};
use super::disposition::Disposition;
use super::encounter::{Encounter, EncounterId};
use super::patient::PatientId;
use super::result::{ClinicalResult, ResultType};
use crate::domain::state::{self, NoteState, Registered};

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
#[derive(Clone, Serialize, Deserialize, PartialEq)]
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

impl std::fmt::Debug for NotePhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotePhase::Registered => write!(f, "Registered"),
            NotePhase::Encountering => write!(f, "Encountering"),
            NotePhase::Idle => write!(f, "Idle"),
            NotePhase::Finalizing => write!(f, "Finalizing"),
            NotePhase::PendingSignOff => write!(f, "PendingSignOff"),
            NotePhase::Signed => write!(f, "Signed"),
        }
    }
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

impl NoteSectionVersion {
    pub fn new(
        section: impl Into<String>,
        content: impl Into<String>,
        version: u64,
        source_encounter_ids: Vec<EncounterId>,
    ) -> Self {
        Self {
            section: section.into(),
            content: content.into(),
            version,
            generated_at: Utc::now(),
            source_encounter_ids,
        }
    }
}

/// The complete patient note — the central domain object.
///
/// Holds both a `NotePhase` enum (for serialization) and a `Box<dyn NoteState>`
/// trait object (for behavior). They are kept in sync by the `handle_*` methods.
#[derive(Serialize, Deserialize)]
pub struct PatientNote {
    pub id: Uuid,
    pub patient_id: PatientId,
    /// Serialization representation — serde reads/writes this.
    pub phase: NotePhase,
    /// Behavior representation — serde skips this.
    /// Reconstructed from `phase` via `hydrate()` after deserialization.
    #[serde(skip)]
    pub state: Option<Box<dyn NoteState>>,
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

// ---------------------------------------------------------------------------
// Manual Clone — Box<dyn NoteState> doesn't derive Clone
// ---------------------------------------------------------------------------

impl Clone for PatientNote {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            patient_id: self.patient_id,
            phase: self.phase.clone(),
            state: self.state.as_ref().map(|s| s.clone_state()),
            encounter_count: self.encounter_count,
            encounters: self.encounters.clone(),
            results: self.results.clone(),
            note_sections: self.note_sections.clone(),
            section_history: self.section_history.clone(),
            disposition: self.disposition.clone(),
            version: self.version,
            audit_log: self.audit_log.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Manual Debug — Box<dyn NoteState> doesn't derive Debug
// ---------------------------------------------------------------------------

impl std::fmt::Debug for PatientNote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state_name = self
            .state
            .as_ref()
            .map(|s| s.phase_name())
            .unwrap_or("unhydrated");

        f.debug_struct("PatientNote")
            .field("id", &self.id)
            .field("patient_id", &self.patient_id)
            .field("phase", &self.phase)
            .field("state", &state_name)
            .field("encounter_count", &self.encounter_count)
            .field("encounters", &self.encounters.len())
            .field("results", &self.results.len())
            .field("version", &self.version)
            .finish()
    }
}

impl fmt::Display for PatientNote {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "ED PROVIDER NOTE")?;
        writeln!(f)?;
        writeln!(f, "Patient ID: {}", self.patient_id)?;
        writeln!(f, "Note ID: {}", self.id)?;
        writeln!(f, "Status: {:?}", self.phase)?;
        writeln!(f, "Version: {}", self.version)?;
        writeln!(f, "Created: {}", self.created_at)?;
        writeln!(f, "Updated: {}", self.updated_at)?;
        writeln!(f)?;

        write_section(f, "HPI", self.note_sections.hpi.as_deref())?;

        if let Some(ros) = self.note_sections.ros.as_deref() {
            write_section(f, "Review of Systems", Some(ros))?;
        } else {
            write_section(f, "Review of Systems", Some("See HPI"))?;
        }

        write_section(f, "Physical Exam", self.note_sections.exam.as_deref())?;
        write_results(f, &self.results)?;
        write_section(
            f,
            "Medical Decision Making",
            self.note_sections.mdm.as_deref(),
        )?;

        if let Some(disposition) = &self.disposition {
            writeln!(f, "Disposition: {}", format_disposition(disposition))?;
            writeln!(f)?;
        } else {
            write_section(
                f,
                "Disposition",
                self.note_sections.disposition_text.as_deref(),
            )?;
        }

        writeln!(f, "---")?;
        writeln!(
            f,
            "Note assisted by AI documentation tool. All content reviewed and approved by signing provider."
        )
    }
}

fn write_section(f: &mut fmt::Formatter<'_>, title: &str, content: Option<&str>) -> fmt::Result {
    if let Some(content) = content {
        writeln!(f, "{title}:")?;
        writeln!(f, "{content}")?;
        writeln!(f)?;
    }

    Ok(())
}

fn write_results(f: &mut fmt::Formatter<'_>, results: &[ClinicalResult]) -> fmt::Result {
    if results.is_empty() {
        return Ok(());
    }

    writeln!(f, "Results Reviewed:")?;
    for result in results {
        match &result.result_type {
            ResultType::Lab(lab) => {
                let abnormal_marker = if lab.is_abnormal { " abnormal" } else { "" };
                writeln!(
                    f,
                    "{}: {} {}{}",
                    lab.display_name, lab.value, lab.unit, abnormal_marker
                )?;
            }
            ResultType::Imaging(imaging) => {
                let critical_marker = if imaging.is_critical { " critical" } else { "" };
                writeln!(
                    f,
                    "{} {}: {}{}",
                    imaging.modality, imaging.body_part, imaging.impression, critical_marker
                )?;
            }
            ResultType::Vital(vital) => {
                writeln!(f, "{:?}: {} {}", vital.vital_type, vital.value, vital.unit)?;
            }
        }
    }
    writeln!(f)
}

fn format_disposition(disposition: &Disposition) -> String {
    match disposition {
        Disposition::Discharge {
            instructions,
            follow_up,
        } => {
            let mut output = format!("Discharge\nInstructions: {instructions}");
            if let Some(follow_up) = follow_up {
                output.push_str(&format!("\nFollow-up: {follow_up}"));
            }
            output
        }
        Disposition::Admit { service, bed } => {
            let mut output = format!("Admit to {service}");
            if let Some(bed) = bed {
                output.push_str(&format!(", Bed: {bed}"));
            }
            output
        }
        Disposition::Transfer {
            destination,
            reason,
        } => {
            let mut output = format!("Transfer to {destination}");
            if let Some(reason) = reason {
                output.push_str(&format!(", Reason: {reason}"));
            }
            output
        }
        Disposition::AMA => "Left Against Medical Advice (AMA)".into(),
        Disposition::LWBS => "Left Without Being Seen (LWBS)".into(),
    }
}

// ---------------------------------------------------------------------------
// State transition methods — these keep phase + state in sync
// ---------------------------------------------------------------------------

impl PatientNote {
    pub fn new(patient_id: PatientId) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::now_v7(),
            patient_id,
            phase: NotePhase::Registered,
            state: Some(Box::new(Registered)),
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

    /// Reconstruct the state trait object from the phase enum.
    /// Must be called after deserialization.
    pub fn hydrate(&mut self) {
        if self.state.is_none() {
            self.state = Some(state::state_from_phase(&self.phase));
        }
    }

    pub fn record_section_versions(
        &mut self,
        sections: &NoteSections,
        source_encounter_ids: &[EncounterId]
    ) {
        let version = self.version + 1;

        if let Some(hpi) = &sections.hpi  {
            self.section_history.push(NoteSectionVersion::new(
                "hpi",
                hpi.clone(),
                version,
                source_encounter_ids.to_vec(),
            ));
        }

        if let Some(ros) = &sections.ros {
            self.section_history.push(NoteSectionVersion::new(
                "ros",
                ros.clone(),
                version,
                source_encounter_ids.to_vec(),
            ));
        }

        if let Some(exam) = &sections.exam {
            self.section_history.push(NoteSectionVersion::new(
                "exam",
                exam.clone(),
                version,
                source_encounter_ids.to_vec(),
            ));
        }
    
    }

    /// Apply a state transition: take the state out, call the method,
    /// put the new state back, and sync the phase enum.
    fn apply_transition(
        &mut self,
        transition: impl FnOnce(Box<dyn NoteState>, &mut PatientNote) -> Box<dyn NoteState>,
    ) {
        if let Some(current) = self.state.take() {
            let old_phase = current.to_phase(); 
            let new_state = transition(current, self); 
            let new_phase = new_state.to_phase(); 

            self.phase = new_phase.clone();
            self.state = Some(new_state);

            // Only log + bump version if the phase actually changed.
            if old_phase != new_phase {
                self.version += 1;
                self.updated_at = Utc::now();
                self.audit_log.push(AuditEntry::new(
                    AuditAction::PhaseTransition {
                        from: format!("{:?}", old_phase),
                        to: format!("{:?}", new_phase),
                    },
                    "",
                ));
            }
        }
    }

    pub fn handle_audio_started(&mut self) {
        self.apply_transition(|state, note| state.on_audio_started(note));
    }

    pub fn handle_audio_stopped(&mut self) {
        self.apply_transition(|state, note| state.on_audio_stopped(note));
    }

    pub fn handle_trigger_mdm(&mut self) {
        self.apply_transition(|state, note| state.on_trigger_mdm(note));
    }

    pub fn handle_review_ready(&mut self) {
        self.apply_transition(|state, note| state.on_review_ready(note));
    }

    pub fn handle_sign(&mut self) {
        self.apply_transition(|state, note| state.on_sign(note));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{LabResult, ResultSource};

    #[test]
    fn display_formats_patient_note_sections_and_results() {
        let patient_id = Uuid::now_v7();
        let mut note = PatientNote::new(patient_id);
        note.note_sections.hpi = Some("Patient presents with chest pain.".into());
        note.note_sections.exam = Some("CV: regular rate and rhythm.".into());
        note.note_sections.mdm = Some("Low risk chest pain workup.".into());
        note.results.push(ClinicalResult {
            id: Uuid::now_v7(),
            patient_id,
            source: ResultSource::Manual,
            timestamp: Utc::now(),
            result_type: ResultType::Lab(LabResult {
                fhir_id: None,
                loinc_code: None,
                display_name: "Troponin".into(),
                value: 0.04,
                unit: "ng/mL".into(),
                reference_range: Some((0.0, 0.04)),
                is_abnormal: false,
            }),
        });
        note.disposition = Some(Disposition::Discharge {
            instructions: "Return if symptoms worsen.".into(),
            follow_up: Some("PCP in 2 days".into()),
        });

        let output = note.to_string();

        assert!(output.contains("ED PROVIDER NOTE"));
        assert!(output.contains(&format!("Patient ID: {patient_id}")));
        assert!(output.contains("HPI:\nPatient presents with chest pain."));
        assert!(output.contains("Review of Systems:\nSee HPI"));
        assert!(output.contains("Troponin: 0.04 ng/mL"));
        assert!(output.contains("Disposition: Discharge"));
        assert!(output.contains("Follow-up: PCP in 2 days"));
    }
}
