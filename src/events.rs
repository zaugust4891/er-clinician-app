use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;
use std::sync::Arc;
use chrono::{DateTime, Utc};
use crate::models::{CaptureMode, ClinicalResult, PatientId};
use crate::repository::Repository;

/// Events that drive the note state machine and the processing pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NoteEvent {
    /// Clinician started recording for a patient.
    AudioStarted {
        patient_id: PatientId,
        capture_mode: CaptureMode,
        clinical_occurred_at: DateTime<Utc>,
    },
    /// Clinician stopped recording.
    AudioStopped {
        patient_id: PatientId,
        encounter_id: Uuid,
        clinical_occurred_at: DateTime<Utc>,
    },
    /// STT produced a transcript chunk.
    TranscriptReady {
        patient_id: PatientId,
        encounter_id: Uuid,
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
        let event = NoteEvent::AudioStarted {
            patient_id,
            capture_mode: CaptureMode::Ambient,
            clinical_occurred_at: Utc::now(),
        };

        println!("patient_id: {:?}", patient_id);
        assert_eq!(event.extract_patient_id(), patient_id);
    }

}
