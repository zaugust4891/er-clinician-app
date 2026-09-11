mod interface;

use dotenvy::dotenv;
use er_app::EventBus;
use er_app::encounter_driver::EncounterDriver;
use er_app::load::{self, PatientNoteStore, load_patient_note};
use er_app::shift_manager::ShiftManager;
use er_app::shift_snapshot::ShiftSnapshot;
use er_domain::{PatientId, PatientNote, PatientRecord, Shift};
use er_app::error::{AppError, AppResult};
use er_ai::claude::ClaudeSummarizer;
use er_ai::summarizer::{AppSummarizer, MockSummarizer};
use er_pipeline::audio::MockAudioCapture;
use er_store::db;
use er_store::repository::{Repository, SqliteRepository};
use er_pipeline::stt::MockTranscriber;
use crate::interface::cli::{Commands, RemoteContext, handle_command};
use er_api::client::ServerApi;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;
use std::sync::Arc;

// Helper with context to handle Ok or error for any fallible step. Generic
// over the error type so it accepts each layer's error (AppError, StoreError,
// AiError, …) directly — it only needs to Display the failure before exiting.
fn bail<T, E: std::fmt::Display>(context: &str, r: Result<T, E>) -> T {
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

/// Remote mode is on only when BOTH `ER_SERVER_URL` and `ER_TOKEN` are present
/// and non-blank. Half a configuration is a mistake, not a mode: silently
/// falling back to local SQLite there would write a shift to the wrong place
/// and look like it worked.
fn remote_config() -> Option<(String, String)> {
    remote_from(env::var("ER_SERVER_URL").ok(), env::var("ER_TOKEN").ok())
}

/// The decision itself, kept free of the environment so it can be tested.
fn remote_from(url: Option<String>, token: Option<String>) -> Option<(String, String)> {
    let non_blank = |v: Option<String>| v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    non_blank(url).zip(non_blank(token))
}

/// The server's shift view becomes the in-memory `Shift` the manager already
/// understands. In remote mode this replaces `shift.json` entirely — the shift
/// lives on the server, which is what lets a second device pick it up.
fn shift_from_view(view: &er_api::ShiftView) -> Shift {
    Shift {
        id: view.shift_id,
        started_at: view.started_at,
        ended_at: view.ended_at,
        patient_ids: view.patients.iter().map(|p| p.patient_id).collect(),
    }
}

/// The roster, as local `PatientRecord`s. The identity fields stay `None` on
/// purpose: they are not absent because the server withheld them, but because
/// they never left the device (§C1 — the wire types have nowhere to put a name).
fn patients_from_view(view: &er_api::ShiftView) -> HashMap<PatientId, PatientRecord> {
    view.patients
        .iter()
        .map(|p| {
            (
                p.patient_id,
                PatientRecord {
                    id: p.patient_id,
                    first_name: None,
                    last_name: None,
                    age: None,
                    chief_complaint: p.chief_complaint.clone(),
                    room: p.display_label.clone(),
                    created_at: p.created_at,
                },
            )
        })
        .collect()
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

    // The swap. Note what does NOT change below this line: EventBus,
    // ShiftManager, EncounterDriver and load_patient_note all take the same
    // `Arc<dyn Repository>` they always did. Remote mode is one match arm.
    let remote = remote_config();
    let sql_lite_db_path = "./er_notes.db";

    let repo: Arc<dyn Repository> = match &remote {
        Some((url, token)) => {
            tracing::info!(server = %url, "remote mode: the server is the system of record");
            Arc::new(er_api::client::RemoteRepository::new(
                url.clone(),
                token.clone(),
            ))
        }
        None => {
            let conn = bail("Failed to open database", db::open_db(sql_lite_db_path));
            tracing::info!("database initialized ({} tables)", count_tables(&conn));
            Arc::new(SqliteRepository::new(conn))
        }
    };

    // The non-storage half of the API: shifts and registration. `None` in local mode.
    let server: Option<ServerApi> = remote
        .as_ref()
        .map(|(url, token)| ServerApi::new(url.clone(), token.clone()));

    let event_bus = Arc::new(EventBus::new(1024, Some(repo.clone())));
    let store: PatientNoteStore = load::new_store();

    let audio = MockAudioCapture::new();
    let transcriber = MockTranscriber;

    let encounter_driver =
        EncounterDriver::new(audio, transcriber, event_bus.clone(), store.clone());

    // Boot. In remote mode the SERVER is the shift memory: `shift.json` is
    // neither read nor written, which is exactly what lets a different device
    // resume this shift. Local mode keeps the file; it dies when local mode does.
    let (shift, patients, shift_was_resumed): (Shift, HashMap<PatientId, PatientRecord>, bool) =
        match &server {
            Some(api) => {
                let (view, resumed) = match bail(
                    "asking the server for an active shift",
                    threaded_rt.block_on(api.active_shift()),
                ) {
                    Some(view) => (view, true), // resume — the dropped-phone moment
                    None => (
                        bail(
                            "starting a shift on the server",
                            threaded_rt.block_on(api.start_shift()),
                        ),
                        false, // first boot of the day
                    ),
                };
                (shift_from_view(&view), patients_from_view(&view), resumed)
            }
            None => {
                let (snapshot, resumed): (ShiftSnapshot, bool) =
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
                (shift, patients.into_iter().map(|p| (p.id, p)).collect(), resumed)
            }
        };

    // Unchanged, and in remote mode this IS the cloud reload: the same snapshot
    // fast-path and delta replay, answered over HTTP instead of by SQLite.
    let mut notes_reloaded_count = 0;
    for (id, _) in &patients {
        match threaded_rt.block_on(load_patient_note(&*repo, *id)) {
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

    let generation_is_remote = server.is_some();
    let mut manager = ShiftManager::new(
        shift,
        patients,
        encounter_driver,
        event_bus.clone(),
        store.clone(),
        repo.clone(),
        summarizer,
        threaded_rt.clone(),
        generation_is_remote,
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
    match &remote {
        Some((url, _)) => {
            println!("Mode: REMOTE — {url}");
            println!("Storage: server (no local database, no shift file)");
            println!("Shift: {}", manager.shift_id());
        }
        None => {
            println!("Mode: LOCAL");
            println!("Database: {}", sql_lite_db_path);
            println!("Shift file: ./shift.json");
        }
    }
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

                let remote_ctx = server.as_ref().map(|api| RemoteContext {
                    api,
                    runtime: &threaded_rt,
                });

                match Commands::new(&args) {
                    Ok(command_wrapper) => {
                        match handle_command(&mut manager, command_wrapper, remote_ctx.as_ref()) {
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

    // Remote mode deliberately writes nothing on the way out. The shift stays
    // OPEN on the server — that is the whole demo: quitting here is "dropping the
    // phone", and the next device to boot with this token resumes it. Ending the
    // shift is an explicit act (POST /v1/shifts/{id}/end), not a side effect of
    // closing a terminal.
    if remote.is_none() {
        let snapshot = manager.snapshot();
        serialize_shift_json_file("./shift.json", &snapshot)?;
        tracing::info!("shift snapshot saved");
    }

    drop(manager);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    fn view(patients: Vec<er_api::PatientView>) -> er_api::ShiftView {
        er_api::ShiftView {
            shift_id: Uuid::now_v7(),
            started_at: Utc.with_ymd_and_hms(2026, 8, 13, 7, 0, 0).unwrap(),
            ended_at: None,
            status: "active".to_string(),
            patients,
        }
    }

    fn patient(chief_complaint: &str, room: Option<&str>) -> er_api::PatientView {
        er_api::PatientView {
            patient_id: Uuid::now_v7(),
            chief_complaint: chief_complaint.to_string(),
            display_label: room.map(str::to_string),
            created_at: Utc.with_ymd_and_hms(2026, 8, 13, 7, 30, 0).unwrap(),
        }
    }

    /// Half a configuration is a mistake, not local mode: booting into SQLite
    /// with ER_SERVER_URL set would silently write the shift to the wrong place.
    #[test]
    fn remote_mode_needs_both_halves_and_ignores_blanks() {
        let some = |s: &str| Some(s.to_string());

        assert_eq!(
            remote_from(some("http://localhost:8080"), some("jwt")),
            Some(("http://localhost:8080".to_string(), "jwt".to_string()))
        );
        assert_eq!(remote_from(some("http://localhost:8080"), None), None);
        assert_eq!(remote_from(None, some("jwt")), None);
        assert_eq!(remote_from(some("  "), some("jwt")), None, "a blank URL is not a URL");
        assert_eq!(remote_from(some("http://x"), some("")), None, "an empty token is not a token");
        assert_eq!(
            remote_from(some(" http://x \n"), some(" jwt ")),
            Some(("http://x".to_string(), "jwt".to_string())),
            "shell copy-paste whitespace must not become part of the URL or token"
        );
    }

    /// The resume path: the server's view has to reconstitute the same `Shift`
    /// the manager would have read from shift.json, roster order included —
    /// `list`'s indices are position-based, so a reordering here would make
    /// `switch 2` select a different patient on the second device.
    #[test]
    fn shift_and_roster_survive_the_trip_from_the_server() {
        let v = view(vec![
            patient("chest pain", Some("Room 4")),
            patient("ankle injury", None),
        ]);

        let shift = shift_from_view(&v);
        assert_eq!(shift.id, v.shift_id);
        assert_eq!(shift.started_at, v.started_at);
        assert_eq!(shift.ended_at, None);
        assert_eq!(
            shift.patient_ids,
            vec![v.patients[0].patient_id, v.patients[1].patient_id],
            "roster order is the server's position order"
        );

        let patients = patients_from_view(&v);
        assert_eq!(patients.len(), 2);
        let first = &patients[&v.patients[0].patient_id];
        assert_eq!(first.chief_complaint, "chest pain");
        assert_eq!(first.room.as_deref(), Some("Room 4"));
        assert_eq!(first.created_at, v.patients[0].created_at);
        // Not withheld — never sent. The wire types have no field for a name (§C1).
        assert!(first.first_name.is_none() && first.last_name.is_none() && first.age.is_none());
    }

    /// First boot of the day: an empty shift is still a usable shift.
    #[test]
    fn an_empty_shift_view_yields_an_empty_roster() {
        let v = view(Vec::new());
        assert!(shift_from_view(&v).patient_ids.is_empty());
        assert!(patients_from_view(&v).is_empty());
    }
}
