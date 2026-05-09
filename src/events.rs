use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use std::sync::Arc;
use chrono::{DateTime, Utc};
use crate::models::{CaptureMode, ClinicalResult, EncounterId, PatientId};
use crate::repository::Repository;
use crate::error::AppResult;


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

    pub fn audio_started(patient_id: PatientId, encounter_id: EncounterId, capture_mode: CaptureMode) -> Self {
        NoteEvent::AudioStarted { patient_id, encounter_id, capture_mode, clinical_occurred_at: Utc::now() }
    }

    pub fn audio_stopped(patient_id: PatientId, encounter_id: EncounterId) -> Self {
        NoteEvent::AudioStopped { patient_id, encounter_id, clinical_occurred_at: Utc::now() }
    }

    pub fn transcript_ready(patient_id: PatientId, encounter_id: EncounterId, text: String, clinical_occurred_at: DateTime<Utc>) -> Self {
        NoteEvent::TranscriptReady { patient_id, encounter_id, text, clinical_occurred_at }
    }

    pub fn result_received(patient_id: PatientId, result: ClinicalResult, clinical_occurred_at: DateTime<Utc>) -> Self {
        NoteEvent::ResultReceived { patient_id, result, clinical_occurred_at }
    }

    pub fn trigger_mdm(patient_id: PatientId) -> Self {
        NoteEvent::TriggerMDM { patient_id, clinical_occurred_at: Utc::now() }
    }

    pub fn review_ready(patient_id: PatientId) -> Self {
        NoteEvent::ReviewReady { patient_id, clinical_occurred_at: Utc::now() }
    }

    pub fn sign(patient_id: PatientId) -> Self {
        NoteEvent::Sign { patient_id, clinical_occurred_at: Utc::now() }
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

use crate::state::{apply_event, PatientNoteStore};
use crate::models::{NotePhase, PatientNote};
/// Broadcast event bus — multiple consumers, single producer per event.
/// Persists first before broadcasting to in-memory subscribers - durable before ephemeral
pub struct EventBus {
    sender: broadcast::Sender<NoteEvent>,
    repository: Option<Arc<dyn Repository>>,
}
///
/// EventBus::publish()` takes a `&PatientNoteStore` parameter and updates 
/// the in-memory projection under the same control flow that persists the event. 
/// Snapshots fire when the resulting phase is `Idle`, `Finalizing`, or `Signed`.

impl EventBus {
    pub fn new(capacity: usize, repository: Option<Arc<dyn Repository>>) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender, repository }
    }

    pub fn publish(&self, event: NoteEvent, store: &PatientNoteStore) -> AppResult<u64> {
        let patient_id = event.extract_patient_id();

        // 1. Persist-first (durable before ephemeral).
        let seq_id = if let Some(repo) = &self.repository {
            repo.append_event(patient_id, event.clone())?
        } else {
            0
        };

        // 2. Synchronously project to the in-memory store.
        // This is creative improvement #7 — the DashMap and SQLite stay consistent because 
        // they are updated in the same code path. Broadcast subscribers never touch state.
        
        let phase_after = {
            let mut entry = store.entry(patient_id).or_insert_with(|| {
                PatientNote::new(patient_id)
            });
            apply_event(&mut *entry, &event);
            entry.phase.clone()
            // DashMap write lock on this shard is dropped - NotePhase escapes 

        };

        // 3. Trigger snapshot on significant phase transitions.
        //    Only when we actually persisted (seq_id != 0) and a repo exists.
        if seq_id != 0 {
            if let Some(repo) = &self.repository {
                if matches!(
                    phase_after,
                    NotePhase::Idle | NotePhase::Finalizing | NotePhase::Signed
                ) {
                    // Read the full note back from the store under a short-lived
                    // borrow using store.get(&patient_id )for serialization. save_snapshot serializes
                    // under its own lock, so we release the DashMap lock
                    // first by cloning. Clones the PatientNote out of the ref immediately, 
                    // which then drops the read lock before save_snapshot is called. 
                    if let Some(snapshot) = store.get(&patient_id).map(|r| r.clone()) {
                        // Best-effort — a snapshot failure should not kill the publish path. Log it and move on.
                        if let Err(e) = repo.save_snapshot(patient_id, seq_id, &snapshot) {
                            tracing::warn!( 
                                patient_id = %patient_id,
                                sequence_id = %seq_id,
                                error = ?e,
                                "snapshot save failed - continuing without"
                            );
                        }
                    }
                }
            }
        }

        // 4. Broadcast (notifications only).
        let _ = self.sender.send(event);
        Ok(seq_id)
    }
    // ***NOTE***
    /// Subscribers get notified, but they're explicitly not responsible for state updates.
    /// ***UI code*** reads from the DashMap, which is now guaranteed current.
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
            (NoteEvent::AudioStarted { patient_id, encounter_id, capture_mode: CaptureMode::Ambient, clinical_occurred_at: now }, "AudioStarted"),
            (NoteEvent::AudioStopped { patient_id, encounter_id, clinical_occurred_at: now }, "AudioStopped"),
            (NoteEvent::TranscriptReady { patient_id, encounter_id, text: "hello".into(), clinical_occurred_at: now }, "TranscriptReady"),
            (NoteEvent::ResultReceived {
                patient_id,
                result: crate::models::ClinicalResult {
                    id: uuid::Uuid::now_v7(),
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
