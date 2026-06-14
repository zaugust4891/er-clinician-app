//! Audio/STT pipeline errors.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("audio capture error: {0}")]
    Audio(String),

    #[error("transcription error: {0}")]
    Transcription(String),
}

pub type PipelineResult<T> = std::result::Result<T, PipelineError>;
