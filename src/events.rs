use tokio::sync::broadcast;
use uuid::Uuid;

use crate::models::{CaptureMode, ClinicalResult, PatientId};

/// Events that drive the note state machine and the processing pipeline.
#[derive(Debug, Clone)]
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
    pub fn publish(&self, event: NoteEvent) -> crate::error::Result<()> {
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
