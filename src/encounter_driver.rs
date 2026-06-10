use crate::error::{AppError, AppResult};
use crate::{audio::AudioCapture, stt::Transcriber,};
use crate::application::EventBus;
use crate::domain::NoteEvent;
use crate::domain::{PatientId, EncounterId, CaptureMode};
use crate::state::PatientNoteStore;

use std::sync::Arc;
use chrono::Utc;
use uuid::{Uuid};

pub struct ActiveEncounter {
    patient_id: PatientId,
    encounter_id: EncounterId,
}
pub struct EncounterDriver<A: AudioCapture, T: Transcriber> {
    audio: A,
    transcriber: T,
    bus: Arc<EventBus>,
    store: PatientNoteStore,
    active_encounter: Option<ActiveEncounter>,
}

impl<A: AudioCapture, T: Transcriber>EncounterDriver<A, T> {

    pub fn new(audio: A, transcriber: T, bus: Arc<EventBus>, store: PatientNoteStore) -> Self {
        Self {
            audio,
            transcriber,
            bus,
            store,
            active_encounter: None,
        }
    }
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

        self.active_encounter = Some(ActiveEncounter { patient_id, encounter_id });

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::MockAudioCapture;
    use crate::domain::NoteEvent;
    use crate::domain::NotePhase;
    use crate::state;
    use crate::stt::MockTranscriber;

    fn test_driver() -> (
        EncounterDriver<MockAudioCapture, MockTranscriber>,
        tokio::sync::broadcast::Receiver<NoteEvent>,
        PatientNoteStore,
    ) {
        let bus = Arc::new(EventBus::new(16, None));
        let receiver = bus.subscribe();
        let store = state::new_store();
        let driver = EncounterDriver::new(
            MockAudioCapture::new(),
            MockTranscriber,
            bus,
            store.clone(),
        );

        (driver, receiver, store)
    }

    #[test]
    fn start_encounter_publishes_audio_started() {
        let (mut driver, mut receiver, store) = test_driver();
        let patient_id = Uuid::now_v7();

        let encounter_id = driver
            .start_encounter(patient_id, CaptureMode::Ambient)
            .unwrap();

        let event = receiver.try_recv().unwrap();
        match event {
            NoteEvent::AudioStarted {
                patient_id: event_patient_id,
                encounter_id: event_encounter_id,
                capture_mode,
                ..
            } => {
                assert_eq!(event_patient_id, patient_id);
                assert_eq!(event_encounter_id, encounter_id);
                assert_eq!(capture_mode, CaptureMode::Ambient);
            }
            other => panic!("expected AudioStarted, got {other:?}"),
        }

        let note = store.get(&patient_id).unwrap();
        assert_eq!(note.phase, NotePhase::Encountering);
        assert_eq!(note.encounter_count, 1);
    }

    #[test]
    fn stop_encounter_publishes_audio_stopped_then_transcript_ready() {
        let (mut driver, mut receiver, store) = test_driver();
        let patient_id = Uuid::now_v7();

        let encounter_id = driver
            .start_encounter(patient_id, CaptureMode::Dictation)
            .unwrap();
        let _started = receiver.try_recv().unwrap();

        let stopped_id = driver.stop_encounter().unwrap();
        assert_eq!(stopped_id, encounter_id);

        let first = receiver.try_recv().unwrap();
        let second = receiver.try_recv().unwrap();

        match first {
            NoteEvent::AudioStopped {
                patient_id: event_patient_id,
                encounter_id: event_encounter_id,
                ..
            } => {
                assert_eq!(event_patient_id, patient_id);
                assert_eq!(event_encounter_id, encounter_id);
            }
            other => panic!("expected AudioStopped first, got {other:?}"),
        }

        match second {
            NoteEvent::TranscriptReady {
                patient_id: event_patient_id,
                encounter_id: event_encounter_id,
                text,
                ..
            } => {
                assert_eq!(event_patient_id, patient_id);
                assert_eq!(event_encounter_id, encounter_id);
                assert!(text.contains("mock transcript"));
            }
            other => panic!("expected TranscriptReady second, got {other:?}"),
        }

        let note = store.get(&patient_id).unwrap();
        assert_eq!(note.phase, NotePhase::Idle);
        assert_eq!(note.encounters.len(), 1);
        assert!(note.encounters[0].ended_at.is_some());
        assert!(note.encounters[0].transcript.is_some());
    }

    #[test]
    fn double_start_returns_error() {
        let (mut driver, _receiver, _store) = test_driver();
        let patient_id = Uuid::now_v7();

        driver
            .start_encounter(patient_id, CaptureMode::Ambient)
            .unwrap();

        let err = driver
            .start_encounter(patient_id, CaptureMode::Dictation)
            .unwrap_err();

        assert!(err.to_string().contains("encounter already in progress"));
    }

    #[test]
    fn stop_without_start_returns_error() {
        let (mut driver, _receiver, _store) = test_driver();

        let err = driver.stop_encounter().unwrap_err();

        assert!(err.to_string().contains("no encounter in progress"));
    }

    #[test]
    fn switch_patient_stops_old_and_starts_new() {
        let (mut driver, mut receiver, store) = test_driver();
        let patient_a = Uuid::now_v7();
        let patient_b = Uuid::now_v7();

        let encounter_a = driver
            .start_encounter(patient_a, CaptureMode::Ambient)
            .unwrap();
        let _started_a = receiver.try_recv().unwrap();

        let encounter_b = driver
            .switch_patient(patient_b, CaptureMode::Dictation)
            .unwrap();

        let stopped_a = receiver.try_recv().unwrap();
        let transcript_a = receiver.try_recv().unwrap();
        let started_b = receiver.try_recv().unwrap();

        assert!(matches!(
            stopped_a,
            NoteEvent::AudioStopped {
                patient_id,
                encounter_id,
                ..
            } if patient_id == patient_a && encounter_id == encounter_a
        ));
        assert!(matches!(
            transcript_a,
            NoteEvent::TranscriptReady {
                patient_id,
                encounter_id,
                ..
            } if patient_id == patient_a && encounter_id == encounter_a
        ));
        assert!(matches!(
            started_b,
            NoteEvent::AudioStarted {
                patient_id,
                encounter_id,
                capture_mode: CaptureMode::Dictation,
                ..
            } if patient_id == patient_b && encounter_id == encounter_b
        ));

        let note_a = store.get(&patient_a).unwrap();
        assert_eq!(note_a.phase, NotePhase::Idle);
        assert!(note_a.encounters[0].transcript.is_some());
        drop(note_a);

        let note_b = store.get(&patient_b).unwrap();
        assert_eq!(note_b.phase, NotePhase::Encountering);
        assert_eq!(note_b.encounter_count, 1);
    }
}




