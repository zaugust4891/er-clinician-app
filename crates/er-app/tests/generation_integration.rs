//! Phase 3 Step 5a — HPI/Exam auto-summarization integration tests.
//!
//! Verifies the full wiring:
//!   stop_encounter -> TranscriptReady -> deidentify -> summarize_encounter
//!     -> EncounterSummarized -> note.note_sections.hpi/exam
//!
//! All tests use MockSummarizer (no network, deterministic output) and
//! MockAudioCapture/MockTranscriber, mirroring the Phase 2 harness.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use uuid::Uuid;

use er_app::EventBus;
use er_app::encounter_driver::EncounterDriver;
use er_app::load::{self as load};
use er_app::shift_manager::ShiftManager;
use er_domain::{
    CaptureMode, ClinicalResult, Disposition, LabResult, NotePhase, PatientId, PatientRecord,
    ResultSource, ResultType, Shift,
};
use er_ai::summarizer::MockSummarizer;
use er_pipeline::audio::MockAudioCapture;
use er_store::repository::{Repository, SqliteRepository};
use er_pipeline::stt::MockTranscriber;

// These tests drive everything through ShiftManager, whose sync command
// methods block_on an internal runtime. They must therefore run on ordinary
// #[test] threads (never #[tokio::test], which would run them on a runtime
// worker and panic: "Cannot start a runtime from within a runtime"). The only
// directly-async calls are the post-crash `load_patient_note` lookups, made
// after the manager (and its runtime) has been dropped — `block` runs those on
// a throwaway runtime, which is safe because the test thread is not a worker.
fn block<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Runtime::new().unwrap().block_on(fut)
}

/// The exact strings MockSummarizer::summarize_encounter returns.
const MOCK_HPI: &str = "[mock HPI from encounter transcript]";
const MOCK_EXAM: &str = "[mock exam findings]";
/// The exact string MockSummarizer::generate_mdm returns.
const MOCK_MDM: &str = "[mock MDM: moderate complexity, 2 diagnoses considered]";

/// Build a manual lab result for the given patient.
fn lab_result(patient_id: PatientId, name: &str, value: f64, unit: &str) -> ClinicalResult {
    ClinicalResult {
        id: Uuid::now_v7(),
        patient_id,
        source: ResultSource::Manual,
        timestamp: Utc::now(),
        result_type: ResultType::Lab(LabResult {
            fhir_id: None,
            loinc_code: None,
            display_name: name.into(),
            value,
            unit: unit.into(),
            reference_range: None,
            is_abnormal: false,
        }),
    }
}

struct GenerationSetup {
    manager: ShiftManager<MockAudioCapture, MockTranscriber, MockSummarizer>,
    repo: Arc<dyn Repository>,
}

fn setup_generation_stack() -> GenerationSetup {
    let conn = er_store::db::open_memory_db().unwrap();
    let repo: Arc<dyn Repository> = Arc::new(SqliteRepository::new(conn));
    let bus = Arc::new(EventBus::new(32, Some(repo.clone())));
    let store = load::new_store();
    let driver = EncounterDriver::new(
        MockAudioCapture::new(),
        MockTranscriber,
        bus.clone(),
        store.clone(),
    );
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());

    let manager = ShiftManager::new(
        Shift::new(),
        HashMap::<PatientId, PatientRecord>::new(),
        driver,
        bus,
        store,
        repo.clone(),
        MockSummarizer::new(),
        runtime,
        false,
    );

    GenerationSetup { manager, repo }
}

