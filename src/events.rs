use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::models::{CaptureMode, ClinicalResult, PatientId};

/// Events that drive the note state machine and the processing pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NoteEvent {
    /// Clinician started recording for a patient.
    AudioStarted {
        patient_id: PatientId,
        capture_mode: CaptureMode,
    },
    /// Clinician stopped recording.
    AudioStopped {
        patient_id: PatientId,
        encounter_id: Uuid,
    },
    /// STT produced a transcript chunk.
    TranscriptReady {
        patient_id: PatientId,
        encounter_id: Uuid,
        text: String,
    },
    /// A clinical result was ingested (manual or FHIR).
    ResultReceived {
        patient_id: PatientId,
        result: ClinicalResult,
    },
    /// Clinician triggered MDM generation.
    TriggerMDM { patient_id: PatientId },
    /// MDM draft is ready for review.
    ReviewReady { patient_id: PatientId },
    /// Clinician signed the note.
    Sign { patient_id: PatientId },
}

impl NoteEvent {
    pub fn extract_patient_id(&self) -> PatientId {
        match self {
            NoteEvent::AudioStarted { patient_id, .. } => *patient_id,
            NoteEvent::AudioStopped { patient_id, .. } => *patient_id,
            NoteEvent::TranscriptReady { patient_id, .. } => *patient_id,
            NoteEvent::ResultReceived { patient_id, .. } => *patient_id,
            NoteEvent::TriggerMDM { patient_id } => *patient_id,
            NoteEvent::ReviewReady { patient_id } => *patient_id,
            NoteEvent::Sign { patient_id } => *patient_id,
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
pub struct EventBus {
    sender: broadcast::Sender<NoteEvent>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Publish an event to all subscribers.
    pub fn publish(&self, event: NoteEvent) -> crate::error::AppResult<()> {
        // Ignore send error (no receivers) — not a failure condition at startup.
        let _ = self.sender.send(event);
        Ok(())
    }

    /// Subscribe to the event stream.
    pub fn subscribe(&self) -> broadcast::Receiver<NoteEvent> {
        self.sender.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(256)
    }
}
