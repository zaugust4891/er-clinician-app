use crate::error::{AppError, AppResult};
use crate::{audio::AudioCapture, stt::Transcriber,};
use crate::events::{EventBus, NoteEvent};
use crate::models::{PatientId, EncounterId, CaptureMode};
use crate::state::PatientNoteStore;

use std::sync::Arc;
use chrono::{DateTime, Utc};
use uuid::{Uuid};

pub struct EncounterDriver<A: AudioCapture, T: Transcriber> {
    audio: A,
    transcriber: T,
    bus: Arc<EventBus>,
    store: PatientNoteStore,
    active_encounter: Option<ActiveEncounter>,
}

impl<A: AudioCapture, T: Transcriber>EncounterDriver<A, T> {
    /// this method's purpose is to begin recording audio for a patient and publish the 'AudioStarted' event
    /// into the event-sourced pipeline.
    /// This will eventually be called by ShiftManager::set_active_patient() 
    /// (set_active_patient will be called by the quick-switch functionality which will be wired up to the UI's one-tap button on the 'active' screen.
    pub fn start_encounter(&mut self, patient_id: PatientId, mode: CaptureMode ) -> AppResult<EncounterId> {
        let encounter_id = Uuid::now_v7();
        // if encounter already active started throw error
        if self.active_encounter.is_some() {
            return Err(AppError::Audio("encounter already in progress.".to_string()));
        }

        self.audio.start(patient_id, encounter_id, mode.clone())?;

        self.bus.publish(NoteEvent::audio_started(patient_id, encounter_id, mode), &self.store)?;

        self.active_encounter = Some(ActiveEncounter { patient_id, encounter_id, started_at: Utc::now() });

        Ok(encounter_id)
    }
}

pub struct ActiveEncounter {
    patient_id: PatientId,
    encounter_id: EncounterId,
    started_at: DateTime<Utc>,
}



