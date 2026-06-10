use crate::error::{AppError, AppResult};
use crate::infrastructure::audio::AudioCapture;
use crate::infrastructure::stt::Transcriber;
use crate::application::EventBus;
use crate::domain::NoteEvent;
use crate::export::format_note;
use crate::domain::{CaptureMode, EncounterId, PatientNote, PatientId, PatientRecord, ClinicalResult, Disposition, NotePhase, Shift, ShiftId};
use crate::domain::NoteSections;
use crate::persistence::ShiftSnapshot;
use crate::infrastructure::persistence::repository::Repository;
use crate::application::load::PatientNoteStore;
use crate::domain::replay::{apply_event, replay};
use crate::encounter_driver::EncounterDriver;
use crate::infrastructure::ai::summarizer::Summarizer;
use crate::infrastructure::ai::deidentify::Deidentifier;

use tokio::runtime::Runtime;
use std::sync::Arc;
use std::collections::HashMap;
use chrono::{DateTime, Utc};


pub struct ShiftManager<A, T, S> 
where
    A: AudioCapture,
    T: Transcriber,
    S: Summarizer,
{
    shift: Shift,
    patients: HashMap<PatientId, PatientRecord>,
    active_patient_id: Option<PatientId>,
    driver: EncounterDriver<A, T>,
    bus: Arc<EventBus>,
    store: PatientNoteStore,
    repo: Arc<dyn Repository>,
    summarizer: S,
    runtime: Arc<Runtime>,
    deidentifier: Deidentifier,
}

