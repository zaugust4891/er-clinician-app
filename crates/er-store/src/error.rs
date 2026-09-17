//! Persistence-layer errors. Wrap the SQLite driver and (de)serialization.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("backend error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("parse error in {context}: {source}")]
    Parse {
        context: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("server returned {status}: {message}")]
    Remote { status: u16, message: String },
}

pub type StoreResult<T> = std::result::Result<T, StoreError>;

/// Keeps er-store stays sqlx-free (it just holds a Box<dyn Error>), and in er-server
/// convert sqlx failures with .map_err(StoreError::backend). serde_json
impl StoreError {
    pub fn backend(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        StoreError::Backend(Box::new(e))
    }
}
