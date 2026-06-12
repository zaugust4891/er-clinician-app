use crate::domain::{
    CaptureMode, ClinicalResult, 
    Disposition, EncounterId, NoteSections,
    PatientId, Medication, Diagnosis,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Events that drive the note state machine and the processing pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NoteEvent {
    /// Clinician started recording for a patient.
    AudioStarted {
        patient_id: PatientId,
        encounter_id: EncounterId,
        capture_mode: CaptureMode,
        clinical_occurred_at: DateTime<Utc>,
    },
    /// Clinician stopped recording.
    AudioStopped {
        patient_id: PatientId,
        encounter_id: EncounterId,
        clinical_occurred_at: DateTime<Utc>,
    },
    /// STT produced a transcript chunk.
    TranscriptReady {
        patient_id: PatientId,
        encounter_id: EncounterId,
        text: String,
        clinical_occurred_at: DateTime<Utc>,
    },
    /// AI produced/updated note prose from an
    /// encounter transcript.
    EncounterSummarized {
        patient_id: PatientId,
        encounter_id: EncounterId,
        sections: NoteSections,
        source_encounter_ids: Vec<EncounterId>,
        clinical_occurred_at: DateTime<Utc>,
    },
    MedicationRecorded { 
        patient_id: PatientId, 
        medication: Medication, 
        clinical_occurred_at: DateTime<Utc>,
    },
    /// A clinical result was ingested (manual or FHIR).
    ResultReceived {
        patient_id: PatientId,
        result: ClinicalResult,
        clinical_occurred_at: DateTime<Utc>,
    },

    DiagnosisAssigned  { 
        patient_id: PatientId,
        diagnosis: Diagnosis,   
        clinical_occurred_at: DateTime<Utc>, 
    },
    
    DataReviewed { 
        patient_id: PatientId, 
        physician_review: String,           
        clinical_occurred_at: DateTime<Utc>,
    },

    /// Clinician triggered MDM generation.
    TriggerMDM {
        patient_id: PatientId,
        clinical_occurred_at: DateTime<Utc>,
    },
    /// AI produced the MDM from accumulated data.
    MdmGenerated {
        patient_id: PatientId,
        mdm_text: String,
        source_encounter_ids: Vec<EncounterId>,
        clinical_occurred_at: DateTime<Utc>,
    },
    /// MDM draft is ready for review.
    ReviewReady {
        patient_id: PatientId,
        clinical_occurred_at: DateTime<Utc>,
    },
    /// Clinician set the disposition.
    DispositionSet {
        patient_id: PatientId,
        disposition: Disposition,
        clinical_occurred_at: DateTime<Utc>,
    },
    /// Clinician signed the note.
    Sign {
        patient_id: PatientId,
        clinical_occurred_at: DateTime<Utc>,
    },

}

impl NoteEvent {
    pub fn extract_patient_id(&self) -> PatientId {
        match self {
            NoteEvent::AudioStarted { patient_id, .. } => *patient_id,
            NoteEvent::AudioStopped { patient_id, .. } => *patient_id,
            NoteEvent::TranscriptReady { patient_id, .. } => *patient_id,
            NoteEvent::EncounterSummarized { patient_id, .. } => *patient_id,
            NoteEvent::MedicationRecorded { patient_id, .. } => *patient_id,
            NoteEvent::ResultReceived { patient_id, .. } => *patient_id,
            NoteEvent::DiagnosisAssigned { patient_id, .. } => *patient_id,
            NoteEvent::DataReviewed { patient_id, .. } => *patient_id,
            NoteEvent::TriggerMDM { patient_id, .. } => *patient_id,
            NoteEvent::MdmGenerated { patient_id, .. } => *patient_id,
            NoteEvent::ReviewReady { patient_id, .. } => *patient_id,
            NoteEvent::DispositionSet { patient_id, .. } => *patient_id,
            NoteEvent::Sign { patient_id, .. } => *patient_id,
        }
    }

    pub fn event_type(&self) -> &'static str {
        match self {
            NoteEvent::AudioStarted { .. } => "AudioStarted",
            NoteEvent::AudioStopped { .. } => "AudioStopped",
            NoteEvent::TranscriptReady { .. } => "TranscriptReady",
            NoteEvent::EncounterSummarized { .. } => "EncounterSummarized",
            NoteEvent::MedicationRecorded { .. } => "MedicationRecorded",
            NoteEvent::ResultReceived { .. } => "ResultReceived",
            NoteEvent::DiagnosisAssigned { .. } => "DiagnosisAssigned",
            NoteEvent::DataReviewed { .. } => "DataReviewed",
            NoteEvent::TriggerMDM { .. } => "TriggerMDM",
            NoteEvent::MdmGenerated { .. } => "MdmGenerated",
            NoteEvent::ReviewReady { .. } => "ReviewReady",
            NoteEvent::DispositionSet { .. } => "DispositionSet",
            NoteEvent::Sign { .. } => "Sign",
        }
    }

    pub fn audio_started(
        patient_id: PatientId,
        encounter_id: EncounterId,
        capture_mode: CaptureMode,
    ) -> Self {
        NoteEvent::AudioStarted {
            patient_id,
            encounter_id,
            capture_mode,
            clinical_occurred_at: Utc::now(),
        }
    }

    pub fn audio_stopped(patient_id: PatientId, encounter_id: EncounterId) -> Self {
        NoteEvent::AudioStopped {
            patient_id,
            encounter_id,
            clinical_occurred_at: Utc::now(),
        }
    }

    pub fn transcript_ready(
        patient_id: PatientId,
        encounter_id: EncounterId,
        text: String,
        clinical_occurred_at: DateTime<Utc>,
    ) -> Self {
        NoteEvent::TranscriptReady {
            patient_id,
            encounter_id,
            text,
            clinical_occurred_at,
        }
    }

    pub fn encounter_summarized(
        patient_id: PatientId,
        encounter_id: EncounterId,
        sections: NoteSections,
        source_encounter_ids: Vec<EncounterId>,
    ) -> Self {
        NoteEvent::EncounterSummarized {
            patient_id,
            encounter_id,
            sections,
            source_encounter_ids,
            clinical_occurred_at: Utc::now(),
        }
    }

    pub fn result_received(
        patient_id: PatientId,
        result: ClinicalResult,
        clinical_occurred_at: DateTime<Utc>,
    ) -> Self {
        NoteEvent::ResultReceived {
            patient_id,
            result,
            clinical_occurred_at,
        }
    }

    pub fn trigger_mdm(patient_id: PatientId) -> Self {
        NoteEvent::TriggerMDM {
            patient_id,
            clinical_occurred_at: Utc::now(),
        }
    }

    pub fn mdm_generated(
        patient_id: PatientId,
        mdm_text: String,
        source_encounter_ids: Vec<EncounterId>,
    ) -> Self {
        NoteEvent::MdmGenerated {
            patient_id,
            mdm_text,
            source_encounter_ids,
            clinical_occurred_at: Utc::now(),
        }
    }

    pub fn review_ready(patient_id: PatientId) -> Self {
        NoteEvent::ReviewReady {
            patient_id,
            clinical_occurred_at: Utc::now(),
        }
    }

    pub fn disposition_set(patient_id: PatientId, disposition: Disposition) -> Self {
        NoteEvent::DispositionSet {
            patient_id,
            disposition,
            clinical_occurred_at: Utc::now(),
        }
    }

    pub fn medication_recorded(patient_id: PatientId, medication: Medication) -> Self {
        NoteEvent::MedicationRecorded {
            patient_id,
            medication,
            clinical_occurred_at: Utc::now(),
        }
    }

    pub fn diagnosis_assigned(patient_id: PatientId, diagnosis: Diagnosis) -> Self {
        NoteEvent::DiagnosisAssigned {
            patient_id,
            diagnosis,
            clinical_occurred_at: Utc::now(),
        }
    }

    pub fn data_reviewed(patient_id: PatientId, physician_review: String) -> Self {
        NoteEvent::DataReviewed {
            patient_id,
            physician_review,
            clinical_occurred_at: Utc::now(),
        }
    }

    pub fn sign(patient_id: PatientId) -> Self {
        NoteEvent::Sign {
            patient_id,
            clinical_occurred_at: Utc::now(),
        }
    }

    pub fn extract_encounter_id(&self) -> Option<EncounterId> {
        match self {
            NoteEvent::AudioStarted { encounter_id, .. } => Some(*encounter_id),
            NoteEvent::AudioStopped { encounter_id, .. } => Some(*encounter_id),
            NoteEvent::TranscriptReady { encounter_id, .. } => Some(*encounter_id),
            NoteEvent::EncounterSummarized { encounter_id, .. } => Some(*encounter_id),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::domain::PatientId;
    use crate::domain::encounter::CaptureMode;

    /// Unit test for extract_patient_id() method.
    #[test]
    fn extract_patient_id() {
        let patient_id = PatientId::now_v7();
        let encounter_id = EncounterId::now_v7();
        let event = NoteEvent::AudioStarted {
            patient_id,
            capture_mode: CaptureMode::Ambient,
            clinical_occurred_at: Utc::now(),
            encounter_id: encounter_id,
        };

        println!("patient_id: {:?}", patient_id);
        assert_eq!(event.extract_patient_id(), patient_id);
    }

    #[test]
    fn event_type_returns_correct_string_for_each_variant() {
        let patient_id = PatientId::now_v7();
        let encounter_id = EncounterId::now_v7();
        let now = Utc::now();

        let cases: Vec<(NoteEvent, &'static str)> = vec![
            (
                NoteEvent::AudioStarted {
                    patient_id,
                    encounter_id,
                    capture_mode: CaptureMode::Ambient,
                    clinical_occurred_at: now,
                },
                "AudioStarted",
            ),
            (
                NoteEvent::AudioStopped {
                    patient_id,
                    encounter_id,
                    clinical_occurred_at: now,
                },
                "AudioStopped",
            ),
            (
                NoteEvent::TranscriptReady {
                    patient_id,
                    encounter_id,
                    text: "hello".into(),
                    clinical_occurred_at: now,
                },
                "TranscriptReady",
            ),
            (
                NoteEvent::ResultReceived {
                    patient_id,
                    result: crate::domain::ClinicalResult {
                        id: uuid::Uuid::now_v7(),
                        patient_id,
                        source: crate::domain::ResultSource::Manual,
                        timestamp: now,
                        result_type: crate::domain::ResultType::Vital(
                            crate::domain::result::VitalSign {
                                vital_type: crate::domain::result::VitalType::HeartRate,
                                value: 72.0,
                                unit: "bpm".into(),
                            },
                        ),
                    },
                    clinical_occurred_at: now,
                },
                "ResultReceived",
            ),
            (
                NoteEvent::TriggerMDM {
                    patient_id,
                    clinical_occurred_at: now,
                },
                "TriggerMDM",
            ),
            (
                NoteEvent::ReviewReady {
                    patient_id,
                    clinical_occurred_at: now,
                },
                "ReviewReady",
            ),
            (
                NoteEvent::Sign {
                    patient_id,
                    clinical_occurred_at: now,
                },
                "Sign",
            ),
        ];

        for (event, expected) in cases {
            assert_eq!(
                event.event_type(),
                expected,
                "wrong event_type for {expected}"
            );
            println!("event: {:?}, expected: {:?}", event, expected);
        }
    }

    #[test]
    fn extract_encounter_id() {
        let patient_id = PatientId::now_v7();
        let encounter_id = EncounterId::now_v7();
        let event = NoteEvent::AudioStarted {
            patient_id,
            capture_mode: CaptureMode::Ambient,
            clinical_occurred_at: Utc::now(),
            encounter_id,
        };

        println!("encounter_id: {:?}", encounter_id);
        assert_eq!(event.extract_encounter_id(), Some(encounter_id));
    }
}