impl<A, T, S>ShiftManager<A, T, S> 
where
    A: AudioCapture,
    T: Transcriber,
    S: Summarizer,
{
     pub fn new(
        shift: Shift,
        patients: HashMap<PatientId, PatientRecord>,
        driver: EncounterDriver<A, T>,
        bus: Arc<EventBus>,
        store: PatientNoteStore,
        repo: Arc<dyn Repository>,
        summarizer: S,
        runtime: Arc<Runtime>,
    ) -> Self {
        Self {
            shift,
            patients,
            active_patient_id: None,
            driver,
            bus,
            store,
            repo,
            summarizer,
            runtime,
            deidentifier: Deidentifier::new(),
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

    pub fn stop_active_encounter_for_shutdown(&mut self) -> AppResult<Option<EncounterId>> {
        if self.driver.has_active_encounter() {
            self.driver.stop_encounter().map(Some)
        } else {
            Ok(None)
        }
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

    pub fn set_disposition(&mut self, disposition: Disposition) -> AppResult<()> {
        let patient_id = self.check_active_patient_id()?;
        self.bus.publish(NoteEvent::disposition_set(patient_id, disposition), &self.store)?;
        Ok(())
    }

    pub fn trigger_mdm(&mut self) -> AppResult<()> {
        self.ensure_no_active_encounter("trigger MDM")?;
        self.ensure_active_phase(NotePhase::Idle, "TriggerMDM")?;
        let patient_id = self.check_active_patient_id()?;

        let trigger_mdm_event = NoteEvent::trigger_mdm(patient_id);

        self.bus.publish(trigger_mdm_event, &self.store)?;
        
        Ok(())
    }

    pub fn review_ready(&mut self) -> AppResult<()> {
        self.ensure_no_active_encounter("mark review ready")?;
        self.ensure_active_phase(NotePhase::Finalizing, "ReviewReady")?;
        let patient_id = self.check_active_patient_id()?;
        let review_ready = NoteEvent::review_ready(patient_id);

        self.bus.publish(review_ready, &self.store)?;
        
        Ok(())
    }

    pub fn sign(&mut self) -> AppResult<()> {
        self.ensure_no_active_encounter("sign note")?;
        self.ensure_active_phase(NotePhase::PendingSignOff, "Sign")?;
        let patient_id = self.check_active_patient_id()?;
        let signed_note = NoteEvent::sign(patient_id);

        self.bus.publish(signed_note, &self.store)?;

        Ok(())
    }

    pub fn get_note(&self, patient_id: PatientId) -> Option<PatientNote> {
        self.store.get(&patient_id).map(|r| r.clone())
    }

    pub fn export_active_note(&self) -> AppResult<String> {
        let patient_id = self.check_active_patient_id()?;
        let patient = self
            .patients
            .get(&patient_id)
            .ok_or(AppError::PatientNotFound(patient_id))?;
        let note = self
            .get_note(patient_id)
            .ok_or_else(|| AppError::NoActivePatient("No note found for active patient.".to_string()))?;

        let mut sections = note.note_sections.clone();
        if sections_are_empty(&sections) {
            let transcript_text = transcripts_text(&note);
            if !transcript_text.is_empty() {
                sections.hpi = Some(transcript_text);
            }
        }

        let results_text = results_text(&note);
        Ok(format_note(
            patient,
            &sections,
            note.disposition.as_ref(),
            results_text.as_deref(),
        ))
    }

    pub fn list_patients(&self) -> Vec<(PatientId, String, NotePhase)> {
        self.shift
            .patient_ids
            .iter()
            .filter_map(|id| {
                let record = self.patients.get(id)?;
                let phase = self.store
                    .get(id)
                    .map(|n| n.phase.clone())
                    .unwrap_or(NotePhase::Registered);
                Some((*id, record.chief_complaint.clone(), phase))
            })
            .collect()
    }

    pub fn active_patient_id(&self) -> Option<PatientId> {
        self.active_patient_id
    }

    pub fn shift_id(&self) -> ShiftId {
        self.shift.id
    }

    pub fn event_count(&self) -> AppResult<usize> {
        self.repo.event_count()
    }

    pub fn snapshot_count(&self) -> AppResult<usize> {
        self.repo.snapshot_count()
    }

    pub fn has_active_encounter(&self) -> bool {
        self.driver.has_active_encounter()
    }

    pub fn reload_active_patient(&mut self) -> AppResult<usize> {
        self.ensure_no_active_encounter("reload active patient")?;
        let patient_id = self.check_active_patient_id()?;

        let (note, replayed_count) = if let Some((sequence_id, mut note)) =
            self.repo.get_latest_snapshot(patient_id)?
        {
            let delta = self.repo.get_events_since(patient_id, sequence_id)?;
            for stored in &delta {
                apply_event(&mut note, &stored.event);
            }
            (note, delta.len())
        } else {
            let events = self.repo.get_events(patient_id)?;
            if events.is_empty() {
                (PatientNote::new(patient_id), 0)
            } else {
                let owned_events: Vec<NoteEvent> =
                    events.iter().map(|stored| stored.event.clone()).collect();
                (replay(patient_id, &owned_events), events.len())
            }
        };

        self.store.insert(patient_id, note);
        Ok(replayed_count)
    }

    pub fn snapshot(&self) -> ShiftSnapshot {
        ShiftSnapshot {
            shift: self.shift.clone(),
            patients: self.patients.values().cloned().collect(),
        }
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

    fn active_note_phase(&self) -> AppResult<NotePhase> {
        let patient_id = self.check_active_patient_id()?;
        Ok(self.store
            .get(&patient_id)
            .map(|n| n.phase.clone())
            .unwrap_or(NotePhase::Registered))
    }

    fn ensure_active_phase(&self, expected: NotePhase, event: &str) -> AppResult<()> {
        let phase = self.active_note_phase()?;
        if phase != expected {
            return Err(AppError::InvalidTransition {
                from: phase,
                event: event.to_string(),
            });
        }

        Ok(())
    }

    fn ensure_no_active_encounter(&self, action: &str) -> AppResult<()> {
        if self.driver.has_active_encounter() {
            return Err(AppError::Audio(format!(
                "cannot {action} while an encounter is in progress; stop the encounter first"
            )));
        }

        Ok(())
    }




}

fn sections_are_empty(sections: &NoteSections) -> bool {
    sections.hpi.is_none()
        && sections.ros.is_none()
        && sections.exam.is_none()
        && sections.mdm.is_none()
        && sections.disposition_text.is_none()
}

fn transcripts_text(note: &PatientNote) -> String {
    note.encounters
        .iter()
        .filter_map(|encounter| {
            encounter
                .transcript
                .as_ref()
                .map(|text| format!("Encounter {}: {}", encounter.encounter_number, text))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn results_text(note: &PatientNote) -> Option<String> {
    if note.results.is_empty() {
        return None;
    }

    Some(
        note.results
            .iter()
            .map(|result| format!("{:?}", result.result_type))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Phase 3 Step 0a test
/// The test should construct a ShiftManager with MockSummarizer,
/// use its runtime to block_on a summarize_encounter call, 
/// and assert the mock HPI comes back. 
#[cfg(test)]

mod tests {
    use super::*;
    
    #[test]
    fn construct_shift_manager_with_mock_summarizer() {
        let conn = crate::infrastructure::persistence::db::open_memory_db().unwrap();
        let sql_lite_repo: Arc<dyn Repository> = Arc::new(crate::infrastructure::persistence::repository::SqliteRepository::new(conn));
        let event_bus = Arc::new(EventBus::new(1024, Some(sql_lite_repo.clone())));

        let store: crate::application::load::PatientNoteStore = crate::application::load::new_store();
        let audio = crate::infrastructure::audio::MockAudioCapture::new();
        let transcriber = crate::infrastructure::stt::MockTranscriber;

        let encounter_driver = crate::encounter_driver::EncounterDriver::new(audio, transcriber, event_bus.clone(), store.clone());
        let summarizer = crate::infrastructure::ai::summarizer::MockSummarizer::new();
        let rt = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let patients: HashMap<PatientId, PatientRecord> = HashMap::new();
        let shift = Shift::new();
        let shift_manager = ShiftManager::new(
            shift,
            patients,
            encounter_driver,
            event_bus.clone(),
            store.clone(),
            sql_lite_repo.clone(),
            summarizer,
            rt.clone(),
        );

        let result = shift_manager.runtime.block_on(
            shift_manager.summarizer.summarize_encounter(
                "test transcript",
                &NoteSections::default(), 
                1
            )
        );

        let sections = result.unwrap();
        assert_eq!(
            sections.hpi.as_deref(),
            Some("[mock HPI from encounter transcript]")
        );
        assert_eq!(
            sections.exam.as_deref(),
            Some("[mock exam findings]")
        );

    }

}
