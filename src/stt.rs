use crate::error::Result;

/// Speech-to-text transcription interface.
///
/// Implementations:
/// - Local: whisper.cpp via whisper-rs (on-device, privacy-preserving)
/// - Cloud: Groq Whisper API or OpenAI Whisper API (fallback if on-device is too slow)
pub trait Transcriber: Send + Sync {
    /// Transcribe raw PCM audio bytes to text.
    /// Audio is expected to be 16kHz mono PCM (i16 samples).
    fn transcribe(&self, audio_pcm: &[u8]) -> Result<TranscriptResult>;
}

/// Result of a transcription operation.
#[derive(Debug, Clone)]
pub struct TranscriptResult {
    pub text: String,
    /// Confidence score from the STT engine (0.0 - 1.0), if available.
    pub confidence: Option<f32>,
    /// Duration of the audio in seconds.
    pub audio_duration_secs: f32,
    /// Time taken to transcribe in seconds.
    pub processing_duration_secs: f32,
}

/// Mock transcriber for desktop testing — returns canned text.
pub struct MockTranscriber;

impl Transcriber for MockTranscriber {
    fn transcribe(&self, _audio_pcm: &[u8]) -> Result<TranscriptResult> {
        Ok(TranscriptResult {
            text: "[mock transcript: 44 year old male presenting with chest pain]".into(),
            confidence: Some(0.95),
            audio_duration_secs: 30.0,
            processing_duration_secs: 0.1,
        })
    }
}
