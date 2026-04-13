use chrono::Utc;
use uuid::Uuid;

use crate::events::{EventBus, NoteEvent};
use crate::models::{CaptureMode, NotePhase};
use crate::repository::{Repository, SqliteRepository};

#[test]
fn publish_persists_projects_snapshots_and_loads() {
    let conn = crate::db::open_memory_db().unwrap();
    let repo = std::sync::Arc::new(SqliteRepository::new(conn));
    let store = crate::state::new_store();
    let bus = EventBus::new(32, Some(repo.clone()));

    let patient_id = Uuid::new_v4();
    let encounter_id = Uuid::new_v4();

    bus.publish(NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient), &store).unwrap();
    bus.publish(NoteEvent::transcript_ready(patient_id, encounter_id, "cp".into(), Utc::now()), &store).unwrap();
    bus.publish(NoteEvent::audio_stopped(patient_id, encounter_id), &store).unwrap();
    bus.publish(NoteEvent::trigger_mdm(patient_id), &store).unwrap();
    bus.publish(NoteEvent::review_ready(patient_id), &store).unwrap();
    bus.publish(NoteEvent::sign(patient_id), &store).unwrap();

    // In-memory store is current.
    assert_eq!(store.get(&patient_id).unwrap().phase, NotePhase::Signed);

    // Load from repository round-trips the state.
    let loaded = crate::state::load_patient_note(repo.as_ref(), patient_id)
        .unwrap()
        .expect("note exists");

    assert_eq!(loaded.phase, NotePhase::Signed);
    assert_eq!(loaded.encounter_count, 1);
    assert_eq!(loaded.encounters.len(), 1);
    assert_eq!(loaded.encounters[0].transcript.as_deref(), Some("cp"));

    // A snapshot was written at Signed (or earlier).
    let snap = repo.get_latest_snapshot(patient_id).unwrap();
    assert!(snap.is_some());
}

#[test]
fn load_nonexistent_patient_returns_none() {
    let conn = crate::db::open_memory_db().unwrap();
    let repo = SqliteRepository::new(conn);

    let loaded = crate::state::load_patient_note(&repo, Uuid::new_v4()).unwrap();
    assert!(loaded.is_none());
}

#[test]
fn load_uses_snapshot_delta_path() {
    // Drive to Idle (first snapshot trigger), then publish one more event,
    // then reload. The reloaded note should reflect the post-Idle event.
    let conn = crate::db::open_memory_db().unwrap();
    let repo = std::sync::Arc::new(SqliteRepository::new(conn));
    let store = crate::state::new_store();
    let bus = EventBus::new(32, Some(repo.clone()));

    let patient_id = Uuid::new_v4();
    let encounter_id = Uuid::new_v4();

    bus.publish(NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient), &store).unwrap();
    bus.publish(NoteEvent::audio_stopped(patient_id, encounter_id), &store).unwrap();
    // Snapshot at Idle.

    bus.publish(NoteEvent::trigger_mdm(patient_id), &store).unwrap();
    // Not yet snapshotted in the Finalizing case — a snapshot IS written on Finalizing.

    let loaded = crate::state::load_patient_note(repo.as_ref(), patient_id)
        .unwrap()
        .unwrap();
    assert_eq!(loaded.phase, NotePhase::Finalizing);
    assert_eq!(loaded.encounter_count, 1);
}

#[test]
fn corrupted_checksum_falls_back_to_full_replay() {
    let conn = crate::db::open_memory_db().unwrap();
    let repo = std::sync::Arc::new(SqliteRepository::new(conn));
    let store = crate::state::new_store();
    let bus = EventBus::new(32, Some(repo.clone()));

    let patient_id = Uuid::new_v4();
    let encounter_id = Uuid::new_v4();

    bus.publish(NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient), &store).unwrap();
    bus.publish(NoteEvent::audio_stopped(patient_id, encounter_id), &store).unwrap();
    // Snapshot written at Idle.

    // Corrupt the checksum.
    repo.execute_raw(
        "UPDATE snapshots SET checksum = 'bad' WHERE patient_id = ?1",
        &[&patient_id.to_string()],
    );

    // get_latest_snapshot returns None on checksum mismatch.
    let snap = repo.get_latest_snapshot(patient_id).unwrap();
    assert!(snap.is_none(), "corrupted checksum should return None");

    // load_patient_note still works via full replay fallback.
    let loaded = crate::state::load_patient_note(repo.as_ref(), patient_id)
        .unwrap()
        .expect("full replay succeeds");
    assert_eq!(loaded.phase, NotePhase::Idle);
    assert_eq!(loaded.encounter_count, 1);
}

#[test]
fn store_and_sqlite_stay_consistent() {
    let conn = crate::db::open_memory_db().unwrap();
    let repo = std::sync::Arc::new(SqliteRepository::new(conn));
    let store = crate::state::new_store();
    let bus = EventBus::new(32, Some(repo.clone()));

    let patient_id = Uuid::new_v4();
    let encounter_id = Uuid::new_v4();

    bus.publish(NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient), &store).unwrap();
    bus.publish(NoteEvent::transcript_ready(patient_id, encounter_id, "sob".into(), Utc::now()), &store).unwrap();
    bus.publish(NoteEvent::audio_stopped(patient_id, encounter_id), &store).unwrap();
    bus.publish(NoteEvent::trigger_mdm(patient_id), &store).unwrap();
    bus.publish(NoteEvent::review_ready(patient_id), &store).unwrap();
    bus.publish(NoteEvent::sign(patient_id), &store).unwrap();

    let from_store = store.get(&patient_id).unwrap().clone();
    let from_db = crate::state::load_patient_note(repo.as_ref(), patient_id)
        .unwrap()
        .expect("note exists");

    assert_eq!(from_store.phase, from_db.phase);
    assert_eq!(from_store.encounter_count, from_db.encounter_count);
    assert_eq!(from_store.results.len(), from_db.results.len());
    assert_eq!(from_store.encounters.len(), from_db.encounters.len());
    assert_eq!(
        from_store.encounters[0].transcript,
        from_db.encounters[0].transcript
    );
}