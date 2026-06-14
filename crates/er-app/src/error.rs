//! Application-layer composite error.
//!
//! Preserves the pre-split public surface: `?` converts each inner-layer error
//! through the `#[from]` arms, while the orchestration/CLI-level variants stay
//! here unchanged. Domain state-machine errors (`InvalidTransition`,
//! `NoteImmutable`) now live in [`er_domain::DomainError`] and arrive via
//! `AppError::Domain`.
use er_ai::AiError;
use er_domain::DomainError;
use er_pipeline::PipelineError;
use er_store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Domain(#[from] DomainError),

    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(transparent)]
    Ai(#[from] AiError),

    #[error(transparent)]
    Pipeline(#[from] PipelineError),

    #[error("Invalid argument: {0}")]
    CliError(String),

    #[error("patient not found: {0}")]
    PatientNotFound(uuid::Uuid),

    #[error("patient id mismatch: {0}")]
    PatientIdError(String),

    #[error("no active patient: {0}")]
    NoActivePatient(String),

    #[error("note error: {0}")]
    NoteError(String),

    #[error("lab error: {0}")]
    LabResultError(String),

    #[error("disposition error: {0}")]
    DispositionError(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("deserialization error: {0}")]
    DeserializeError(String),

    #[error("no active shift")]
    NoActiveShift,

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("audio capture error: {0}")]
    Audio(String),

    #[error("transcription error: {0}")]
    Transcription(String),

    #[error("parse error in {context}: {source}")]
    ParseError {
        context: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("other error: {0}")]
    OtherError(String),
}

pub type AppResult<T> = std::result::Result<T, AppError>;

impl AppError {
    pub fn parse_error<E>(context: impl Into<String>, error: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        AppError::ParseError {
            context: context.into(),
            source: Box::new(error),
        }
    }
}
