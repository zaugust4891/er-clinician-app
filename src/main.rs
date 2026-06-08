use er_note_app::audio::MockAudioCapture;
use er_note_app::db;
use er_note_app::encounter_driver::EncounterDriver;
use er_note_app::error::{AppError, AppResult};
use er_note_app::events::EventBus;
use er_note_app::models::{PatientId, PatientRecord, Shift};
use er_note_app::repository::{Repository, SqliteRepository};
use er_note_app::shift_manager::ShiftManager;
use er_note_app::state::{self, PatientNoteStore, load_patient_note};
use er_note_app::stt::MockTranscriber;
use er_note_app::persistence::ShiftSnapshot;
use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;
use std::sync::Arc;
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

    let (snapshot, shift_was_resumed): (ShiftSnapshot, bool) = match deserialize_shift_json_file("./shift.json") {
        Ok(s) => (s, true),
        Err(AppError::Io(e)) if e.kind() == io::ErrorKind::NotFound => {
            tracing::info!("no prior shift, starting fresh");
            (ShiftSnapshot { shift: Shift::new(), patients: Vec::new() }, false)
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

    let manager = ShiftManager::new(
        shift,
        patients,
        encounter_driver,
        event_bus.clone(),
        store.clone(),
    );
    let reloaded_patient_count = manager.list_patients().len();

// Welcome banner 
    println!("======== WELCOME to Gegonos ========");
    println!("Version: {}", env!("CARGO_PKG_VERSION"));
    println!("Database: {}", sql_lite_db_path);
    println!("Shift file: ./shift.json");
    println!("Status: {}", if shift_was_resumed { "Resumed shift" } else { "Fresh shift" });
    println!("Patients reloaded: {}", reloaded_patient_count);
    println!();
    println!("Available commands:");
    println!("  new <chief complaint>");
    println!("  list");
    println!("  switch <index>");
    println!("  start <ambient|dictation>");
    println!("  stop");
    println!("  lab <name> <value> <unit>");
    println!("  mdm");
    println!("  review");
    println!("  sign");
    println!("  note");
    println!("  export");
    println!("  reload");
    println!("  status");
    println!("  quit");


/*


###### A8. Enter the command loop
Loop forever (until the user types `quit`):
1. Print a prompt — something like `> ` or `[patient name]> ` if an active patient is set.
2. Read one line from stdin using `std::io::stdin().read_line(...)`.
3. If EOF (Ctrl-D), treat it like `quit`.
4. Trim whitespace.
5. If the line is empty, loop back.
6. Parse the line into a `Command` enum (see section B).
7. Dispatch to the appropriate handler.
8. Print the handler's result — either a success message or the formatted error.


*/






}

fn count_tables(conn: &rusqlite::Connection) -> usize {
    conn.prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='table'")
        .and_then(|mut stmt| stmt.query_row([], |row| row.get::<_, usize>(0)))
        .unwrap_or(0)
}
