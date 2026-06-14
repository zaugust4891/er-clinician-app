//! Persistence-layer errors. Wrap the SQLite driver and (de)serialization.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("parse error in {context}: {source}")]
    Parse {
        context: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

pub type StoreResult<T> = std::result::Result<T, StoreError>;
