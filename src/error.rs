use crate::models::NotePhase;

/// Unified error type for the application.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("invalid state transition from {from:?} on event {event}")]
    InvalidTransition { from: NotePhase, event: String },

    #[error("note is signed and immutable")]
    NoteImmutable,

    #[error("patient not found: {0}")]
    PatientNotFound(uuid::Uuid),

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
}

pub type AppResult<T> = std::result::Result<T, AppError>;
