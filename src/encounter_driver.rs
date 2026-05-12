use crate::error::{AppError, AppResult};
use crate::{audio::AudioCapture, stt::Transcriber,};
use crate::events::{EventBus, NoteEvent};
use crate::models::{PatientId, EncounterId, CaptureMode};
use crate::state::PatientNoteStore;

use std::sync::Arc;
use chrono::{DateTime, Utc};
use uuid::{Uuid};

pub struct ActiveEncounter {
    patient_id: PatientId,
    encounter_id: EncounterId,
    started_at: DateTime<Utc>,
}
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

    ///TO DO: Implement stop_encounter()
    /// Purpose: 
    /// Stop recording, 
    /// Run speech-to-text on the captured audio, 
    /// Publish both 'AudioStopped' and TranscriptReady events
    /// 
    /// Note: 
    /// AudioStopped should publish BEFORE transcription
    /// Transcription takes time - Whisper will take seconds,
    /// The state machine should transition to 'Idle' the moment the clinician stops talking, not after transcription finishes
    /// (This is triggered on the UI by a 'stop-tap' or a 'direct switch', meaning a voice activated command switch or tapping a different patients button. 
    ///   e.g, the doctor switches through  voice) 
    pub fn stop_encounter(&mut self) -> AppResult<EncounterId> {
        let active = self.active_encounter.take().ok_or_else(|| {
            AppError::Audio("no encounter in progress".to_string())
        })?;
        
        let pcm_bytes = self.audio.stop()?;
        
        self.bus.publish(NoteEvent::audio_stopped(active.patient_id, active.encounter_id), &self.store)?;

        let transcript_result = self.transcriber.transcribe(&pcm_bytes)?;

        self.bus.publish(NoteEvent::transcript_ready(
            active.patient_id,
            active.encounter_id, 
            transcript_result.text,
            Utc::now(),
        ), &self.store)?;

        Ok(active.encounter_id)
    }


    /// This will be used for Active patient context + 'switch_patient`
    /// When the clinician taps the Apple Watch/Iphone to switch patients
    /// the system needs to:
    /// - stop the current encounter, 
    /// - flush its audio/transcript, and start a new encounter for the new patient
    ///  -- all atomically from the clinician's perspective.
    pub fn switch_patient(&mut self, new_patient_id: PatientId, mode: CaptureMode) -> AppResult<EncounterId> {
        if self.active_encounter.is_some() {
            self.stop_encounter()?;
        }

        let new_encounter_id = self.start_encounter(new_patient_id, mode);
        new_encounter_id
    }

    pub fn has_active_encounter(&self) -> bool {
        self.active_encounter.is_some()
    }

    pub fn active_patient_id(&self) -> Option<PatientId> {
        self.active_encounter.as_ref().map(|e| e.patient_id)
    }



    








}





