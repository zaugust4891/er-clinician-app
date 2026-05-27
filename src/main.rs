use er_note_app::audio::MockAudioCapture;
use er_note_app::db;
use er_note_app::encounter_driver::EncounterDriver;
use er_note_app::error::{AppError, AppResult};
use er_note_app::events::EventBus;
use er_note_app::models::{PatientId, PatientNote, PatientRecord, Shift};
use er_note_app::repository::{Repository, SqliteRepository};
use er_note_app::state::{self, PatientNoteStore, load_patient_note};
use er_note_app::stt::MockTranscriber;
use er_note_app::persistence::ShiftSnapshot;
use std::fs::File;
use std::io::{self, BufReader};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use serde_json::{Deserializer, Serializer};
use serde::de::DeserializeOwned;
use std::collections::HashMap;

// Helper with context to handle Ok or error for steps that return AppResult 
fn bail<T>(context: &str, r: AppResult<T>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{context}: {e}");
            std::process::exit(1);
        }
    }
}

fn deserialize_shift_json_file<P: AsRef<Path>, S: DeserializeOwned>(path: P) -> AppResult<S> {
    let file = File::open(path)?;
    let rdr = BufReader::new(file);
    let value = serde_json::from_reader(rdr)?;
    Ok(value)
}


fn main() {
    tracing_subscriber::fmt::init();

    tracing::info!("ED Note App starting");

    // Initialize in-memory database for demo.
    let sql_lite_db_path = "./er_notes.db";
    let conn = bail("Failed to open database", db::open_db(sql_lite_db_path));
    tracing::info!("database initialized ({} tables)", count_tables(&conn));

    let sql_lite_repo: Arc<dyn Repository> = Arc::new(SqliteRepository::new(conn));

    let event_bus = Arc::new(EventBus::new(1024, Some(sql_lite_repo.clone())));
    let store: PatientNoteStore = state::new_store();
    
    let audio = MockAudioCapture::new();
    let transcriber = MockTranscriber;

    let encounter_driver = EncounterDriver::new(audio, transcriber, event_bus.clone(), store.clone());
    let args: Vec<String> = env::args().collect();

    let snapshot: ShiftSnapshot = match deserialize_shift_json_file("./shift.json") {
        Ok(s) => s,
        Err(AppError::Io(e)) if e.kind() == io::ErrorKind::NotFound => {
            tracing::info!("no prior shift, starting fresh");
            ShiftSnapshot { shift: Shift::new(), patients: Vec::new() }
        }
        Err(e) => bail("loading shift.json", Err(e)),
    };
    let ShiftSnapshot { shift, patients } = snapshot;
    let patients: HashMap<PatientId, PatientRecord> = patients.into_iter().map(|p| (p.id, p)).collect();

    for (id, _) in &patients {
        match load_patient_note(&*sql_lite_repo, *id) {
            Ok(Some(note)) => { 
                store.insert(*id, note);
            }
            Ok(None) => tracing::warn!(patient_id = %id, "in shift.json but no events in DB."),
            Err(e) => bail("loading patient note", Err(e)),
        }
    }

/* 
- **Exists:** 
    read it, deserialize into a `Shift` and `Vec<PatientRecord>`. 
    For each patient record, call `load_patient_note(repo, patient_id)` 
    to reconstruct the note from SQLite and insert it back into the `PatientNoteStore`. 
    The active patient context (`active_patient_id`) is intentionally NOT persisted —   
    the clinician should re-select on resume, both for safety (confirms intent) and simplicity.
- **Doesn't exist:** 
    create a fresh `Shift::new()` and empty patient list.


-Build the `ShiftManager` from these pieces: `shift`, the rebuilt `patients` HashMap, `active_patient_id: None`, the driver, the bus, the store.
*/

    // Following code is from phase 1 - original state machine testing 
    //
    // // Create a shift and a patient.
    // let mut shift = Shift::new();
    // let patient = PatientRecord::new("chest pain");
    // shift.add_patient(patient.id);

    // tracing::info!(
    //     shift_id = %shift.id,
    //     patient_id = %patient.id,
    //     cc = %patient.chief_complaint,
    //     "shift started, patient registered"
    // );

    // // Create a note and exercise the state machine.
    // let mut note = PatientNote::new(patient.id);
    // tracing::info!(?note, "note created");

    // note.handle_audio_started();
    // tracing::info!(?note, "encounter started");

    // note.handle_audio_stopped();
    // tracing::info!(?note, "encounter stopped");

    // note.handle_trigger_mdm();
    // tracing::info!(?note, "MDM triggered");

    // note.handle_review_ready();
    // tracing::info!(?note, "review ready");

    // note.handle_sign();
    // tracing::info!(?note, "note signed");

    // // Verify serialization round-trip with hydrate.
    // let json = serde_json::to_string(&note).expect("serialize");
    // let mut restored: PatientNote = serde_json::from_str(&json).expect("deserialize");
    // restored.hydrate();
    // tracing::info!(?restored, "deserialized + hydrated");

    // // Put it in the store.
    // let store = state::new_store();
    // store.insert(patient.id, note);

    // tracing::info!(
    //     patient_count = store.len(),
    //     "note store ready"
    // );

    // tracing::info!("state pattern operational — all transitions verified");

   




}

fn count_tables(conn: &rusqlite::Connection) -> usize {
    conn.prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='table'")
        .and_then(|mut stmt| stmt.query_row([], |row| row.get::<_, usize>(0)))
        .unwrap_or(0)
}