/// register -> switch -> start -> stop must populate HPI and Exam from the
/// summarizer, leave MDM/ROS untouched, and land the note in Idle.
#[test]
fn stop_encounter_populates_hpi_via_mock_summarizer() {
    let mut setup = setup_generation_stack();

    let patient_id = setup.manager.register_patient("chest pain");
    setup.manager.set_active_patient(patient_id).unwrap();

    setup.manager.start_encounter(CaptureMode::Ambient).unwrap();
    let encounter_id = setup.manager.stop_encounter().unwrap();

    let note = setup.manager.get_note(patient_id).unwrap();

    // The generated prose reached the note sections.
    assert_eq!(note.note_sections.hpi.as_deref(), Some(MOCK_HPI));
    assert_eq!(note.note_sections.exam.as_deref(), Some(MOCK_EXAM));

    // EncounterSummarized must never touch MDM (owned by MdmGenerated) or
    // ROS (mock returns None for it).
    assert_eq!(note.note_sections.mdm, None);
    assert_eq!(note.note_sections.ros, None);

    // The encounter itself is intact: stopped, transcribed, counted.
    assert_eq!(note.phase, NotePhase::Idle);
    assert_eq!(note.encounter_count, 1);
    assert_eq!(note.encounters[0].id, encounter_id);
    assert!(note.encounters[0].transcript.is_some());

    // Section history recorded one version each for hpi and exam.
    let hpi_versions = note
        .section_history
        .iter()
        .filter(|v| v.section == "hpi")
        .count();
    let exam_versions = note
        .section_history
        .iter()
        .filter(|v| v.section == "exam")
        .count();
    assert_eq!(hpi_versions, 1);
    assert_eq!(exam_versions, 1);

    // Four events persisted: AudioStarted, AudioStopped, TranscriptReady,
    // EncounterSummarized. (manager.event_count() is the sync wrapper that
    // block_on's the now-async repo call on the manager's own runtime.)
    assert_eq!(setup.manager.event_count().unwrap(), 4);
}

/// A second start -> stop must keep the HPI populated (the merge path runs
/// instead of blanking prior prose), and everything must survive a
/// simulated crash + reload from SQLite.
#[test]
fn second_encounter_keeps_hpi_and_survives_reload() {
    let mut setup = setup_generation_stack();

    let patient_id = setup.manager.register_patient("chest pain");
    setup.manager.set_active_patient(patient_id).unwrap();

    // Encounter 1.
    setup.manager.start_encounter(CaptureMode::Ambient).unwrap();
    setup.manager.stop_encounter().unwrap();

    // Encounter 2 — the re-eval / merge path.
    setup.manager.start_encounter(CaptureMode::Ambient).unwrap();
    setup.manager.stop_encounter().unwrap();

    let note = setup.manager.get_note(patient_id).unwrap();

    // HPI is still populated after the second summarization — the merge
    // augments, never blanks (acceptance criterion 4).
    assert_eq!(note.note_sections.hpi.as_deref(), Some(MOCK_HPI));
    assert_eq!(note.note_sections.exam.as_deref(), Some(MOCK_EXAM));
    assert_eq!(note.note_sections.mdm, None);

    assert_eq!(note.phase, NotePhase::Idle);
    assert_eq!(note.encounter_count, 2);
    assert_eq!(note.encounters.len(), 2);

    // Two summarizations -> two hpi versions in the audit trail.
    let hpi_versions = note
        .section_history
        .iter()
        .filter(|v| v.section == "hpi")
        .count();
    assert_eq!(hpi_versions, 2);

    // 2 encounters x 4 events each.
    assert_eq!(setup.manager.event_count().unwrap(), 8);

    let expected = note.clone();
    drop(note);

    // Simulated crash: drop the manager (and with it the in-memory DashMap,
    // bus, and driver), keeping only the SQLite handle.
    let GenerationSetup { manager, repo } = setup;
    drop(manager);

    let loaded = block(load::load_patient_note(repo.as_ref(), patient_id))
        .unwrap()
        .expect("note with generated sections should reload from SQLite");

    // The generated prose survived persistence — EncounterSummarized events
    // (and/or the Idle snapshot) round-trip through SQLite.
    assert_eq!(loaded.note_sections.hpi.as_deref(), Some(MOCK_HPI));
    assert_eq!(loaded.note_sections.exam.as_deref(), Some(MOCK_EXAM));
    assert_eq!(loaded.note_sections.mdm, None);
    assert_eq!(loaded.phase, expected.phase);
    assert_eq!(loaded.encounter_count, expected.encounter_count);
    assert_eq!(loaded.version, expected.version);
}

// ---------------------------------------------------------------------------
// Step 6a — one-button MDM generation
// ---------------------------------------------------------------------------

