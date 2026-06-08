use crate::models::CaptureMode;
use crate::error::{AppResult, AppError};
pub enum Command {
    New { chief_complaint: String },        // register a new patient on the shift
    List,                                    // list patients on the shift
    Switch { index: usize },                 // set active patient by list index
    Start { mode: CaptureMode },             // start_encounter on active patient
    Stop,                                    // stop_encounter
    Lab { name: String, value: String, unit: String },
    Mdm,
    Review,
    Sign,
    Note,                                    // print formatted note
    Export,
    Reload,                                  // rehydrate from repository
    Status,
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

            "mdm" => Command::Mdm,
            "review" => Command::Review,
            "sign" => Command::Sign,
            "note" => Command::Note,
            "export" => Command::Export,
            "reload" => Command::Reload,
            "status" => Command::Status,
            "quit" | "exit" => Command::Quit,

            _ => Command::Unknown(args.join(" ")),

        };

        Ok(Commands { commands: query })

    }
}
