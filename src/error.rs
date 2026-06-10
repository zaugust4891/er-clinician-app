use crate::domain::NotePhase;

/// Unified error type for the application.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("invalid state transition from {from:?} on event {event}")]
    InvalidTransition { from: NotePhase, event: String },

    #[error("note is signed and immutable")]
    NoteImmutable,

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

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("deserialization error: {0}")]
    DeserializeError(String),

    #[error("no active shift")]
    NoActiveShift,

    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("audio capture error: {0}")]
    Audio(String),

    #[error("transcription error: {0}")]
    Transcription(String),

    #[error("summarization error: {0}")]
    Summarization(String),

    #[error("API request error: {0}")]
    Api(#[from] reqwest::Error),

    #[error("de-identification error: {0}")]
    Deidentify(String),

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