/// Drive a patient up to Idle with a generated HPI (one encounter), a lab,
/// a medication, a diagnosis, a data-reviewed note, and a disposition — i.e.
/// the full objective picture trigger_mdm assembles from.
fn seed_note_ready_for_mdm(
    manager: &mut ShiftManager<MockAudioCapture, MockTranscriber, MockSummarizer>,
) -> PatientId {
    let patient_id = manager.register_patient("chest pain");
    manager.set_active_patient(patient_id).unwrap();

    // One encounter -> HPI/Exam populated, note lands in Idle.
    manager.start_encounter(CaptureMode::Ambient).unwrap();
    manager.stop_encounter().unwrap();

    // Objective data feeding the MDM (exercises every assemble_objective_data branch).
    manager
        .add_lab(lab_result(patient_id, "Troponin", 0.04, "ng/mL"), Utc::now())
        .unwrap();
    manager
        .record_medication(
            "Aspirin".into(),
            "324mg".into(),
            Some("PO".into()),
            er_domain::MedicationKind::AdministeredInEd,
        )
        .unwrap();
    manager
        .assign_diagnosis("Acute PE".into(), Some("I26.99".into()), true)
        .unwrap();
    manager
        .record_data_reviewed("CTA chest: PE in RLL".into())
        .unwrap();
    manager
        .set_disposition(Disposition::Admit {
            service: "Hospitalist".into(),
            bed: None,
        })
        .unwrap();

    patient_id
}

/// trigger_mdm must generate the MDM via the summarizer, persist it onto the
/// note, and advance the phase Idle -> Finalizing.
#[test]
fn trigger_mdm_generates_persists_and_advances_phase() {
    let mut setup = setup_generation_stack();
    let patient_id = seed_note_ready_for_mdm(&mut setup.manager);

    // Sanity: pre-conditions for trigger_mdm hold.
    let before = setup.manager.get_note(patient_id).unwrap();
    assert_eq!(before.phase, NotePhase::Idle);
    assert_eq!(before.note_sections.mdm, None);
    let version_before = before.version;
    drop(before);

    setup.manager.trigger_mdm().unwrap();

    let note = setup.manager.get_note(patient_id).unwrap();

    // The headline: MDM text reached the note, phase advanced.
    assert_eq!(note.note_sections.mdm.as_deref(), Some(MOCK_MDM));
    assert_eq!(note.phase, NotePhase::Finalizing);

    // MdmGenerated + TriggerMDM each bump the version (2 mutations).
    assert_eq!(note.version, version_before + 2);

    // HPI from the earlier encounter is untouched by MDM generation.
    assert_eq!(note.note_sections.hpi.as_deref(), Some(MOCK_HPI));

    // The MDM was recorded in the section-history audit trail.
    let mdm_versions = note
        .section_history
        .iter()
        .filter(|v| v.section == "mdm")
        .count();
    assert_eq!(mdm_versions, 1);

    // source_encounter_ids on the MDM points at the single encounter.
    assert_eq!(note.encounters.len(), 1);
}

/// The generated MDM and the Finalizing phase must survive a simulated crash
/// (drop the in-memory DashMap) and reload from SQLite (acceptance crit. 7).
#[test]
fn generated_mdm_survives_reload() {
    let mut setup = setup_generation_stack();
    let patient_id = seed_note_ready_for_mdm(&mut setup.manager);
    setup.manager.trigger_mdm().unwrap();

    let expected = setup.manager.get_note(patient_id).unwrap();
    assert_eq!(expected.note_sections.mdm.as_deref(), Some(MOCK_MDM));
    assert_eq!(expected.phase, NotePhase::Finalizing);

    // Simulated crash: drop the manager (DashMap, bus, driver, runtime all go).
    let GenerationSetup { manager, repo } = setup;
    drop(manager);

    let loaded = block(load::load_patient_note(repo.as_ref(), patient_id))
        .unwrap()
        .expect("note with generated MDM should reload from SQLite");

    // Full clinical picture survives replay/snapshot: prose, MDM, disposition,
    // AND the structured objective data (meds, diagnoses, reviewed-data, labs).
    assert_eq!(loaded.note_sections.mdm.as_deref(), Some(MOCK_MDM));
    assert_eq!(loaded.phase, NotePhase::Finalizing);
    assert_eq!(loaded.note_sections.hpi.as_deref(), Some(MOCK_HPI));
    assert_eq!(loaded.version, expected.version);
    assert_eq!(loaded.disposition, expected.disposition);

    assert_eq!(loaded.results.len(), 1);
    assert_eq!(loaded.medication.len(), 1);
    assert_eq!(loaded.medication[0].name, "Aspirin");
    assert_eq!(loaded.diagnoses.len(), 1);
    assert_eq!(loaded.diagnoses[0].text, "Acute PE");
    assert!(loaded.diagnoses[0].is_primary);
    assert_eq!(loaded.data_reviewed, vec!["CTA chest: PE in RLL".to_string()]);
}

