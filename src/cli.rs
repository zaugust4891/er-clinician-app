use crate::models::CaptureMode;

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
