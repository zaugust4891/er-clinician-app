use dotenvy::dotenv;
use er_note_app::application::EventBus;
use er_note_app::application::encounter_driver::EncounterDriver;
use er_note_app::application::load::{self, PatientNoteStore, load_patient_note};
use er_note_app::application::shift_manager::ShiftManager;
use er_note_app::application::shift_snapshot::ShiftSnapshot;
use er_note_app::domain::{PatientId, PatientNote, PatientRecord, Shift};
use er_note_app::error::{AppError, AppResult};
use er_note_app::infrastructure::ai::claude::ClaudeSummarizer;
use er_note_app::infrastructure::ai::summarizer::{AppSummarizer, MockSummarizer};
use er_note_app::infrastructure::audio::MockAudioCapture;
use er_note_app::infrastructure::persistence::db;
use er_note_app::infrastructure::persistence::repository::{Repository, SqliteRepository};
use er_note_app::infrastructure::stt::MockTranscriber;
use er_note_app::interface::cli::{Commands, handle_command};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;
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

fn deserialize_shift_json_file<P: AsRef<Path>, S: DeserializeOwned>(path: P) -> AppResult<S> {
    let file = File::open(path)?;
    let rdr = BufReader::new(file);
    let value = serde_json::from_reader(rdr)?;
    Ok(value)
}

fn serialize_shift_json_file<P: AsRef<Path>, S: Serialize>(path: P, value: &S) -> AppResult<()> {
    let file = File::create(path)?;
    serde_json::to_writer_pretty(file, value)?;
    Ok(())
}

fn count_tables(conn: &rusqlite::Connection) -> usize {
    conn.prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='table'")
        .and_then(|mut stmt| stmt.query_row([], |row| row.get::<_, usize>(0)))
        .unwrap_or(0)
}

fn main() -> AppResult<()> {
    tracing_subscriber::fmt::init();

    tracing::info!("ED Note App starting");

    dotenv().ok();
    let threaded_rt = Arc::new(tokio::runtime::Runtime::new()?);

    let summarizer = if env::var("ANTHROPIC_KEY").is_ok() {
        AppSummarizer::Claude(bail(
            "Failed to create ClaudeSummarizer",
            ClaudeSummarizer::new(),
        ))
    } else {
        AppSummarizer::Mock(MockSummarizer::new())
    };

    // Initialize in-memory database for demo.
    let sql_lite_db_path = "./er_notes.db";
    let conn = bail("Failed to open database", db::open_db(sql_lite_db_path));
    tracing::info!("database initialized ({} tables)", count_tables(&conn));

    let sql_lite_repo: Arc<dyn Repository> = Arc::new(SqliteRepository::new(conn));

    let event_bus = Arc::new(EventBus::new(1024, Some(sql_lite_repo.clone())));
    let store: PatientNoteStore = load::new_store();

    let audio = MockAudioCapture::new();
    let transcriber = MockTranscriber;

    let encounter_driver =
        EncounterDriver::new(audio, transcriber, event_bus.clone(), store.clone());

    let (snapshot, shift_was_resumed): (ShiftSnapshot, bool) =
        match deserialize_shift_json_file("./shift.json") {
            Ok(s) => (s, true),
            Err(AppError::Io(e)) if e.kind() == io::ErrorKind::NotFound => {
                tracing::info!("no prior shift, starting fresh");
                (
                    ShiftSnapshot {
                        shift: Shift::new(),
                        patients: Vec::new(),
                    },
                    false,
                )
            }
            Err(e) => bail("loading shift.json", Err(e)),
        };
    let ShiftSnapshot { shift, patients } = snapshot;
    let patients: HashMap<PatientId, PatientRecord> =
        patients.into_iter().map(|p| (p.id, p)).collect();

    let mut notes_reloaded_count = 0;
    for (id, _) in &patients {
        match load_patient_note(&*sql_lite_repo, *id) {
            Ok(Some(note)) => {
                store.insert(*id, note);
                notes_reloaded_count += 1;
            }
            Ok(None) => {
                tracing::info!(
                    patient_id = %id,
                    "patient record has no persisted events yet; restoring as registered"
                );
                store.insert(*id, PatientNote::new(*id));
            }
            Err(e) => bail("loading patient note", Err(e)),
        }
    }

    let mut manager = ShiftManager::new(
        shift,
        patients,
        encounter_driver,
        event_bus.clone(),
        store.clone(),
        sql_lite_repo.clone(),
        summarizer,
        threaded_rt.clone(),
    );
    let patient_record_count = manager.list_patients().len();

    // Welcome banner
    println!("======== WELCOME to Gegonos CLI ========");
    println!();
    println!(
        "The purpose of this CLI is for developers to test current and new backend Rust features and their functionality."
    );
    println!(
        "To ensure smooth, efficient and robust development ALL NEW FEATURES and UPDATED FUNCTIONALITY should be tested using their corresponding command BEFORE being wired to the front end to ensure they work as intended."
    );
    println!();
    println!("Version: {}", env!("CARGO_PKG_VERSION"));
    println!("Database: {}", sql_lite_db_path);
    println!("Shift file: ./shift.json");
    println!(
        "Status: {}",
        if shift_was_resumed {
            "Resumed shift"
        } else {
            "Fresh shift"
        }
    );
    println!("Patient records loaded: {}", patient_record_count);
    println!("Notes reloaded from DB: {}", notes_reloaded_count);
    println!();
    Commands::print_help();
    println!();

    loop {
        Commands::print_prompt();

        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(0) => {
                // EOF (Ctrl-D)
                println!();
                println!("EOF received. Goodbye!");
                break;
            }
            Ok(_) => {
                //Normal input
                let input = input.trim();

                if input.is_empty() {
                    continue;
                }

                let args: Vec<String> = input.split_whitespace().map(|s| s.to_string()).collect();

                match Commands::new(&args) {
                    Ok(command_wrapper) => {
                        match handle_command(&mut manager, command_wrapper) {
                            Ok(true) => {}      // continue
                            Ok(false) => break, // Quit command
                            Err(e) => eprintln!("Error: {}", e),
                        }
                    }
                    Err(e) => {
                        eprintln!("Parse error: {}", e);
                    }
                }
            }
            Err(e) => {
                // read_line error
                eprintln!("Error reading input: {}", e);
                // Continue the loop
                continue;
            }
        }
    }

    if let Err(e) = manager.stop_active_encounter_for_shutdown() {
        tracing::warn!(error = %e, "failed to stop active encounter during shutdown");
    }

    let snapshot = manager.snapshot();
    serialize_shift_json_file("./shift.json", &snapshot)?;
    tracing::info!("shift snapshot saved");

    drop(manager);

    Ok(())
}
