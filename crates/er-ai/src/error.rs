//! AI-layer errors. Wrap the HTTP client and model-output failures.
#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("API request error: {0}")]
    Api(#[from] reqwest::Error),

    #[error("summarization error: {0}")]
    Summarization(String),
}

pub type AiResult<T> = std::result::Result<T, AiError>;
