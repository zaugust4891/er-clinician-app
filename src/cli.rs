
use std::io::Write;

use uuid::Uuid;

use crate::audio::AudioCapture;
use crate::conversions::disposition::{format_disposition_for_cli, parse_disposition};
use crate::error::AppError::NoActivePatient;
use crate::domain::{CaptureMode, ClinicalResult, Disposition, LabResult};
use crate::error::{AppResult, AppError};
use crate::shift_manager::ShiftManager;
use crate::stt::Transcriber;
use crate::summarizer::Summarizer;
use crate::conversions::labs::{parse_string_to_f64};

use chrono::Utc;
pub enum Command {
    New { chief_complaint: String },        // register a new patient on the shift
    List,                                    // list patients on the shift
    Switch { index: usize },                 // set active patient by list index
    Start { mode: CaptureMode },             // start_encounter on active patient
    Stop,                                    // stop_encounter
    Lab { name: String, value: String, unit: String },
    Dispo { disposition: Disposition },
    Mdm,
    Review,
    Sign,
    Note,                                    // print formatted note
    Export,
    Reload,                                  // rehydrate from repository
    Status,
    Help,
    Quit,
    Unknown(String),                         // raw line for error reporting
}

pub struct Commands {
    pub commands: Command,
}

impl Commands {
    pub fn new(args: &[String]) -> AppResult<Commands> {
        if args.is_empty() {
            return Err(AppError::CliError("Not enough arguments. Please enter one of the following arguments".to_string()
            ));
        }

        let command_str = &args[0].to_lowercase();
        let query = match command_str.as_str() {
            "new" => {
                if args.len() < 2 {
                    return Err(AppError::CliError(
                        "Usage: new <chief_complaint>".to_string()
                    ));
                }
                Command::New {
                    chief_complaint: args[1..].join(" "), // everything after "new" is the complaint
                }

            }

            "list" => Command::List,

            "switch" => {
                if args.len() < 2 {
                    return Err(AppError::CliError("Usage: switch <index>".to_string()
                    ));
                }
                let index: usize = args[1].parse().map_err(|_| {
                    AppError::CliError("Index must be a number".to_string())
                })?;
                Command::Switch { index }
            }

            "start" => {
                if args.len() < 2 {
                    return Err(AppError::CliError("Usage: start <mode>".to_string()
                    ));
                }
                let mode = match args[1].to_lowercase().as_str() {
                    "ambient" => CaptureMode::Ambient,
                    "dictation" => CaptureMode::Dictation,
                    _ => {
                        return Err(AppError::CliError(format!("Unknown mode: {}", args[1])));
                    }
                };
                Command::Start { mode }
            }

            "stop" => Command::Stop,

            "lab" => {
                if args.len() < 4 {
                    return Err(AppError::CliError("Usage: lab <name> <value> <unit>".to_string()
                    ));
                }
                Command::Lab {
                    name: args[1].clone(),
                    value: args[2].clone(),
                    unit: args[3].clone(),
                }
            }

            "dispo" => Command::Dispo {
                disposition: parse_disposition(args)?,
            },

            "mdm" => Command::Mdm,
            "review" => Command::Review,
            "sign" => Command::Sign,
            "note" => Command::Note,
            "export" => Command::Export,
            "reload" => Command::Reload,
            "status" => Command::Status,
            "help" => Command::Help,
            "quit" | "exit" => Command::Quit,

            _ => Command::Unknown(args.join(" ")),

        };

        Ok(Commands { commands: query })

    }

    pub fn print_prompt() {
        print!("> ");
        std::io::stdout().flush().unwrap();
    }

    pub fn print_help() {
        println!("Available commands:");
        println!("  new <chief complaint>");
        println!("  list");
        println!("  switch <index>");
        println!("  start <ambient|dictation>");
        println!("  stop");
        println!("  lab <name> <value> <unit>");
        println!("  dispo discharge <instructions> [<follow-up>]");
        println!("  dispo admit <service> [<bed>]");
        println!("  dispo transfer <destination> [<reason>]");
        println!("  dispo ama");
        println!("  dispo lwbs");
        println!("  mdm");
        println!("  review");
        println!("  sign");
        println!("  note");
        println!("  export");
        println!("  reload");
        println!("  status");
        println!("  help");
        println!("  quit");
    }
}

