use std::sync::Arc;

use uuid::Uuid;

use crate::infrastructure::audio::MockAudioCapture;
use crate::application::encounter_driver::EncounterDriver;
use crate::application::EventBus;
use crate::domain::NoteEvent;
use crate::domain::{CaptureMode, PatientId, NotePhase};
use crate::infrastructure::persistence::repository::{Repository, SqliteRepository};
use crate::application::load::{self as load, PatientNoteStore};
use crate::infrastructure::stt::MockTranscriber;

struct CrashRecoverySetup {
    repo: Arc<dyn Repository>,
    bus: Arc<EventBus>,
    store: PatientNoteStore,
    driver: EncounterDriver<MockAudioCapture, MockTranscriber>,
    patient_id: PatientId,
}

fn setup_crash_recovery_stack() -> CrashRecoverySetup {
    let conn = crate::infrastructure::persistence::db::open_memory_db().unwrap();
    let repo: Arc<dyn Repository> = Arc::new(SqliteRepository::new(conn));
    let bus = Arc::new(EventBus::new(32, Some(repo.clone())));
    let store = load::new_store();

    let audio = MockAudioCapture::new();
    let transcriber = MockTranscriber;
    let driver = EncounterDriver::new(audio, transcriber, bus.clone(), store.clone());

    CrashRecoverySetup {
        repo,
        bus,
        store,
        driver,
        patient_id: Uuid::now_v7(),
    }
}

#[test]
fn crash_recovery_test_module_is_registered() {
    assert!(true);
}

#[test]
fn crash_recovery_setup_stack_compiles() {
    let setup = setup_crash_recovery_stack();

    assert_eq!(setup.repo.event_count().unwrap(), 0);
    assert_eq!(setup.repo.snapshot_count().unwrap(), 0);
    assert!(setup.store.is_empty());
    assert!(!setup.driver.has_active_encounter());

    let _receiver = setup.bus.subscribe();
    let loaded = load::load_patient_note(setup.repo.as_ref(), setup.patient_id).unwrap();
    assert!(loaded.is_none());
}


#[test]
fn full_lifecycle_survives_simulated_crash() {
    let mut setup = setup_crash_recovery_stack();

    let first_encounter_id = setup
        .driver
        .start_encounter(setup.patient_id, CaptureMode::Ambient)
        .unwrap();

    let first_stopped_id = setup.driver.stop_encounter().unwrap();

    assert_eq!(first_stopped_id, first_encounter_id);

    let second_encounter_id = setup
        .driver
        .start_encounter(setup.patient_id, CaptureMode::Dictation)
        .unwrap();

    let second_stopped_id = setup.driver.stop_encounter().unwrap();

    assert_eq!(second_stopped_id, second_encounter_id);

    let note = setup.store.get(&setup.patient_id).unwrap();

    assert_eq!(note.phase, NotePhase::Idle);
    assert_eq!(note.encounter_count, 2);
    assert_eq!(note.encounters.len(), 2);

    assert_eq!(note.encounters[0].capture_mode, CaptureMode::Ambient);
    assert_eq!(note.encounters[1].capture_mode, CaptureMode::Dictation);

    assert!(note.encounters[0].ended_at.is_some());
    assert!(note.encounters[1].ended_at.is_some());

    assert!(note.encounters[0].transcript.is_some());
    assert!(note.encounters[1].transcript.is_some());

    drop(note);

    setup
        .bus
        .publish(NoteEvent::trigger_mdm(setup.patient_id), &setup.store)
        .unwrap();
    setup
        .bus
        .publish(NoteEvent::review_ready(setup.patient_id), &setup.store)
        .unwrap();

    let note = setup.store.get(&setup.patient_id).unwrap();

    assert_eq!(note.phase, NotePhase::PendingSignOff);
    assert_eq!(note.encounter_count, 2);

    let expected = note.clone();
    drop(note);

    let CrashRecoverySetup {
        repo,
        bus,
        store,
        driver,
        patient_id,
    } = setup;

    drop(driver);
    drop(store);
    drop(bus);

    assert_eq!(expected.patient_id, patient_id);
    assert_eq!(expected.phase, NotePhase::PendingSignOff);
    assert_eq!(expected.encounter_count, 2);
    assert_eq!(repo.event_count().unwrap(), 8);
    assert_eq!(repo.snapshot_count().unwrap(), 1);

    let loaded = load::load_patient_note(repo.as_ref(), patient_id)
        .unwrap()
        .expect("patient note should reload from SQLite after simulated crash");

    assert_eq!(loaded.patient_id, expected.patient_id);
    assert_eq!(loaded.phase, NotePhase::PendingSignOff);
    assert_eq!(loaded.phase, expected.phase);
    assert_eq!(loaded.encounter_count, 2);
    assert_eq!(loaded.encounter_count, expected.encounter_count);
    assert_eq!(loaded.encounters.len(), 2);
    assert_eq!(loaded.encounters.len(), expected.encounters.len());
    assert_eq!(loaded.results.len(), expected.results.len());
    assert_eq!(loaded.version, expected.version);

    assert_eq!(loaded.encounters[0].capture_mode, CaptureMode::Ambient);
    assert_eq!(loaded.encounters[1].capture_mode, CaptureMode::Dictation);
    assert!(loaded.encounters[0].ended_at.is_some());
    assert!(loaded.encounters[1].ended_at.is_some());
    assert!(loaded.encounters[0].transcript.is_some());
    assert!(loaded.encounters[1].transcript.is_some());
}

