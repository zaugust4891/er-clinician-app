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

use er_note_app::application::EventBus;
use er_note_app::application::encounter_driver::EncounterDriver;
use er_note_app::application::load::{self as load};
use er_note_app::application::shift_manager::ShiftManager;
use er_note_app::domain::{CaptureMode, NotePhase, PatientId, PatientRecord, Shift};
use er_note_app::infrastructure::ai::summarizer::MockSummarizer;
use er_note_app::infrastructure::audio::MockAudioCapture;
use er_note_app::infrastructure::persistence::repository::{Repository, SqliteRepository};
use er_note_app::infrastructure::stt::MockTranscriber;

/// The exact strings MockSummarizer::summarize_encounter returns.
const MOCK_HPI: &str = "[mock HPI from encounter transcript]";
const MOCK_EXAM: &str = "[mock exam findings]";

struct GenerationSetup {
    manager: ShiftManager<MockAudioCapture, MockTranscriber, MockSummarizer>,
    repo: Arc<dyn Repository>,
}

fn setup_generation_stack() -> GenerationSetup {
    let conn = er_note_app::infrastructure::persistence::db::open_memory_db().unwrap();
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
    // EncounterSummarized.
    assert_eq!(setup.repo.event_count().unwrap(), 4);
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
    assert_eq!(setup.repo.event_count().unwrap(), 8);

    let expected = note.clone();
    drop(note);

    // Simulated crash: drop the manager (and with it the in-memory DashMap,
    // bus, and driver), keeping only the SQLite handle.
    let GenerationSetup { manager, repo } = setup;
    drop(manager);

    let loaded = load::load_patient_note(repo.as_ref(), patient_id)
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
