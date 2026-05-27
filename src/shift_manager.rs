use crate::error::{AppError, AppResult};
use crate::{audio::AudioCapture, stt::Transcriber,};
use crate::events::{EventBus, NoteEvent};
use crate::models::{CaptureMode, EncounterId, PatientNote, PatientId, PatientRecord, ClinicalResult, NotePhase, Shift};
use crate::state::PatientNoteStore;
use crate::encounter_driver::EncounterDriver;


use std::sync::Arc;
use std::collections::HashMap;
use chrono::{DateTime, Utc};


pub struct ShiftManager<A: AudioCapture, T: Transcriber> {
    shift: Shift,
    patients: HashMap<PatientId, PatientRecord>,
    active_patient_id: Option<PatientId>,
    driver: EncounterDriver<A, T>,
    bus: Arc<EventBus>,
    store: PatientNoteStore,
}

impl<A: AudioCapture, T: Transcriber>ShiftManager<A, T> {
     pub fn new(
        shift: Shift,
        patients: HashMap<PatientId, PatientRecord>,
        driver: EncounterDriver<A, T>,
        bus: Arc<EventBus>,
        store: PatientNoteStore,
    ) -> Self {
        Self {
            shift,
            patients,
            active_patient_id: None,
            driver,
            bus,
            store,
        }
    }
    
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

    pub fn add_lab(&mut self, result: ClinicalResult, clinical_occurred_at: DateTime<Utc>) -> AppResult<()> {
        
        let patient_id = self.check_active_patient_id()?;

        if result.patient_id != patient_id {
            Err(AppError::LabResultError("Lab Result's patient Id does not match active patient id.".to_string()))?;
        }


        let lab_result_event = NoteEvent::result_received(patient_id, result, clinical_occurred_at); 
        
        self.bus.publish(lab_result_event, &self.store)?;

        Ok(())
        
    }

    pub fn trigger_mdm(&mut self) -> AppResult<()> {
        let patient_id = self.check_active_patient_id()?;

        let trigger_mdm_event = NoteEvent::trigger_mdm(patient_id);

        self.bus.publish(trigger_mdm_event, &self.store)?;
        
        Ok(())
    }

    pub fn review_ready(&mut self) -> AppResult<()> {
        let patient_id = self.check_active_patient_id()?;
        let review_ready = NoteEvent::review_ready(patient_id);

        self.bus.publish(review_ready, &self.store)?;
        
        Ok(())
    }

    pub fn sign(&mut self) -> AppResult<()> {
        let patient_id = self.check_active_patient_id()?;
        let signed_note = NoteEvent::sign(patient_id);

        self.bus.publish(signed_note, &self.store)?;

        Ok(())
    }

    pub fn get_note(&self, patient_id: PatientId) -> Option<PatientNote> {
        self.store.get(&patient_id).map(|r| r.clone())
    }

    pub fn list_patients(&self) -> Vec<(PatientId, String, NotePhase)> {
        self.patients
            .iter()
            .map(|(id, record)| {
                let phase = self.store
                    .get(id)
                    .map(|n| n.phase.clone())
                    .unwrap_or(NotePhase::Registered);
                (*id, record.chief_complaint.clone(), phase)
            })
            .collect()
    }

    pub fn end_shift(&mut self) -> AppResult<()> {
        if self.shift.is_active() {
            self.stop_encounter()?;
        }

        self.shift.end_shift();
        
        Ok(())
    }

    fn check_active_patient_id(&self) -> AppResult<PatientId> {
        self.active_patient_id
            .ok_or_else(|| AppError::NoActivePatient("No active patient found.".to_string()))

    } 




}