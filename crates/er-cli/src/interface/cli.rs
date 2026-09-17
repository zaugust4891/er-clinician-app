use std::{
    future::Future,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

use uuid::Uuid;

use er_ai::summarizer::Summarizer;
use er_api::client::ServerApi;
use er_app::conversions::diagnosis::parse_diagnosis;
use er_app::conversions::disposition::{format_disposition_for_cli, parse_disposition};
use er_app::conversions::labs::parse_string_to_f64;
use er_app::conversions::medication::parse_medication;
use er_app::error::AppError::NoActivePatient;
use er_app::error::{AppError, AppResult};
use er_app::shift_manager::ShiftManager;
use er_domain::{CaptureMode, ClinicalResult, Disposition, LabResult, MedicationKind};
use er_pipeline::audio::AudioCapture;
use er_pipeline::stt::Transcriber;
use er_store::{StoreError, StoreResult};
use tokio::runtime::Runtime;

use chrono::Utc;
pub enum Command {
    New {
        chief_complaint: String,
    }, // register a new patient on the shift
    List, // list patients on the shift
    Switch {
        index: usize,
    }, // set active patient by list index
    Start {
        mode: CaptureMode,
    }, // start_encounter on active patient
    Stop, // stop_encounter
    Audio {
        path: PathBuf,
    }, // upload one final chunk for the active encounter (remote mode)
    Lab {
        name: String,
        value: String,
        unit: String,
    },
    Dispo {
        disposition: Disposition,
    },
    Med {
        kind: MedicationKind,
        name: String,
        dose: String,
        route: Option<String>,
    },
    Dx {
        text: String,
        icd10_code: Option<String>,
        is_primary: bool,
    },
    Data {
        physician_review: String,
    },
    Mdm,
    Review,
    Sign,
    Note,       // print formatted note
    Transcript, // print raw encounter transcripts
    Export,
    Reload, // rehydrate from repository
    Status,
    Help,
    Quit,
    Unknown(String), // raw line for error reporting
}

pub struct Commands {
    pub commands: Command,
}

impl Commands {
    pub fn new(args: &[String]) -> AppResult<Commands> {
        if args.is_empty() {
            return Err(AppError::CliError(
                "Not enough arguments. Please enter one of the following arguments".to_string(),
            ));
        }

        let command_str = &args[0].to_lowercase();
        let query = match command_str.as_str() {
            "new" => {
                if args.len() < 2 {
                    return Err(AppError::CliError(
                        "Usage: new <chief_complaint>".to_string(),
                    ));
                }
                Command::New {
                    chief_complaint: args[1..].join(" "), // everything after "new" is the complaint
                }
            }

            "list" => Command::List,

            "switch" => {
                if args.len() < 2 {
                    return Err(AppError::CliError("Usage: switch <index>".to_string()));
                }
                let index: usize = args[1]
                    .parse()
                    .map_err(|_| AppError::CliError("Index must be a number".to_string()))?;
                Command::Switch { index }
            }

            "start" => {
                if args.len() < 2 {
                    return Err(AppError::CliError("Usage: start <mode>".to_string()));
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

            "audio" => {
                if args.len() < 2 {
                    return Err(AppError::CliError("Usage: audio <path>".to_string()));
                }
                Command::Audio {
                    path: PathBuf::from(args[1..].join(" ")),
                }
            }

            "lab" => {
                if args.len() < 4 {
                    return Err(AppError::CliError(
                        "Usage: lab <name> <value> <unit>".to_string(),
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

            "med" => {
                let (kind, name, dose, route) = parse_medication(args)?;
                Command::Med {
                    kind,
                    name,
                    dose,
                    route,
                }
            }

            "dx" => {
                let (text, icd10_code, is_primary) = parse_diagnosis(args)?;
                Command::Dx {
                    text,
                    icd10_code,
                    is_primary,
                }
            }

            "data" => {
                let physician_review = args[1..].join(" ").trim_matches('"').to_string();
                if physician_review.is_empty() {
                    return Err(AppError::CliError("Usage: data <free text>".to_string()));
                }
                Command::Data { physician_review }
            }

            "mdm" => Command::Mdm,
            "review" => Command::Review,
            "sign" => Command::Sign,
            "note" => Command::Note,
            "transcript" => Command::Transcript,
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
        println!("  audio <path.wav|path.ogg|path.m4a|path.mp3>");
        println!("  lab <name> <value> <unit>");
        println!("  dispo discharge <instructions> [<follow-up>]");
        println!("  dispo admit <service> [<bed>]");
        println!("  dispo transfer <destination> [<reason>]");
        println!("  dispo ama");
        println!("  dispo lwbs");
        println!("  med home <name> [dose] [route]");
        println!("  med given <name> [dose] [route]");
        println!("  dx <text> [--primary] [icd10]");
        println!("  data <free text>");
        println!("  mdm");
        println!("  review");
        println!("  sign");
        println!("  note");
        println!("  transcript");
        println!("  export");
        println!("  reload");
        println!("  status");
        println!("  help");
        println!("  quit");
    }
}

#[derive(Clone, Copy)]
struct ObservationPolicy {
    budget: Duration,
    poll_delay: Duration,
    request_timeout: Duration,
}

impl ObservationPolicy {
    const CLI: Self = Self {
        budget: Duration::from_secs(30),
        poll_delay: Duration::from_secs(2),
        request_timeout: Duration::from_secs(5),
    };
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ObservationUnavailableReason {
    Authentication,
    NotFound,
    TemporarilyUnavailable,
    RequestTimedOut,
    InvalidResponse,
    Rejected(u16),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ObservationOutcome {
    Done,
    Failed,
    PendingAtDeadline {
        last_status: String,
    },
    ObservationUnavailable {
        reason: ObservationUnavailableReason,
        last_status: String,
    },
}

/// The narrow read seam used by the polling policy.
///
/// Keeping the helper generic lets tests supply deterministic responses and a
/// paused Tokio clock. Production implements the same seam with ServerApi and
/// therefore reuses its one reqwest connection pool.
trait ChunkStatusSource {
    fn audio_chunk(
        &self,
        chunk_id: Uuid,
    ) -> impl Future<Output = StoreResult<er_api::ChunkView>> + Send;
}

impl ChunkStatusSource for ServerApi {
    fn audio_chunk(
        &self,
        chunk_id: Uuid,
    ) -> impl Future<Output = StoreResult<er_api::ChunkView>> + Send {
        ServerApi::audio_chunk(self, chunk_id)
    }
}

fn terminal_outcome(status: &str) -> Option<ObservationOutcome> {
    if !er_api::chunk_status::is_terminal(status) {
        return None;
    }

    Some(if status == er_api::chunk_status::DONE {
        ObservationOutcome::Done
    } else {
        ObservationOutcome::Failed
    })
}

fn unavailable_reason(error: &StoreError) -> ObservationUnavailableReason {
    match error {
        StoreError::Remote { status, .. } => match *status {
            401 | 403 => ObservationUnavailableReason::Authentication,
            404 => ObservationUnavailableReason::NotFound,
            429 | 500..=599 => ObservationUnavailableReason::TemporarilyUnavailable,
            status => ObservationUnavailableReason::Rejected(status),
        },
        StoreError::Backend(_)
        | StoreError::Db(_)
        | StoreError::Serialization(_)
        | StoreError::Parse { .. } => ObservationUnavailableReason::TemporarilyUnavailable,
    }
}

/// Observe one completed chunk without changing the server-side job.
///
/// The completion response is observation zero. Every later read begins after
/// the previous response plus poll_delay, so requests never overlap and a slow
/// response cannot cause an interval catch-up burst. Both the delay and each
/// request are clipped to one monotonic overall deadline.
async fn observe_audio_chunk<C, F>(
    source: &C,
    chunk_id: Uuid,
    initial: er_api::ChunkView,
    policy: ObservationPolicy,
    mut status_changed: F,
) -> ObservationOutcome
where
    C: ChunkStatusSource,
    F: FnMut(&str),
{
    let deadline = tokio::time::Instant::now() + policy.budget;
    let mut last_status = initial.status;
    status_changed(&last_status);

    if initial.chunk_id != chunk_id {
        return ObservationOutcome::ObservationUnavailable {
            reason: ObservationUnavailableReason::InvalidResponse,
            last_status,
        };
    }
    if let Some(outcome) = terminal_outcome(&last_status) {
        return outcome;
    }

    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return ObservationOutcome::PendingAtDeadline { last_status };
        }

        tokio::time::sleep_until(std::cmp::min(now + policy.poll_delay, deadline)).await;
        if tokio::time::Instant::now() >= deadline {
            return ObservationOutcome::PendingAtDeadline { last_status };
        }

        let request_deadline = std::cmp::min(
            tokio::time::Instant::now() + policy.request_timeout,
            deadline,
        );
        let observed =
            tokio::time::timeout_at(request_deadline, source.audio_chunk(chunk_id)).await;

        let view = match observed {
            Ok(Ok(view)) => view,
            Ok(Err(error)) => {
                return ObservationOutcome::ObservationUnavailable {
                    reason: unavailable_reason(&error),
                    last_status,
                };
            }
            Err(_) if tokio::time::Instant::now() >= deadline => {
                return ObservationOutcome::PendingAtDeadline { last_status };
            }
            Err(_) => {
                return ObservationOutcome::ObservationUnavailable {
                    reason: ObservationUnavailableReason::RequestTimedOut,
                    last_status,
                };
            }
        };

        if view.chunk_id != chunk_id {
            return ObservationOutcome::ObservationUnavailable {
                reason: ObservationUnavailableReason::InvalidResponse,
                last_status,
            };
        }

        if view.status != last_status {
            last_status = view.status;
            status_changed(&last_status);
        }
        if let Some(outcome) = terminal_outcome(&last_status) {
            return outcome;
        }
    }
}

fn observation_message(outcome: &ObservationOutcome) -> String {
    match outcome {
        ObservationOutcome::Done => concat!(
            "Chunk transcription is done; encounter assembly may still be finishing. ",
            "Run 'reload', then 'transcript' to read any published transcript."
        )
        .to_string(),
        ObservationOutcome::Failed => {
            "Chunk transcription failed on the server; the failure is terminal.".to_string()
        }
        ObservationOutcome::PendingAtDeadline { last_status } => format!(
            "Observation ended after 30 seconds at status '{last_status}'; processing continues \
             independently. Run 'reload' later, then 'transcript'."
        ),
        ObservationOutcome::ObservationUnavailable {
            reason,
            last_status,
        } => {
            let category = match reason {
                ObservationUnavailableReason::Authentication => {
                    "authentication or access was rejected"
                }
                ObservationUnavailableReason::NotFound => "chunk status is unavailable",
                ObservationUnavailableReason::TemporarilyUnavailable => {
                    "status could not be checked; the server may still be processing"
                }
                ObservationUnavailableReason::RequestTimedOut => {
                    "the status request timed out; the server may still be processing"
                }
                ObservationUnavailableReason::InvalidResponse => {
                    "the status response did not identify the requested chunk"
                }
                ObservationUnavailableReason::Rejected(status) => {
                    return format!(
                        "Observation stopped at status '{last_status}': status read was rejected \
                         (HTTP {status}); processing may continue."
                    );
                }
            };
            format!("Observation stopped at status '{last_status}': {category}.")
        }
    }
}

trait JobStatusSource {
    fn job(&self, job_id: Uuid) -> impl Future<Output = StoreResult<er_api::JobView>> + Send;
}

impl JobStatusSource for ServerApi {
    fn job(&self, job_id: Uuid) -> impl Future<Output = StoreResult<er_api::JobView>> + Send {
        ServerApi::job(self, job_id)
    }
}

fn terminal_job_outcome(status: &str) -> Option<ObservationOutcome> {
    if !er_api::job_status::is_terminal(status) {
        return None;
    }
    Some(if status == er_api::job_status::DONE {
        ObservationOutcome::Done
    } else {
        ObservationOutcome::Failed
    })
}

async fn observe_mdm_job<C, F>(
    source: &C,
    job_id: Uuid,
    initial: er_api::MdmAccepted,
    policy: ObservationPolicy,
    mut status_changed: F,
) -> ObservationOutcome
where
    C: JobStatusSource,
    F: FnMut(&str),
{
    let deadline = tokio::time::Instant::now() + policy.budget;
    let mut last_status = initial.status;
    status_changed(&last_status);

    if initial.job_id != job_id {
        return ObservationOutcome::ObservationUnavailable {
            reason: ObservationUnavailableReason::InvalidResponse,
            last_status,
        };
    }
    if let Some(outcome) = terminal_job_outcome(&last_status) {
        return outcome;
    }

    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return ObservationOutcome::PendingAtDeadline { last_status };
        }
        tokio::time::sleep_until(std::cmp::min(now + policy.poll_delay, deadline)).await;
        if tokio::time::Instant::now() >= deadline {
            return ObservationOutcome::PendingAtDeadline { last_status };
        }

        let request_deadline = std::cmp::min(
            tokio::time::Instant::now() + policy.request_timeout,
            deadline,
        );
        let observed = tokio::time::timeout_at(request_deadline, source.job(job_id)).await;
        let view = match observed {
            Ok(Ok(view)) => view,
            Ok(Err(error)) => {
                return ObservationOutcome::ObservationUnavailable {
                    reason: unavailable_reason(&error),
                    last_status,
                };
            }
            Err(_) if tokio::time::Instant::now() >= deadline => {
                return ObservationOutcome::PendingAtDeadline { last_status };
            }
            Err(_) => {
                return ObservationOutcome::ObservationUnavailable {
                    reason: ObservationUnavailableReason::RequestTimedOut,
                    last_status,
                };
            }
        };

        if view.job_id != job_id {
            return ObservationOutcome::ObservationUnavailable {
                reason: ObservationUnavailableReason::InvalidResponse,
                last_status,
            };
        }
        if view.status != last_status {
            last_status = view.status;
            status_changed(&last_status);
        }
        if let Some(outcome) = terminal_job_outcome(&last_status) {
            return outcome;
        }
    }
}

fn mdm_observation_message(outcome: &ObservationOutcome) -> String {
    match outcome {
        ObservationOutcome::Done => {
            "MDM generation is done; the note was reloaded from the event stream.".to_string()
        }
        ObservationOutcome::Failed => {
            "MDM generation failed on the server; the job is terminal.".to_string()
        }
        ObservationOutcome::PendingAtDeadline { last_status } => format!(
            "MDM observation ended after 30 seconds at status '{last_status}'; generation \
             continues independently. Run 'reload' later."
        ),
        ObservationOutcome::ObservationUnavailable {
            reason,
            last_status,
        } => {
            let category = match reason {
                ObservationUnavailableReason::Authentication => "authentication was rejected",
                ObservationUnavailableReason::NotFound => "job status is unavailable",
                ObservationUnavailableReason::TemporarilyUnavailable => {
                    "status could not be checked; the server may still be processing"
                }
                ObservationUnavailableReason::RequestTimedOut => {
                    "the status request timed out; the server may still be processing"
                }
                ObservationUnavailableReason::InvalidResponse => {
                    "the server returned a mismatched job id"
                }
                ObservationUnavailableReason::Rejected(status) => {
                    return format!(
                        "MDM observation stopped after HTTP {status} at status '{last_status}'."
                    );
                }
            };
            format!("MDM observation stopped: {category} (last status '{last_status}').")
        }
    }
}

fn content_type_for_audio_path(path: &Path) -> AppResult<&'static str> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase);

    match extension.as_deref() {
        Some("wav") => Ok("audio/wav"),
        Some("ogg") => Ok("audio/ogg"),
        Some("m4a" | "mp4") => Ok("audio/mp4"),
        Some("mp3" | "mpeg") => Ok("audio/mpeg"),
        _ => Err(AppError::CliError(
            "Unsupported audio type. Use wav, ogg, m4a, mp4, mp3, or mpeg.".to_string(),
        )),
    }
}

fn safe_request_error(action: &str, error: &StoreError) -> AppError {
    let detail = match error {
        StoreError::Remote { status, .. } => format!("server returned HTTP {status}"),
        _ => "the service could not be reached or returned an invalid response".to_string(),
    };
    AppError::CliError(format!("{action} failed: {detail}"))
}

/// What a command needs to reach the server directly, in remote mode (S10).
///
/// Most commands never touch it: they go through `ShiftManager`, whose
/// `Arc<dyn Repository>` is already a `RemoteRepository`, and so cross the wire
/// without knowing it. This exists for the handful of operations that are *not*
/// Repository storage — patient registration and the direct-to-object-storage
/// audio protocol.
///
/// The runtime rides along because the REPL is synchronous and the client is
/// async; this is the same `block_on` bridge `ShiftManager` uses internally.
pub struct RemoteContext<'a> {
    pub api: &'a ServerApi,
    pub runtime: &'a Runtime,
}

pub fn handle_command<A, T, S>(
    manager: &mut ShiftManager<A, T, S>,
    command_wrapper: Commands,
    remote: Option<&RemoteContext<'_>>,
) -> AppResult<bool>
where
    A: AudioCapture,
    T: Transcriber,
    S: Summarizer,
{
    match command_wrapper.commands {
        Command::New { chief_complaint } => {
            let patient_id = match remote {
                // SERVER-FIRST on purpose. If registration fails — network down,
                // token expired — no local ghost patient is created whose every
                // later event-append would 404. Fail loudly at step one instead of
                // confusingly at step five. The server's insert is idempotent
                // (S3), so retrying after a timeout is always safe.
                Some(ctx) => {
                    let patient_id = uuid::Uuid::now_v7(); // client-stamped (D5.3)
                    ctx.runtime
                        .block_on(ctx.api.register_patient(&er_api::RegisterPatientRequest {
                            patient_id,
                            chief_complaint: chief_complaint.clone(),
                            display_label: None,
                        }))
                        .map_err(|e| {
                            AppError::CliError(format!(
                                "the server refused to register this patient ({e}); \
                                 nothing was created locally"
                            ))
                        })?;
                    manager.register_patient_with_id(patient_id, &chief_complaint)
                }
                None => manager.register_patient(&chief_complaint),
            };
            println!(
                "Registered patient {} with chief complaint '{}'",
                patient_id, chief_complaint
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
            println!("Stopped encounter {}.", short_id(encounter_id));
            if remote.is_some() {
                println!(
                    "Transcribing and summarizing on the server — run 'reload' in a few seconds."
                );
            }
            Ok(true)
        }

        Command::Audio { path } => {
            let Some(ctx) = remote else {
                println!(
                    "The audio upload command is available in remote mode; \
                     local mode transcribes captured mock audio when you run 'stop'."
                );
                return Ok(true);
            };

            let (patient_id, encounter_id) = manager.active_encounter().ok_or_else(|| {
                AppError::Audio(
                    "No encounter is recording. Select a patient and run 'start' first."
                        .to_string(),
                )
            })?;
            let content_type = content_type_for_audio_path(&path)?;
            let bytes = std::fs::read(&path).map_err(|error| {
                AppError::CliError(format!("Could not read the audio file: {error}"))
            })?;
            if bytes.is_empty() {
                return Err(AppError::CliError(
                    "The audio file is empty; choose a file containing audio.".to_string(),
                ));
            }

            let chunk_id = Uuid::now_v7();
            let request = er_api::AudioUploadRequest {
                chunk_id,
                chunk_index: 0,
                is_final: true,
                content_type: content_type.to_string(),
            };
            let intent = ctx
                .runtime
                .block_on(
                    ctx.api
                        .request_audio_upload(patient_id, encounter_id, &request),
                )
                .map_err(|error| safe_request_error("Requesting an audio upload", &error))?;
            ctx.runtime
                .block_on(
                    ctx.api
                        .put_presigned(&intent.upload_url, bytes, content_type),
                )
                .map_err(|error| safe_request_error("Uploading audio", &error))?;
            let initial = ctx
                .runtime
                .block_on(ctx.api.complete_audio(chunk_id))
                .map_err(|error| safe_request_error("Completing the audio upload", &error))?;

            // Completion acknowledged durable server work. Mark ownership before
            // observation so a timeout, 404, disconnect, or Ctrl-C cannot bring
            // the device mock-transcript path back.
            manager.mark_encounter_server_stt(encounter_id)?;
            println!("Uploaded audio chunk {}.", short_id(chunk_id));

            let outcome = ctx.runtime.block_on(observe_audio_chunk(
                ctx.api,
                chunk_id,
                initial,
                ObservationPolicy::CLI,
                |status| println!("Audio chunk {} status: {status}", short_id(chunk_id)),
            ));
            println!("{}", observation_message(&outcome));
            Ok(true)
        }

        Command::Lab { name, value, unit } => {
            let patient_id = manager
                .active_patient_id()
                .ok_or(NoActivePatient("No active patient found".into()))?;

            let value: f64 = parse_string_to_f64(value)?;

            let result = ClinicalResult {
                id: Uuid::now_v7(),
                patient_id,
                source: er_domain::ResultSource::Manual,
                timestamp: Utc::now(),
                result_type: er_domain::ResultType::Lab(LabResult {
                    fhir_id: None,
                    loinc_code: None,
                    display_name: name.clone(),
                    value,
                    unit: unit.clone(),
                    reference_range: None,
                    is_abnormal: false,
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

        Command::Med {
            kind,
            name,
            dose,
            route,
        } => {
            let kind_label = match kind {
                MedicationKind::Home => "home",
                MedicationKind::AdministeredInEd => "given",
            };
            manager.record_medication(name.clone(), dose.clone(), route.clone(), kind)?;
            let dose_label = if dose.is_empty() {
                String::new()
            } else {
                format!(" {dose}")
            };
            let route_label = route.map(|r| format!(" {r}")).unwrap_or_default();
            println!("Recorded medication [{kind_label}]: {name}{dose_label}{route_label}");
            Ok(true)
        }

        Command::Dx {
            text,
            icd10_code,
            is_primary,
        } => {
            manager.assign_diagnosis(text.clone(), icd10_code.clone(), is_primary)?;
            let primary_label = if is_primary { " (primary)" } else { "" };
            let code_label = icd10_code.map(|c| format!(" [{c}]")).unwrap_or_default();
            println!("Diagnosis assigned: {text}{primary_label}{code_label}");
            Ok(true)
        }

        Command::Data { physician_review } => {
            manager.record_data_reviewed(physician_review.clone())?;
            println!("Data reviewed recorded: {physician_review}");
            Ok(true)
        }

        Command::Mdm => {
            if let Some(ctx) = remote {
                let patient_id = manager
                    .active_patient_id()
                    .ok_or(NoActivePatient("No active patient found".into()))?;
                let job_id = Uuid::now_v7();
                let accepted = ctx
                    .runtime
                    .block_on(ctx.api.request_mdm(patient_id, job_id))
                    .map_err(|error| safe_request_error("Requesting MDM generation", &error))?;
                println!("MDM queued ({}). Generating…", short_id(accepted.job_id));

                let outcome = ctx.runtime.block_on(observe_mdm_job(
                    ctx.api,
                    job_id,
                    accepted,
                    ObservationPolicy::CLI,
                    |status| println!("MDM job {} status: {status}", short_id(job_id)),
                ));
                if outcome == ObservationOutcome::Done {
                    manager.reload_active_patient()?;
                }
                println!("{}", mdm_observation_message(&outcome));
                if outcome != ObservationOutcome::Done {
                    return Ok(true);
                }
            } else {
                manager.trigger_mdm()?;
            }

            println!("MDM generated and note moved to Finalizing.");
            println!("── MDM ──────────────────────────────────────");
            if let Some(patient_id) = manager.active_patient_id()
                && let Some(note) = manager.get_note(patient_id)
            {
                match note.note_sections.mdm.as_deref() {
                    Some(mdm) => println!("{}", mdm_preview(mdm)),
                    None => println!("<no MDM text generated>"),
                }
            }
            println!("(run 'note' or 'export' to see the full note)");
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

        Command::Transcript => {
            let patient_id = manager
                .active_patient_id()
                .ok_or(NoActivePatient("No active patient found".into()))?;
            let note = manager
                .get_note(patient_id)
                .ok_or(AppError::NoteError("No note found".into()))?;

            if note.encounters.is_empty() {
                println!("No encounters recorded for the active patient.");
                return Ok(true);
            }

            for encounter in &note.encounters {
                let text = encounter
                    .transcript
                    .as_deref()
                    .filter(|text| !text.trim().is_empty())
                    .unwrap_or("‹none›");
                println!(
                    "Encounter {} [{:?}]: {}",
                    encounter.encounter_number, encounter.capture_mode, text
                );
            }
            Ok(true)
        }

        Command::Export => {
            let exported = manager.export_active_note()?;
            println!("{exported}");
            Ok(true)
        }

        Command::Reload => {
            let replayed_count = manager.reload_active_patient()?;
            // Identical code path either way — only the sentence changes, so the
            // operator can see which store just answered.
            let source = if remote.is_some() {
                "the server"
            } else {
                "SQLite"
            };
            println!("Reloaded active patient from {source}. Replayed {replayed_count} events.");
            Ok(true)
        }

        Command::Status => {
            let patient_count = manager.list_patients().len();
            let active_patient = manager
                .active_patient_id()
                .map(short_id)
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

/// First ~400 characters of the MDM, with an ellipsis if truncated.
/// Char-based (not byte) so it never splits a multi-byte UTF-8 sequence.
fn mdm_preview(mdm: &str) -> String {
    const MAX_CHARS: usize = 400;
    if mdm.chars().count() <= MAX_CHARS {
        mdm.to_string()
    } else {
        let truncated: String = mdm.chars().take(MAX_CHARS).collect();
        format!("{truncated}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use er_ai::summarizer::MockSummarizer;
    use er_app::EventBus;
    use er_app::encounter_driver::EncounterDriver;
    use er_app::load;
    use er_domain::{PatientId, PatientRecord, Shift};
    use er_pipeline::audio::MockAudioCapture;
    use er_pipeline::stt::MockTranscriber;
    use er_store::repository::{Repository, SqliteRepository};

    fn test_manager() -> ShiftManager<MockAudioCapture, MockTranscriber, MockSummarizer> {
        let conn = er_store::db::open_memory_db().unwrap();
        let repo: Arc<dyn Repository> = Arc::new(SqliteRepository::new(conn));
        let bus = Arc::new(EventBus::new(32, Some(repo.clone())));
        let store = load::new_store();
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
            false,
        )
    }

    #[derive(Debug)]
    enum FakeReply {
        Status {
            status: &'static str,
            delay: Duration,
        },
        Remote(u16),
        Backend(&'static str),
        WrongChunk,
        Hang,
    }

    struct FakeStatusSource {
        replies: Mutex<VecDeque<FakeReply>>,
        calls: AtomicUsize,
        in_flight: AtomicUsize,
        max_in_flight: AtomicUsize,
        starts: Mutex<Vec<tokio::time::Instant>>,
    }

    impl FakeStatusSource {
        fn new(replies: impl IntoIterator<Item = FakeReply>) -> Self {
            Self {
                replies: Mutex::new(replies.into_iter().collect()),
                calls: AtomicUsize::new(0),
                in_flight: AtomicUsize::new(0),
                max_in_flight: AtomicUsize::new(0),
                starts: Mutex::new(Vec::new()),
            }
        }
    }

    struct InFlight<'a>(&'a AtomicUsize);

    impl Drop for InFlight<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl ChunkStatusSource for FakeStatusSource {
        // Preserve the trait's explicit `Send` bound for the returned future.
        #[allow(clippy::manual_async_fn)]
        fn audio_chunk(
            &self,
            chunk_id: Uuid,
        ) -> impl Future<Output = StoreResult<er_api::ChunkView>> + Send {
            async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let current = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                self.max_in_flight.fetch_max(current, Ordering::SeqCst);
                self.starts
                    .lock()
                    .unwrap()
                    .push(tokio::time::Instant::now());
                let _in_flight = InFlight(&self.in_flight);
                let reply = self
                    .replies
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(FakeReply::Hang);

                match reply {
                    FakeReply::Status { status, delay } => {
                        tokio::time::sleep(delay).await;
                        Ok(chunk_view(chunk_id, status))
                    }
                    FakeReply::Remote(status) => Err(StoreError::Remote {
                        status,
                        message: "secret backend response".to_string(),
                    }),
                    FakeReply::Backend(message) => {
                        Err(StoreError::backend(std::io::Error::other(message)))
                    }
                    FakeReply::WrongChunk => Ok(chunk_view(Uuid::now_v7(), "transcribing")),
                    FakeReply::Hang => std::future::pending().await,
                }
            }
        }
    }

    struct FakeJobStatusSource {
        statuses: Mutex<VecDeque<&'static str>>,
        calls: AtomicUsize,
    }

    impl FakeJobStatusSource {
        fn new(statuses: impl IntoIterator<Item = &'static str>) -> Self {
            Self {
                statuses: Mutex::new(statuses.into_iter().collect()),
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl JobStatusSource for FakeJobStatusSource {
        #[allow(clippy::manual_async_fn)]
        fn job(&self, job_id: Uuid) -> impl Future<Output = StoreResult<er_api::JobView>> + Send {
            async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let status = self.statuses.lock().unwrap().pop_front().unwrap();
                Ok(er_api::JobView {
                    job_id,
                    kind: "mdm".to_string(),
                    status: status.to_string(),
                    error: None,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                })
            }
        }
    }

    fn chunk_view(chunk_id: Uuid, status: &str) -> er_api::ChunkView {
        er_api::ChunkView {
            chunk_id,
            encounter_id: Uuid::nil(),
            chunk_index: 0,
            is_final: true,
            status: status.to_string(),
            duration_ms: None,
            error: None,
        }
    }

    fn policy(budget_secs: u64, delay_secs: u64, request_timeout_secs: u64) -> ObservationPolicy {
        ObservationPolicy {
            budget: Duration::from_secs(budget_secs),
            poll_delay: Duration::from_secs(delay_secs),
            request_timeout: Duration::from_secs(request_timeout_secs),
        }
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

        // `None` = local mode. Remote mode differs in exactly one arm (`new`),
        // and that path is covered end to end by er-server's sync_gate tests.
        assert!(handle_command(&mut manager, command, None).unwrap());

        let note = manager.get_note(patient_id).unwrap();
        assert_eq!(
            note.disposition,
            Some(Disposition::Discharge {
                instructions: "ibuprofen as needed".to_string(),
                follow_up: Some("Cardiology 1wk".to_string()),
            })
        );
    }

    fn run(
        manager: &mut ShiftManager<MockAudioCapture, MockTranscriber, MockSummarizer>,
        parts: &[&str],
    ) {
        let args: Vec<String> = parts.iter().map(|p| p.to_string()).collect();
        let command = Commands::new(&args).unwrap();
        assert!(handle_command(manager, command, None).unwrap());
    }

    #[test]
    fn med_dx_data_commands_persist_onto_note() {
        let mut manager = test_manager();
        let patient_id = manager.register_patient("chest pain");
        manager.set_active_patient(patient_id).unwrap();

        run(&mut manager, &["med", "given", "Aspirin", "324mg", "PO"]);
        run(
            &mut manager,
            &["dx", "\"Acute", "PE\"", "--primary", "I26.99"],
        );
        run(
            &mut manager,
            &["data", "\"CTA", "chest:", "PE", "in", "RLL\""],
        );

        let note = manager.get_note(patient_id).unwrap();

        assert_eq!(note.medication.len(), 1);
        let med = &note.medication[0];
        assert_eq!(med.name, "Aspirin");
        assert_eq!(med.dose, "324mg");
        assert_eq!(med.route, Some("PO".to_string()));
        assert_eq!(med.kind, MedicationKind::AdministeredInEd);

        assert_eq!(note.diagnoses.len(), 1);
        let dx = &note.diagnoses[0];
        assert_eq!(dx.text, "Acute PE");
        assert_eq!(dx.icd10_code, Some("I26.99".to_string()));
        assert!(dx.is_primary);

        assert_eq!(note.data_reviewed, vec!["CTA chest: PE in RLL".to_string()]);
    }

    #[test]
    fn mdm_command_generates_mdm_and_returns_ok() {
        let mut manager = test_manager();
        let patient_id = manager.register_patient("chest pain");
        manager.set_active_patient(patient_id).unwrap();

        // Reach Idle with a generated HPI so trigger_mdm's preconditions hold.
        run(&mut manager, &["start", "ambient"]);
        run(&mut manager, &["stop"]);

        // The mdm command itself: must succeed and persist the MDM.
        run(&mut manager, &["mdm"]);

        let note = manager.get_note(patient_id).unwrap();
        assert_eq!(
            note.note_sections.mdm.as_deref(),
            Some("[mock MDM: moderate complexity, 2 diagnoses considered]")
        );
    }

    #[test]
    fn mdm_preview_returns_short_text_unchanged() {
        let short = "Moderate complexity MDM. 2 diagnoses considered.";
        assert_eq!(mdm_preview(short), short);
    }

    #[test]
    fn mdm_preview_truncates_long_text_with_ellipsis() {
        let long = "x".repeat(500);
        let preview = mdm_preview(&long);

        // 400 kept chars + the single-char ellipsis.
        assert_eq!(preview.chars().count(), 401);
        assert!(preview.ends_with('…'));
        assert!(preview.starts_with("xxxx"));
    }

    #[test]
    fn mdm_preview_handles_multibyte_boundary() {
        // 401 multi-byte chars: truncating by bytes would panic; by chars is safe.
        let multibyte = "é".repeat(401);
        let preview = mdm_preview(&multibyte);
        assert_eq!(preview.chars().count(), 401); // 400 'é' + '…'
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn audio_and_transcript_commands_parse_and_local_audio_is_explanatory() {
        let audio = Commands::new(&[
            "audio".to_string(),
            "fixtures/tiny".to_string(),
            "recording.wav".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            audio.commands,
            Command::Audio { path } if path.as_path() == Path::new("fixtures/tiny recording.wav")
        ));
        assert!(matches!(
            Commands::new(&["transcript".to_string()]).unwrap().commands,
            Command::Transcript
        ));
        assert!(Commands::new(&["audio".to_string()]).is_err());

        let mut manager = test_manager();
        let local_audio = Commands::new(&[
            "audio".to_string(),
            "does-not-need-to-exist.wav".to_string(),
        ])
        .unwrap();
        assert!(
            handle_command(&mut manager, local_audio, None).unwrap(),
            "local mode explains the command without touching the filesystem"
        );
    }

    #[test]
    fn audio_extensions_map_to_the_server_allowlist() {
        for (path, expected) in [
            ("a.WAV", "audio/wav"),
            ("a.ogg", "audio/ogg"),
            ("a.m4a", "audio/mp4"),
            ("a.mp4", "audio/mp4"),
            ("a.mp3", "audio/mpeg"),
            ("a.mpeg", "audio/mpeg"),
        ] {
            assert_eq!(
                content_type_for_audio_path(Path::new(path)).unwrap(),
                expected
            );
        }
        assert!(content_type_for_audio_path(Path::new("a.flac")).is_err());
        assert!(content_type_for_audio_path(Path::new("no-extension")).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn an_initial_terminal_status_returns_without_a_get() {
        for (status, expected) in [
            (er_api::chunk_status::DONE, ObservationOutcome::Done),
            (er_api::chunk_status::FAILED, ObservationOutcome::Failed),
        ] {
            let chunk_id = Uuid::now_v7();
            let source = FakeStatusSource::new([]);
            let mut printed = Vec::new();

            let outcome = observe_audio_chunk(
                &source,
                chunk_id,
                chunk_view(chunk_id, status),
                policy(30, 2, 5),
                |value| printed.push(value.to_string()),
            )
            .await;

            assert_eq!(outcome, expected);
            assert_eq!(source.calls.load(Ordering::SeqCst), 0);
            assert_eq!(printed, vec![status]);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn mdm_observation_polls_transitions_then_stops_on_done() {
        let job_id = Uuid::now_v7();
        let source =
            FakeJobStatusSource::new([er_api::job_status::RUNNING, er_api::job_status::DONE]);
        let mut printed = Vec::new();
        let outcome = observe_mdm_job(
            &source,
            job_id,
            er_api::MdmAccepted {
                job_id,
                status: er_api::job_status::QUEUED.to_string(),
            },
            policy(30, 2, 5),
            |status| printed.push(status.to_string()),
        )
        .await;

        assert_eq!(outcome, ObservationOutcome::Done);
        assert_eq!(source.calls.load(Ordering::SeqCst), 2);
        assert_eq!(printed, vec!["queued", "running", "done"]);
    }

    #[tokio::test(start_paused = true)]
    async fn observation_prints_transitions_only_and_stops_on_done() {
        let chunk_id = Uuid::now_v7();
        let source = FakeStatusSource::new([
            FakeReply::Status {
                status: "uploaded",
                delay: Duration::ZERO,
            },
            FakeReply::Status {
                status: "transcribing",
                delay: Duration::ZERO,
            },
            FakeReply::Status {
                status: "transcribing",
                delay: Duration::ZERO,
            },
            FakeReply::Status {
                status: "done",
                delay: Duration::ZERO,
            },
        ]);
        let mut printed = Vec::new();

        let outcome = observe_audio_chunk(
            &source,
            chunk_id,
            chunk_view(chunk_id, "uploaded"),
            policy(30, 2, 5),
            |value| printed.push(value.to_string()),
        )
        .await;

        assert_eq!(outcome, ObservationOutcome::Done);
        assert_eq!(source.calls.load(Ordering::SeqCst), 4);
        assert_eq!(printed, vec!["uploaded", "transcribing", "done"]);
    }

    #[tokio::test(start_paused = true)]
    async fn unknown_status_and_a_stalled_get_are_bounded_by_the_overall_deadline() {
        let chunk_id = Uuid::now_v7();
        let source = FakeStatusSource::new([
            FakeReply::Status {
                status: "vendor_queued",
                delay: Duration::ZERO,
            },
            FakeReply::Hang,
        ]);
        let started = tokio::time::Instant::now();
        let mut printed = Vec::new();

        let outcome = observe_audio_chunk(
            &source,
            chunk_id,
            chunk_view(chunk_id, "uploaded"),
            policy(6, 2, 100),
            |value| printed.push(value.to_string()),
        )
        .await;

        assert_eq!(
            outcome,
            ObservationOutcome::PendingAtDeadline {
                last_status: "vendor_queued".to_string(),
            }
        );
        assert_eq!(
            tokio::time::Instant::now().duration_since(started),
            Duration::from_secs(6)
        );
        assert_eq!(printed, vec!["uploaded", "vendor_queued"]);
        assert_eq!(source.in_flight.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_per_request_timeout_stops_observation_before_the_overall_deadline() {
        let chunk_id = Uuid::now_v7();
        let source = FakeStatusSource::new([FakeReply::Hang]);
        let started = tokio::time::Instant::now();

        let outcome = observe_audio_chunk(
            &source,
            chunk_id,
            chunk_view(chunk_id, "uploaded"),
            policy(30, 2, 5),
            |_| {},
        )
        .await;

        assert_eq!(
            outcome,
            ObservationOutcome::ObservationUnavailable {
                reason: ObservationUnavailableReason::RequestTimedOut,
                last_status: "uploaded".to_string(),
            }
        );
        assert_eq!(
            tokio::time::Instant::now().duration_since(started),
            Duration::from_secs(7)
        );
        assert_eq!(source.in_flight.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn slow_responses_never_overlap_or_trigger_catch_up_requests() {
        let chunk_id = Uuid::now_v7();
        let source = FakeStatusSource::new([
            FakeReply::Status {
                status: "transcribing",
                delay: Duration::from_secs(3),
            },
            FakeReply::Status {
                status: "done",
                delay: Duration::from_secs(3),
            },
        ]);

        let outcome = observe_audio_chunk(
            &source,
            chunk_id,
            chunk_view(chunk_id, "uploaded"),
            policy(30, 2, 10),
            |_| {},
        )
        .await;

        assert_eq!(outcome, ObservationOutcome::Done);
        assert_eq!(source.max_in_flight.load(Ordering::SeqCst), 1);
        let starts = source.starts.lock().unwrap();
        assert_eq!(starts.len(), 2);
        assert_eq!(starts[1].duration_since(starts[0]), Duration::from_secs(5));
    }

    #[tokio::test(start_paused = true)]
    async fn status_failures_are_categorized_without_raw_diagnostics() {
        for (status, reason) in [
            (401, ObservationUnavailableReason::Authentication),
            (403, ObservationUnavailableReason::Authentication),
            (404, ObservationUnavailableReason::NotFound),
            (429, ObservationUnavailableReason::TemporarilyUnavailable),
            (500, ObservationUnavailableReason::TemporarilyUnavailable),
            (503, ObservationUnavailableReason::TemporarilyUnavailable),
            (409, ObservationUnavailableReason::Rejected(409)),
        ] {
            let chunk_id = Uuid::now_v7();
            let source = FakeStatusSource::new([FakeReply::Remote(status)]);
            let outcome = observe_audio_chunk(
                &source,
                chunk_id,
                chunk_view(chunk_id, "uploaded"),
                policy(30, 1, 5),
                |_| {},
            )
            .await;
            assert_eq!(
                outcome,
                ObservationOutcome::ObservationUnavailable {
                    reason,
                    last_status: "uploaded".to_string(),
                }
            );
            let rendered = observation_message(&outcome);
            assert!(!rendered.contains("secret backend response"));
        }

        for diagnostic in [
            "malformed JSON containing patient speech",
            "disconnect at https://presigned.example/?secret=credential",
        ] {
            let chunk_id = Uuid::now_v7();
            let source = FakeStatusSource::new([FakeReply::Backend(diagnostic)]);
            let outcome = observe_audio_chunk(
                &source,
                chunk_id,
                chunk_view(chunk_id, "uploaded"),
                policy(30, 1, 5),
                |_| {},
            )
            .await;
            let rendered = observation_message(&outcome);
            assert_eq!(
                outcome,
                ObservationOutcome::ObservationUnavailable {
                    reason: ObservationUnavailableReason::TemporarilyUnavailable,
                    last_status: "uploaded".to_string(),
                }
            );
            assert!(!rendered.contains(diagnostic));
            assert!(!rendered.contains("presigned"));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_response_for_another_chunk_is_rejected_safely() {
        let chunk_id = Uuid::now_v7();
        let source = FakeStatusSource::new([FakeReply::WrongChunk]);
        let outcome = observe_audio_chunk(
            &source,
            chunk_id,
            chunk_view(chunk_id, "uploaded"),
            policy(30, 1, 5),
            |_| {},
        )
        .await;

        assert_eq!(
            outcome,
            ObservationOutcome::ObservationUnavailable {
                reason: ObservationUnavailableReason::InvalidResponse,
                last_status: "uploaded".to_string(),
            }
        );
    }
}
