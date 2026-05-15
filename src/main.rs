use er_note_app::audio::MockAudioCapture;
use er_note_app::db;
use er_note_app::encounter_driver::EncounterDriver;
use er_note_app::error::{AppError, AppResult};
use er_note_app::events::EventBus;
use er_note_app::models::{PatientNote, PatientRecord, Shift};
use er_note_app::repository::{Repository, SqliteRepository};
use er_note_app::state::{self, PatientNoteStore};
use er_note_app::stt::MockTranscriber;
use std::path::PathBuf;
use std::{env, path};
use std::sync::Arc;

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
