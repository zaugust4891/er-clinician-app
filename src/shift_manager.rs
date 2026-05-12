use crate::error::{AppError, AppResult};
use crate::{audio::AudioCapture, stt::Transcriber,};
use crate::events::{EventBus, NoteEvent};
use crate::models::{CaptureMode, EncounterId, PatientNote, PatientId, PatientRecord, Shift};
use crate::state::PatientNoteStore;
use crate::encounter_driver::EncounterDriver;

use std::sync::Arc;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use uuid::{Uuid};


pub struct ShiftManager<A: AudioCapture, T: Transcriber> {
    shift: Shift,
    patients: HashMap<PatientId, PatientRecord>,
    active_patient_id: Option<PatientId>,
    driver: EncounterDriver<A, T>,
    bus: Arc<EventBus>,
    store: PatientNoteStore,
}

impl<A: AudioCapture, T: Transcriber>ShiftManager<A, T> {

    pub fn register_patient(&mut self, chief_complaint: &str) -> PatientId {
        // create PatientRecord
        let patient_record = PatientRecord::new(chief_complaint);
        let patient_id = patient_record.id;
        // add patient record to Shift
        self.shift.add_patient(patient_record.id);
        // add patient to shift manager patients map.
        self.patients.insert(patient_id, patient_record);
        // insert a new empty PatientNote into the store.
        self.store.insert(patient_id, PatientNote::new(patient_id));
        
        patient_id
        
    }

    

    pub fn start_encounter(&mut self, mode: CaptureMode) -> AppResult<EncounterId> {

        let patient_id = self.active_patient_id.ok_or_else(|| { AppError::Audio(
                "No active patient. Please register and select one first."
                    .to_string()
            )
        })?;

        self.driver.start_encounter(patient_id, mode)
        
    }

    pub fn stop_encounter(&mut self) -> AppResult<EncounterId> {
        self.driver.stop_encounter()

    }
    
    pub fn set_active_patient(&mut self, new_id: PatientId ) -> AppResult<()> {
        // Validate the patient is actually registered in this shift
        if !self.patients.contains_key(&new_id) {
            return Err(AppError::PatientNotFound(new_id));
        } 

        // No-op - if it's already the active patient.
        if self.active_patient_id == Some(new_id) {
            return Ok(());
        }

        // If an encounter is in progress (for the OLD patient), close it cleanly.
        if self.driver.has_active_encounter() {
            self.driver.stop_encounter()?;
        }

        // Switch context. We do NOT auto-start a new encounter - that's a separate clinician action.
        self.active_patient_id = Some(new_id);

        Ok(())
    }


}