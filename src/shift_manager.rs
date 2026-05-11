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
}