/// trigger_mdm must be rejected when the note is not in Idle — e.g. a fresh
/// Registered note with no encounter. Guards against generating from nothing
/// and from clobbering a note in the wrong phase.
#[test]
fn trigger_mdm_rejected_when_not_idle() {
    let mut setup = setup_generation_stack();
    let patient_id = setup.manager.register_patient("chest pain");
    setup.manager.set_active_patient(patient_id).unwrap();

    // Phase is Registered (no encounter yet) -> trigger_mdm must error.
    let result = setup.manager.trigger_mdm();
    assert!(result.is_err());

    let note = setup.manager.get_note(patient_id).unwrap();
    assert_eq!(note.phase, NotePhase::Registered);
    assert_eq!(note.note_sections.mdm, None);
}

// ---------------------------------------------------------------------------
// Step 6 guard — refuse to generate an MDM from an empty note
// ---------------------------------------------------------------------------

/// A transcriber that yields no text — simulates a started/stopped recording
/// with nothing said, so no HPI is ever summarized.
struct EmptyTranscriber;

impl er_pipeline::stt::Transcriber for EmptyTranscriber {
    fn transcribe(
        &self,
        _audio_pcm: &[u8],
    ) -> er_pipeline::error::PipelineResult<er_pipeline::stt::TranscriptResult> {
        Ok(er_pipeline::stt::TranscriptResult {
            text: String::new(),
            confidence: None,
            audio_duration_secs: 0.0,
            processing_duration_secs: 0.0,
        })
    }
}

/// An Idle note with no HPI, no objective data, and no disposition must be
/// refused by trigger_mdm's guard — before any (mock) generation runs.
#[test]
fn trigger_mdm_rejected_when_note_is_empty() {
    let conn = er_store::db::open_memory_db().unwrap();
    let repo: Arc<dyn Repository> = Arc::new(SqliteRepository::new(conn));
    let bus = Arc::new(EventBus::new(32, Some(repo.clone())));
    let store = load::new_store();
    let driver = EncounterDriver::new(
        MockAudioCapture::new(),
        EmptyTranscriber,
        bus.clone(),
        store.clone(),
    );
    let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
    let mut manager = ShiftManager::new(
        Shift::new(),
        HashMap::<PatientId, PatientRecord>::new(),
        driver,
        bus,
        store,
        repo,
        MockSummarizer::new(),
        runtime,
        false,
    );

    let patient_id = manager.register_patient("chest pain");
    manager.set_active_patient(patient_id).unwrap();

    // Encounter with an empty transcript -> reaches Idle, but no HPI summarized.
    manager.start_encounter(CaptureMode::Ambient).unwrap();
    manager.stop_encounter().unwrap();

    let before = manager.get_note(patient_id).unwrap();
    assert_eq!(before.phase, NotePhase::Idle);
    assert_eq!(before.note_sections.hpi, None);
    drop(before);

    // The guard fires: nothing to reason from.
    let result = manager.trigger_mdm();
    assert!(result.is_err());

    // Note is untouched — no MDM, still Idle (no wasted state transition).
    let note = manager.get_note(patient_id).unwrap();
    assert_eq!(note.note_sections.mdm, None);
    assert_eq!(note.phase, NotePhase::Idle);
}
