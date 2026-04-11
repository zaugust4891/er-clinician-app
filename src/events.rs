use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use std::sync::Arc;
use chrono::{DateTime, Utc};
use crate::models::{CaptureMode, ClinicalResult, EncounterId, PatientId};
use crate::repository::Repository;

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
    /// A clinical result was ingested (manual or FHIR).
    ResultReceived {
        patient_id: PatientId,
        result: ClinicalResult,
        clinical_occurred_at: DateTime<Utc>,
    },
    /// Clinician triggered MDM generation.
    TriggerMDM { 
        patient_id: PatientId,
        clinical_occurred_at: DateTime<Utc>,
    },
    /// MDM draft is ready for review.
    ReviewReady {
        patient_id: PatientId,
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
            NoteEvent::ResultReceived { patient_id, .. } => *patient_id,
            NoteEvent::TriggerMDM { patient_id, .. } => *patient_id,
            NoteEvent::ReviewReady { patient_id, .. } => *patient_id,
            NoteEvent::Sign { patient_id, .. } => *patient_id,
        }
    }

    pub fn event_type(&self) -> &'static str {
        match self {
            NoteEvent::AudioStarted { .. } => "AudioStarted",
            NoteEvent::AudioStopped { .. } => "AudioStopped",
            NoteEvent::TranscriptReady { .. } => "TranscriptReady",
            NoteEvent::ResultReceived { .. } => "ResultReceived",
            NoteEvent::TriggerMDM { .. } => "TriggerMDM",
            NoteEvent::ReviewReady { .. } => "ReviewReady",
            NoteEvent::Sign { .. } => "Sign",
        }
    }

    pub fn extract_encounter_id(&self) -> Option<EncounterId> {
        match self {
            NoteEvent::AudioStarted { encounter_id, ..} => Some(*encounter_id),
            NoteEvent::AudioStopped { encounter_id, .. } => Some(*encounter_id),
            NoteEvent::TranscriptReady { encounter_id, .. } => Some(*encounter_id),
            _=> None,
        }
    }
}

/// Broadcast event bus — multiple consumers, single producer per event.
/// Persists first before broadcasting to in-memory subscribers - durable before ephemeral
pub struct EventBus {
    sender: broadcast::Sender<NoteEvent>,
    repository: Option<Arc<dyn Repository>>,
}

impl EventBus {
    pub fn new(capacity: usize, repository: Option<Arc<dyn Repository>>) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender, repository }
    }

    pub fn publish(&self, event: NoteEvent) -> crate::error::AppResult<()> {
        if let Some(repo) = &self.repository {
            repo.append_event(event.extract_patient_id(), event.clone())?;
        }

        let _ = self.sender.send(event);
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<NoteEvent> {
        self.sender.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(256, None)
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::models::{PatientId};
    use crate::models::encounter::CaptureMode;


    /// Unit test for extract_patient_id() method.
    #[test]
    fn extract_patient_id() {
        let patient_id = PatientId::new_v4();
        let encounter_id = EncounterId::new_v4();
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
        let patient_id = PatientId::new_v4();
        let encounter_id = EncounterId::new_v4();
        let now = Utc::now();

        let cases: Vec<(NoteEvent, &'static str)> = vec![
            (NoteEvent::AudioStarted { patient_id, encounter_id, capture_mode: CaptureMode::Ambient, clinical_occurred_at: now }, "AudioStarted"),
            (NoteEvent::AudioStopped { patient_id, encounter_id, clinical_occurred_at: now }, "AudioStopped"),
            (NoteEvent::TranscriptReady { patient_id, encounter_id, text: "hello".into(), clinical_occurred_at: now }, "TranscriptReady"),
            (NoteEvent::ResultReceived {
                patient_id,
                result: crate::models::ClinicalResult {
                    id: uuid::Uuid::new_v4(),
                    patient_id,
                    source: crate::models::ResultSource::Manual,
                    timestamp: now,
                    result_type: crate::models::ResultType::Vital(crate::models::result::VitalSign {
                        vital_type: crate::models::result::VitalType::HeartRate,
                        value: 72.0,
                        unit: "bpm".into(),
                    }),
                },
                clinical_occurred_at: now,
            }, "ResultReceived"),
            (NoteEvent::TriggerMDM { patient_id, clinical_occurred_at: now }, "TriggerMDM"),
            (NoteEvent::ReviewReady { patient_id, clinical_occurred_at: now }, "ReviewReady"),
            (NoteEvent::Sign { patient_id, clinical_occurred_at: now }, "Sign"),
        ];

        for (event, expected) in cases {
            assert_eq!(event.event_type(), expected, "wrong event_type for {expected}");
            println!("event: {:?}, expected: {:?}", event, expected);
        }
    }

     #[test]
    fn extract_encounter_id() {
        let patient_id = PatientId::new_v4();
        let encounter_id = EncounterId::new_v4();
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
