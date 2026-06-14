//! Capture pipeline: platform-agnostic audio capture and speech-to-text
//! traits plus their desktop/test mocks. Kept nearly dep-free (domain types
//! only) so the future on-device build never compiles cloud machinery.
pub mod audio;
pub mod error;
pub mod stt;

pub use error::{PipelineError, PipelineResult};
