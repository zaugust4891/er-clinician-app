//! Domain-layer errors.
//!
//! The pure core depends on nothing outward, so these reference only domain
//! types (`NotePhase`) and std — never `rusqlite`, `reqwest`, or the composite
//! `AppError`. Higher layers fold this into their own error via `#[from]`.
use crate::note::NotePhase;

#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("invalid state transition from {from:?} on event {event}")]
    InvalidTransition { from: NotePhase, event: String },

    #[error("note is signed and immutable")]
    NoteImmutable,
}

pub type DomainResult<T> = std::result::Result<T, DomainError>;