pub fn handle_command<A, T, S>(
    manager: &mut ShiftManager<A, T, S>,
    command_wrapper: Commands,
) -> AppResult<bool>
where
    A: AudioCapture,
    T: Transcriber,
    S: Summarizer,
{
    match command_wrapper.commands {
        Command::New { chief_complaint } => {
            let patient_id = manager.register_patient(&chief_complaint);
            println!(
                "Registered patient {} with chief complaint '{}'",
                patient_id,
                chief_complaint
            );
            println!("Tip: type 'list' to see patient indices, then 'switch <index>'.");
            Ok(true)
        }

        Command::List => {
            let patients = manager.list_patients();

            if patients.is_empty() {
                println!("No patients registered. Use 'new <chief complaint>' to register one.");
                return Ok(true);
            }

            for (index, (patient_id, chief_complaint, phase)) in patients.iter().enumerate() {
                println!(
                    "{}. {} [{:?}] ({})",
                    index + 1,
                    chief_complaint,
                    phase,
                    short_id(*patient_id)
                );
            }

            Ok(true)
        }

        Command::Switch { index } => {
            if index == 0 {
                return Err(AppError::CliError(
                    "Use a 1-based patient index from the list command.".to_string(),
                ));
            }

            let patients = manager.list_patients();
            let Some((patient_id, chief_complaint, _phase)) = patients.get(index - 1) else {
                return Err(AppError::CliError(format!(
                    "No patient at index {index}. Use 'list' to see valid indices."
                )));
            };

            manager.set_active_patient(*patient_id)?;
            println!(
                "Active patient: {} ({})",
                chief_complaint,
                short_id(*patient_id)
            );

            Ok(true)
        }

        Command::Start { mode } => {
            let mode_label = format!("{:?}", mode);
            let encounter_id = manager.start_encounter(mode)?;
            println!(
                "Started {} encounter {}.",
                mode_label,
                short_id(encounter_id)
            );
            Ok(true)
        }

        Command::Stop => {
            let encounter_id = manager.stop_encounter()?;
            println!(
                "Stopped encounter {}. Transcript captured.",
                short_id(encounter_id)
            );
            Ok(true)
        }

        Command::Lab { name, value, unit } => {
            let patient_id = manager
                .active_patient_id()
                .ok_or(NoActivePatient(
                    "No active patient found".into(),
                ))?;

            let value: f64 = parse_string_to_f64(value)?;
            
            let result = ClinicalResult {
                id: Uuid::now_v7(),
                patient_id,
                source: crate::domain::ResultSource::Manual,
                timestamp: Utc::now(),
                result_type: crate::domain::ResultType::Lab(LabResult{
                    fhir_id: None,
                    loinc_code: None,
                    display_name: name.clone(),
                    value,
                    unit: unit.clone(),
                    reference_range: None,
                    is_abnormal: false
                }),
            };


            manager.add_lab(result, Utc::now())?;
            println!("Added lab: {name} = {value} {unit}");
            
            Ok(true)
        }

        Command::Dispo { disposition } => {
            let label = format_disposition_for_cli(&disposition);
            manager.set_disposition(disposition)?;
            println!("Disposition set: {label}");
            Ok(true)
        }

        Command::Mdm => {
            manager.trigger_mdm()?;
            println!("MDM triggered.");
            Ok(true)
        }

        Command::Review => {
            manager.review_ready()?;
            println!("Note marked ready for review.");
            Ok(true)
        }

        Command::Sign => {
            manager.sign()?;
            println!("Note signed.");
            Ok(true)
        }

        Command::Note => {
            let patient_id = manager
                .active_patient_id()
                .ok_or(NoActivePatient("No active patient found".into()))?;
            
            let note = manager
                .get_note(patient_id)
                .ok_or(AppError::NoteError("No note found".into()))?;

            println!("{note}");
            Ok(true)
        }

        Command::Export => {
            let exported = manager.export_active_note()?;
            println!("{exported}");
            Ok(true)
        }

        Command::Reload => {
            let replayed_count = manager.reload_active_patient()?;
            println!("Reloaded active patient from SQLite. Replayed {replayed_count} events.");
            Ok(true)
        }

        Command::Status => {
            let patient_count = manager.list_patients().len();
            let active_patient = manager
                .active_patient_id()
                .map(|id| short_id(id))
                .unwrap_or_else(|| "none".to_string());
            let event_count = manager.event_count()?;
            let snapshot_count = manager.snapshot_count()?;

            println!("Shift ID: {}", manager.shift_id());
            println!("Patients registered: {patient_count}");
            println!("Active patient: {active_patient}");
            println!("Active encounter: {}", manager.has_active_encounter());
            println!("Events stored: {event_count}");
            println!("Snapshots stored: {snapshot_count}");
            Ok(true)
        }

        Command::Help => {
            Commands::print_help();
            Ok(true)
        }

        Command::Quit => {
            println!("Goodbye.");
            Ok(false)
        }

        Command::Unknown(line) => Err(AppError::CliError(format!(
            "Unknown command: '{}'. Type 'help' to see commands.",
            line
        ))),
    }
}

fn short_id(id: uuid::Uuid) -> String {
    let id = id.to_string();
    id[id.len() - 8..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    use crate::audio::MockAudioCapture;
    use crate::encounter_driver::EncounterDriver;
    use crate::events::EventBus;
    use crate::domain::{PatientId, PatientRecord, Shift};
    use crate::repository::{Repository, SqliteRepository};
    use crate::state;
    use crate::stt::MockTranscriber;
    use crate::summarizer::MockSummarizer;

    fn test_manager() -> ShiftManager<MockAudioCapture, MockTranscriber, MockSummarizer> {
        let conn = crate::db::open_memory_db().unwrap();
        let repo: Arc<dyn Repository> = Arc::new(SqliteRepository::new(conn));
        let bus = Arc::new(EventBus::new(32, Some(repo.clone())));
        let store = state::new_store();
        let driver = EncounterDriver::new(
            MockAudioCapture::new(),
            MockTranscriber,
            bus.clone(),
            store.clone(),
        );
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());

        ShiftManager::new(
            Shift::new(),
            HashMap::<PatientId, PatientRecord>::new(),
            driver,
            bus,
            store,
            repo,
            MockSummarizer::new(),
            runtime,
        )
    }

    #[test]
    fn dispo_discharge_command_sets_note_disposition() {
        let mut manager = test_manager();
        let patient_id = manager.register_patient("chest pain");
        manager.set_active_patient(patient_id).unwrap();

        let args = vec![
            "dispo".to_string(),
            "discharge".to_string(),
            "\"ibuprofen".to_string(),
            "as".to_string(),
            "needed\"".to_string(),
            "\"Cardiology".to_string(),
            "1wk\"".to_string(),
        ];
        let command = Commands::new(&args).unwrap();

        assert!(handle_command(&mut manager, command).unwrap());

        let note = manager.get_note(patient_id).unwrap();
        assert_eq!(
            note.disposition,
            Some(Disposition::Discharge {
                instructions: "ibuprofen as needed".to_string(),
                follow_up: Some("Cardiology 1wk".to_string()),
            })
        );
    }
}
