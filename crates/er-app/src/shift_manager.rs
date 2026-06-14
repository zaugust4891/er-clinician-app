use crate::EventBus;
use crate::conversions::disposition;
use crate::encounter_driver::EncounterDriver;
use crate::load::PatientNoteStore;
use crate::shift_snapshot::ShiftSnapshot;
use er_domain::NoteEvent;
use er_domain::NoteSections;
use er_domain::replay::{apply_event, replay};
use er_domain::{
    CaptureMode, ClinicalResult, Diagnosis, Disposition, DomainError, EncounterId, Medication,
    MedicationKind,
    NotePhase, PatientId, PatientNote, PatientRecord, Shift, ShiftId,
};
use crate::error::AppError::PatientNotFound;
use crate::error::{AppError, AppResult};
use er_ai::deidentify::Deidentifier;
use er_ai::summarizer::Summarizer;
use er_pipeline::audio::AudioCapture;
use er_store::repository::Repository;
use er_pipeline::stt::Transcriber;
use crate::export::format_note;
use er_domain::note::format_result_line;
use crate::shift_manager::disposition::format_disposition_for_mdm;

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::runtime::Runtime;
use uuid::Uuid;

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

impl<A, T, S> ShiftManager<A, T, S>
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
        let patient_id = self.active_patient_id.ok_or_else(|| {
            AppError::Audio("No active patient. Please register and select one first.".to_string())
        })?;

        self.driver.start_encounter(patient_id, mode)
    }

    pub fn stop_encounter(&mut self) -> AppResult<EncounterId> {
        // 1. Driver publishes AudioStopped + TranscriptReady 
        // (state  Idle, snapshot).
        let encounter_id = self.driver.stop_encounter()?;
        
        // 2. Summarize and persist. 
        //      Best-effort: a summarization failure must NOT lose the encounter
        //          — the transcript is already durably persisted.
        let _ = self.summarize_encounter_deidentify_persist(encounter_id)
            .inspect_err(|e| tracing::warn!(error = %e, %encounter_id,
            "summarization failed; transcript retained."
        ));

        Ok(encounter_id)
    }

    /// Called by stop_encounter - preserves re-eval if network or claude api error
    /// Deidentifies patient BEFORE sending over Network to AWS - name becomes 'Patient_A', 'Patient_B' etc.
    /// Calls async summarize_encounter AFTER Deidentifying. 
    fn summarize_encounter_deidentify_persist(&mut self, encounter_id: EncounterId) -> AppResult<()> {
        let patient_id = self.check_active_patient_id()?;

         // Read fresh transcript + prior sections + encounter number from the store.
        let (transcript, prior, encounter_number) = {
            let note = self
                .store
                .get(&patient_id)
                .ok_or_else(|| AppError::NoteError(
                    format!("No note found in PatientNoteStore for patient: {patient_id}"
                )))?;
            let enc = note
                .encounters
                .iter()
                .find(|e| e.id == encounter_id);
            
            let transcript = enc
                .and_then(
                    |e| e.transcript
                        .clone()
                ).unwrap_or_default();

            (transcript, note.note_sections.clone(), note.encounter_count)
        };

        // handle nothing to summarize
        if transcript.trim().is_empty() {
            return Ok(());
        };

        // De-identify before the network call (F4 / HIPPA)
        let patient = self.patients
            .get(&patient_id)
            .cloned()
            .ok_or(PatientNotFound(patient_id))?;
        let safe_transcript = self.deidentifier.deidentify(&transcript, &patient);

        // Bridge sync -> async
        let sections = self.runtime.block_on(
            self.summarizer.summarize_encounter(&safe_transcript, &prior, encounter_number)
        )?;

        // Persist 
        self.bus.publish(NoteEvent::encounter_summarized(
            patient_id, 
            encounter_id, 
            sections, 
            vec![encounter_id]),
            &self.store, 
        )?;
        Ok(())
    }


    pub fn stop_active_encounter_for_shutdown(&mut self) -> AppResult<Option<EncounterId>> {
        if self.driver.has_active_encounter() {
            self.driver.stop_encounter().map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn set_active_patient(&mut self, new_id: PatientId) -> AppResult<()> {
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

    pub fn add_lab(
        &mut self,
        result: ClinicalResult,
        clinical_occurred_at: DateTime<Utc>,
    ) -> AppResult<()> {
        let patient_id = self.check_active_patient_id()?;

        if result.patient_id != patient_id {
            Err(AppError::LabResultError(
                "Lab Result's patient Id does not match active patient id.".to_string(),
            ))?;
        }

        let lab_result_event = NoteEvent::result_received(patient_id, result, clinical_occurred_at);

        self.bus.publish(lab_result_event, &self.store)?;

        Ok(())
    }

    pub fn set_disposition(&mut self, disposition: Disposition) -> AppResult<()> {
        let patient_id = self.check_active_patient_id()?;
        self.bus.publish(
            NoteEvent::disposition_set(patient_id, disposition),
            &self.store,
        )?;
        Ok(())
    }

    pub fn record_medication(
        &mut self,
        name: String,
        dose: String,
        route: Option<String>,
        kind: MedicationKind,
    ) -> AppResult<()> {
        let patient_id = self.check_active_patient_id()?;

        let medication = Medication {
            id: Uuid::now_v7(),
            patient_id,
            name,
            dose,
            route,
            rxnorm_code: None,
            kind,
            administered_at: Utc::now(),
            frequency: String::new(),
        };

        self.bus.publish(
            NoteEvent::medication_recorded(patient_id, medication),
            &self.store,
        )?;
        Ok(())
    }

    pub fn assign_diagnosis(
        &mut self,
        text: String,
        icd10_code: Option<String>,
        is_primary: bool,
    ) -> AppResult<()> {
        let patient_id = self.check_active_patient_id()?;

        let diagnosis = Diagnosis {
            id: Uuid::now_v7(),
            patient_id,
            text,
            icd10_code,
            is_primary,
        };

        self.bus.publish(
            NoteEvent::diagnosis_assigned(patient_id, diagnosis),
            &self.store,
        )?;
        Ok(())
    }

    pub fn record_data_reviewed(&mut self, physician_review: String) -> AppResult<()> {
        let patient_id = self.check_active_patient_id()?;
        self.bus.publish(
            NoteEvent::data_reviewed(patient_id, physician_review),
            &self.store,
        )?;
        Ok(())
    }

    pub fn trigger_mdm(&mut self) -> AppResult<()> {
        let patient_id = self.check_active_patient_id()?;
        self.ensure_no_active_encounter("trigger MDM")?;
        self.ensure_active_phase(NotePhase::Idle, "TriggerMDM")?;
        // 1. Snapshot the note's current clinical picture.
        // Tuple destructuring matches purely by position, not name — so the compiler won't warn you if the order is off. 
        // Just confirm the four values come out in the order you expect: (sections, objective_data, disposition_str, source_ids).
        let (sections, objective_data, disposition_str, source_ids) = {
            let note = self.get_note(patient_id).ok_or_else(|| AppError::NoteError(format!("No Note found for this patient.")))?;
            let sections = note.note_sections.clone();
            let objective_data = assemble_objective_data(&note);

            // Guard: refuse to call the model with nothing. If there's no HPI,
            // no objective data, and no disposition, there is nothing to reason
            // from — bail before a wasted API call and a hallucinated MDM.
            let has_hpi = sections
                .hpi
                .as_deref()
                .is_some_and(|hpi| !hpi.trim().is_empty());
            if !has_hpi && objective_data.trim().is_empty() && note.disposition.is_none() {
                return Err(AppError::NoteError(
                    "Nothing to generate an MDM from yet — record an encounter or add results first."
                        .to_string(),
                ));
            }

            let disposition_str = note.disposition.as_ref()
                .map(format_disposition_for_mdm)
                .unwrap_or_else(|| "Disposition not yet determined".to_string());
            let source_ids = note.encounters.iter().map(|en| en.id).collect::<Vec<_>>();
            (sections, objective_data, disposition_str, source_ids)
        };


         // 2. De-identify the objective-data blob 
         //   (sections already de-identified at summarize time).
        let patient = self.patients.get(&patient_id).cloned().ok_or_else(|| PatientNotFound(patient_id.clone()))?;
        let safe_objective_data = self.deidentifier.deidentify(&objective_data, &patient);

        // 3. Generate (sync → async bridge).
        let mdm = self.runtime.block_on(
            self.summarizer.generate_mdm(&sections, &safe_objective_data, &disposition_str,)
        )?;
        

        // 4. Persist the MDM, THEN drive the state transition.
        self.bus.publish(
            NoteEvent::mdm_generated(patient_id, mdm, source_ids), &self.store)?; 

        self.bus.publish(NoteEvent::trigger_mdm(patient_id), &self.store)?;
        // Idle -> Finalizing

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
        let note = self.get_note(patient_id).ok_or_else(|| {
            AppError::NoActivePatient("No note found for active patient.".to_string())
        })?;

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
            &note.medication,
            &note.diagnoses,
            &note.data_reviewed,
        ))
    }

    pub fn list_patients(&self) -> Vec<(PatientId, String, NotePhase)> {
        self.shift
            .patient_ids
            .iter()
            .filter_map(|id| {
                let record = self.patients.get(id)?;
                let phase = self
                    .store
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
        Ok(self.repo.event_count()?)
    }

    pub fn snapshot_count(&self) -> AppResult<usize> {
        Ok(self.repo.snapshot_count()?)
    }

    pub fn has_active_encounter(&self) -> bool {
        self.driver.has_active_encounter()
    }

    pub fn reload_active_patient(&mut self) -> AppResult<usize> {
        self.ensure_no_active_encounter("reload active patient")?;
        let patient_id = self.check_active_patient_id()?;

        let (note, replayed_count) =
            if let Some((sequence_id, mut note)) = self.repo.get_latest_snapshot(patient_id)? {
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
        Ok(self
            .store
            .get(&patient_id)
            .map(|n| n.phase.clone())
            .unwrap_or(NotePhase::Registered))
    }

    fn ensure_active_phase(&self, expected: NotePhase, event: &str) -> AppResult<()> {
        let phase = self.active_note_phase()?;
        if phase != expected {
            return Err(DomainError::InvalidTransition {
                from: phase,
                event: event.to_string(),
            }
            .into());
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

/// Render the note's structured facts into the plain-text "data reviewed"
/// blob that generate_mdm() consumes as `results_summary`
fn assemble_objective_data(note: &PatientNote) -> String {
    let mut out = String::new();
    if !note.results.is_empty() {
        out.push_str("Results:\n");
        for r in &note.results {
            out.push_str(&format!("- {}\n", format_result_line(r)
        )); 
        
        } 
    }
    if !note.medication.is_empty() {
        out.push_str("Medications:\n");
        for m in &note.medication {
            out.push_str(&format!(
                "- {}\n", m.summary_line()
            ));
        }

    }

    if !note.diagnoses.is_empty() {
        out.push_str("Diagnoses:\n");
        for d in &note.diagnoses {
            out.push_str(&format!("- {}{}\n", d.text, if d.is_primary {
                " (primary)"
            } else { "" }
            ));
        }
    }

    if !note.data_reviewed.is_empty() {
        out.push_str("Other data reviewed (orders/consults):\n");
        for s in &note.data_reviewed {
            out.push_str(&format!("- {s}\n"));
        }
    }
    out    
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
        let conn = er_store::db::open_memory_db().unwrap();
        let sql_lite_repo: Arc<dyn Repository> =
            Arc::new(er_store::repository::SqliteRepository::new(conn));
        let event_bus = Arc::new(EventBus::new(1024, Some(sql_lite_repo.clone())));

        let store: crate::load::PatientNoteStore =
            crate::load::new_store();
        let audio = er_pipeline::audio::MockAudioCapture::new();
        let transcriber = er_pipeline::stt::MockTranscriber;

        let encounter_driver = crate::encounter_driver::EncounterDriver::new(
            audio,
            transcriber,
            event_bus.clone(),
            store.clone(),
        );
        let summarizer = er_ai::summarizer::MockSummarizer::new();
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

        let result = shift_manager
            .runtime
            .block_on(shift_manager.summarizer.summarize_encounter(
                "test transcript",
                &NoteSections::default(),
                1,
            ));

        let sections = result.unwrap();
        assert_eq!(
            sections.hpi.as_deref(),
            Some("[mock HPI from encounter transcript]")
        );
        assert_eq!(sections.exam.as_deref(), Some("[mock exam findings]"));
    }

    #[test]
    fn assemble_objective_data_renders_all_sections() {
        let mut note = PatientNote::new(Uuid::now_v7());
        let patient_id = note.patient_id;

        note.diagnoses.push(Diagnosis {
            id: Uuid::now_v7(),
            patient_id,
            text: "Acute PE".into(),
            icd10_code: Some("I26.99".into()),
            is_primary: true,
        });
        note.diagnoses.push(Diagnosis {
            id: Uuid::now_v7(),
            patient_id,
            text: "Hypoxia".into(),
            icd10_code: None,
            is_primary: false,
        });
        note.data_reviewed.push("CTA chest: PE in RLL".into());

        let blob = assemble_objective_data(&note);

        // Primary diagnosis gets a space before "(primary)"; non-primary gets nothing.
        assert!(
            blob.contains("- Acute PE (primary)"),
            "primary dx should read 'Acute PE (primary)', got:\n{blob}"
        );
        assert!(
            blob.contains("- Hypoxia\n"),
            "non-primary dx should have no suffix, got:\n{blob}"
        );
        assert!(blob.contains("Diagnoses:"));
        assert!(blob.contains("Other data reviewed (orders/consults):"));
        assert!(blob.contains("- CTA chest: PE in RLL"));
    }

    #[test]
    fn assemble_objective_data_is_empty_for_blank_note() {
        let note = PatientNote::new(Uuid::now_v7());
        assert_eq!(assemble_objective_data(&note), "");
    }
}