#[test]
fn switch_patient_mid_encounter_persists_both() {
    let mut setup = setup_crash_recovery_stack();
    let patient_a = Uuid::now_v7();
    let patient_b = Uuid::now_v7();

    setup
        .driver
        .start_encounter(patient_a, CaptureMode::Ambient)
        .unwrap();

    setup
        .driver
        .switch_patient(patient_b, CaptureMode::Ambient)
        .unwrap();

    setup.driver.stop_encounter().unwrap();

    let note_a = setup.store.get(&patient_a).unwrap();
    assert_eq!(note_a.phase, NotePhase::Idle);
    assert_eq!(note_a.encounter_count, 1);
    assert_eq!(note_a.encounters.len(), 1);
    assert_eq!(note_a.encounters[0].capture_mode, CaptureMode::Ambient);
    assert!(note_a.encounters[0].ended_at.is_some());
    assert!(note_a.encounters[0].transcript.is_some());
    let expected_a = note_a.clone();
    drop(note_a);

    let note_b = setup.store.get(&patient_b).unwrap();
    assert_eq!(note_b.phase, NotePhase::Idle);
    assert_eq!(note_b.encounter_count, 1);
    assert_eq!(note_b.encounters.len(), 1);
    assert_eq!(note_b.encounters[0].capture_mode, CaptureMode::Ambient);
    assert!(note_b.encounters[0].ended_at.is_some());
    assert!(note_b.encounters[0].transcript.is_some());
    let expected_b = note_b.clone();
    drop(note_b);

    let CrashRecoverySetup {
        repo,
        bus,
        store,
        driver,
        patient_id: _,
    } = setup;

    drop(driver);
    drop(store);
    drop(bus);

    assert_eq!(repo.event_count().unwrap(), 6);
    assert_eq!(repo.snapshot_count().unwrap(), 2);

    let loaded_a = load::load_patient_note(repo.as_ref(), patient_a)
        .unwrap()
        .expect("patient A should reload from SQLite after switch crash");
    let loaded_b = load::load_patient_note(repo.as_ref(), patient_b)
        .unwrap()
        .expect("patient B should reload from SQLite after switch crash");

    assert_eq!(loaded_a.patient_id, expected_a.patient_id);
    assert_eq!(loaded_a.phase, NotePhase::Idle);
    assert_eq!(loaded_a.phase, expected_a.phase);
    assert_eq!(loaded_a.encounter_count, 1);
    assert_eq!(loaded_a.encounter_count, expected_a.encounter_count);
    assert_eq!(loaded_a.encounters.len(), 1);
    assert_eq!(loaded_a.encounters.len(), expected_a.encounters.len());
    assert_eq!(loaded_a.encounters[0].capture_mode, CaptureMode::Ambient);
    assert!(loaded_a.encounters[0].ended_at.is_some());
    assert!(loaded_a.encounters[0].transcript.is_some());

    assert_eq!(loaded_b.patient_id, expected_b.patient_id);
    assert_eq!(loaded_b.phase, NotePhase::Idle);
    assert_eq!(loaded_b.phase, expected_b.phase);
    assert_eq!(loaded_b.encounter_count, 1);
    assert_eq!(loaded_b.encounter_count, expected_b.encounter_count);
    assert_eq!(loaded_b.encounters.len(), 1);
    assert_eq!(loaded_b.encounters.len(), expected_b.encounters.len());
    assert_eq!(loaded_b.encounters[0].capture_mode, CaptureMode::Ambient);
    assert!(loaded_b.encounters[0].ended_at.is_some());
    assert!(loaded_b.encounters[0].transcript.is_some());
}
