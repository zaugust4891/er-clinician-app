//! Offline phone composition. Hardware, auth, transport and generation are later steps.
pub mod core;
pub mod handle;
pub mod views;
pub use core::{CaptureAdapter, DeviceConfig, DeviceCore};
pub use er_store::device::{ConsentStatus, ContextCategory, DefinitionKind, RecorderStatus};
pub use handle::{DeviceHandle, DeviceListener};
pub use views::*;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DeviceError {
    #[error("ER_INVALID_INPUT")]
    InvalidInput,
    #[error("ER_INVALID_STATE")]
    InvalidState,
    #[error("ER_NOT_FOUND")]
    NotFound,
    #[error("ER_CONFLICT")]
    Conflict,
    #[error("ER_SCOPE")]
    Scope,
    #[error("ER_STORAGE")]
    Storage,
    #[error("ER_RECOVERY")]
    Recovery,
    #[error("ER_CLOSED")]
    Closed,
    #[error("ER_BUSY")]
    Busy,
    #[error("ER_INTERNAL")]
    Internal,
}
pub type Result<T> = std::result::Result<T, DeviceError>;
impl From<er_store::StoreError> for DeviceError {
    fn from(e: er_store::StoreError) -> Self {
        match e {
            er_store::StoreError::Conflict => Self::Conflict,
            er_store::StoreError::Scope => Self::Scope,
            er_store::StoreError::Integrity
            | er_store::StoreError::Schema
            | er_store::StoreError::Serialization(_)
            | er_store::StoreError::Parse { .. } => Self::Recovery,
            _ => Self::Storage,
        }
    }
}
impl From<er_app::AppError> for DeviceError {
    fn from(e: er_app::AppError) -> Self {
        match e {
            er_app::AppError::Store(e) => e.into(),
            _ => Self::InvalidState,
        }
    }
}

pub mod sync;
pub use sync::{AccessTokenProvider, SyncConfig, SyncView};
