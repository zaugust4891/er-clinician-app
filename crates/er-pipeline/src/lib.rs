//! Capture pipeline: platform-agnostic audio capture and speech-to-text
//! traits plus their desktop/test mocks. Kept nearly dep-free (domain types
//! only) so the future on-device build never compiles cloud machinery — the
//! one cloud implementation lives behind the default-off `cloud-stt` feature.
//!
//! Two STT seams live here, on purpose (Step 6 §C8): `stt` is the synchronous,
//! PCM-shaped, on-device one; `speech_to_text` is the async, file-shaped one the
//! server's transcription worker calls.
pub mod audio;
/// The one real cloud transcriber (Step 6 S6). Behind the default-off
/// `cloud-stt` feature, which S7 will enable in `er-server`. A standalone
/// default-feature build compiles no HTTP client (D6.21).
#[cfg(feature = "cloud-stt")]
pub mod deepgram;
pub mod error;
pub mod speech_to_text;
pub mod stt;

pub use error::{PipelineError, PipelineResult};
