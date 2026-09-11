//! AI-layer errors with retry classification for background workers.

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("API request error: {0}")]
    Api(#[from] reqwest::Error),

    #[error("AI transport error: {0}")]
    Transport(String),

    #[error("AI provider rate limited the request: {0}")]
    RateLimited(String),

    #[error("AI provider refused the request (HTTP {status}): {message}")]
    Refused { status: u16, message: String },

    #[error("summarization error: {0}")]
    Summarization(String),
}

impl AiError {
    /// Whether another queue delivery could plausibly succeed without changing
    /// the request. Model-output and 4xx failures are terminal for the job.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Api(_) | Self::Transport(_) | Self::RateLimited(_)
        )
    }
}

pub type AiResult<T> = std::result::Result<T, AiError>;
