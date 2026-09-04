//! Cloud speech-to-text — the seam the Step 6 worker transcribes through.
//!
//! This is the *second* STT trait in this crate, and the split is deliberate
//! (Step 6 §C8). [`crate::stt::Transcriber`] models on-device transcription:
//! synchronous, fed raw 16 kHz PCM samples straight off the capture layer, no
//! I/O in its contract, and destined for whisper-rs on an iPhone. `SpeechToText`
//! models the cloud: asynchronous because it *is* an HTTP call, fed a whole
//! container file (WAV/OGG/M4A bytes plus the MIME type saying which), and
//! running in a headless worker rather than on the phone.
//!
//! Both produce the same [`TranscriptResult`], because "a transcript" means one
//! thing in this system no matter who produced it.
//!
//! Nothing in this file touches the network. The one real implementation
//! (`DeepgramTranscriber`, S6) lives behind the `cloud-stt` feature so a device
//! build of `er-pipeline` never compiles an HTTP client.

use crate::error::{PipelineError, PipelineResult};
use crate::stt::TranscriptResult;

/// Transcribe a complete audio *file* into text.
///
/// Async by RPITIT rather than `#[async_trait]`, matching `er_ai::Summarizer`:
/// one async-trait style per codebase. The consequence is that the trait is not
/// dyn-compatible, so a caller needing a runtime choice reaches for
/// [`AppSpeechToText`] instead of `Box<dyn SpeechToText>` — the `AppSummarizer`
/// pattern, adopted for the same reason.
///
/// The `+ Send` bound on the returned future is load-bearing, not decoration:
/// the worker awaits this from a task on a multi-threaded tokio runtime.
pub trait SpeechToText: Send + Sync {
    /// `audio` is a whole container file — header and all, byte-for-byte what
    /// the device PUT to object storage.
    ///
    /// `content_type` is that file's MIME type, carried on the chunk row since
    /// the upload intent declared it (S3). Implementations pass it along rather
    /// than sniffing the bytes: the ledger holds the type the client committed
    /// to, and an STT vendor given an honest Content-Type does not have to guess
    /// (§C7).
    fn transcribe(
        &self,
        audio: &[u8],
        content_type: &str,
    ) -> impl std::future::Future<Output = PipelineResult<TranscriptResult>> + Send;
}

/// What every mock transcript claims about the length of its audio. A constant,
/// not a measurement: a test double's contract is determinism, and
/// `MockTranscriber` has reported the same 30 seconds since Phase 2. The worker
/// writes this into `audio_chunks.duration_ms`, so a fixed non-zero value also
/// makes "the worker stored the STT's duration" an observable fact instead of
/// something indistinguishable from a default.
const MOCK_AUDIO_DURATION_SECS: f32 = 30.0;

/// Deliberately identical to `MockTranscriber`'s sentence — one canned
/// transcript in the repo, so a fixture that migrates from the device seam to
/// the cloud seam keeps asserting the same string.
const CANNED_TRANSCRIPT: &str = "[mock transcript: 44 year old male presenting with chest pain]";

/// Deterministic [`SpeechToText`] for tests, for CI, and for any dev run without
/// a `DEEPGRAM_API_KEY` (D6.12 — the worker falls back to this, loudly).
///
/// Two modes:
///
/// - **canned** (`echo: false`, the [`Default`]) — returns [`CANNED_TRANSCRIPT`]
///   and ignores the audio entirely. Right whenever a test only cares that *a*
///   transcript arrived and landed where it belongs.
/// - **echo** (`echo: true`) — returns the audio's own payload bytes read as
///   UTF-8. This sounds absurd until you see what it buys. The worker gate (S9)
///   uploads chunks whose "audio" is the literal text `segment zero.` /
///   `segment one.` wrapped in a WAV header, then asserts the *assembled*
///   transcript equals `"segment zero.\n\nsegment one."` exactly. A canned
///   string can only prove that a transcript appeared; echo proves that chunk
///   1's audio became chunk 1's words, in index order, after arriving out of
///   order (§C3). The mock exists to make ordering and attribution observable.
pub struct MockSpeechToText {
    pub echo: bool,
}

impl MockSpeechToText {
    /// Canned mode — the audio is ignored.
    pub fn new() -> Self {
        Self { echo: false }
    }
}

impl Default for MockSpeechToText {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeechToText for MockSpeechToText {
    async fn transcribe(
        &self,
        audio: &[u8],
        content_type: &str,
    ) -> PipelineResult<TranscriptResult> {
        // The one precondition the mock enforces. Every chunk row has a
        // content_type (S0 made the column NOT NULL, S3 range-checked it against
        // an allowlist), so a caller arriving here with nothing has dropped the
        // row's value somewhere between the ledger and the vendor. Fail in a
        // test rather than at the first real Deepgram call.
        if content_type.trim().is_empty() {
            return Err(PipelineError::Transcription(
                "no content type supplied for the audio".into(),
            ));
        }

        let text = if self.echo {
            String::from_utf8_lossy(audio_payload(audio)).into_owned()
        } else {
            CANNED_TRANSCRIPT.to_string()
        };

        Ok(TranscriptResult {
            text,
            confidence: Some(0.95),
            audio_duration_secs: MOCK_AUDIO_DURATION_SECS,
            processing_duration_secs: 0.0,
        })
    }
}

/// The spoken part of a container: for RIFF/WAVE, the contents of the `data`
/// chunk; for anything else — or a RIFF this cannot make sense of — the whole
/// buffer.
///
/// Echo mode needs this because a WAV is a header *then* samples (§C3). Echoing
/// the header too would prefix every transcript with `RIFF$\0\0\0WAVEfmt …` and
/// downgrade the worker gate's exact-equality assertions into substring guesses.
fn audio_payload(audio: &[u8]) -> &[u8] {
    if audio.len() < 12 || &audio[0..4] != b"RIFF" || &audio[8..12] != b"WAVE" {
        return audio;
    }

    // RIFF is a list of chunks — [4-byte id][4-byte little-endian length][body],
    // each body padded to an even length. Walk it instead of assuming the
    // canonical 44-byte prelude: real encoders interleave LIST/fact chunks
    // before `data`, and the walker costs three lines more than a hardcoded
    // offset that would silently return garbage for those files.
    let mut pos = 12;
    while pos + 8 <= audio.len() {
        let id = &audio[pos..pos + 4];
        let len = u32::from_le_bytes([
            audio[pos + 4],
            audio[pos + 5],
            audio[pos + 6],
            audio[pos + 7],
        ]) as usize;
        let body = pos + 8;

        if id == b"data" {
            // Clamped, because a truncated upload declares more than it carries
            // and a panicking mock would report that as a harness bug.
            let end = body.saturating_add(len).min(audio.len());
            return audio.get(body..end).unwrap_or(&[]);
        }

        pos = body.saturating_add(len).saturating_add(len % 2);
    }

    audio
}

/// Runtime choice of transcriber for the worker, mirroring `er_ai::AppSummarizer`.
///
/// [`SpeechToText`] is RPITIT and therefore not dyn-compatible, so "real when the
/// key is configured, mock when it isn't" cannot be expressed as a
/// `Box<dyn SpeechToText>`. An enum over the implementations, itself implementing
/// the trait, gives the same runtime choice with static dispatch and no
/// allocation — and it is the shape this codebase already settled on once, for
/// exactly this reason.
pub enum AppSpeechToText {
    // ── S6 socket ───────────────────────────────────────────────────────────
    // #[cfg(feature = "cloud-stt")]
    // Deepgram(crate::deepgram::DeepgramTranscriber),
    //
    // The `cloud-stt` feature and its optional dependencies are already declared
    // in Cargo.toml, so the fence can be *proved* empty (`cargo tree -p
    // er-pipeline` shows no reqwest) before anything hides behind it. The variant
    // itself lands with the implementation in S6: naming a module that does not
    // exist yet would break `cargo check --features cloud-stt` for a whole step,
    // and a feature flag nobody can compile is not a fence, it is a trap.
    Mock(MockSpeechToText),
}

impl SpeechToText for AppSpeechToText {
    async fn transcribe(
        &self,
        audio: &[u8],
        content_type: &str,
    ) -> PipelineResult<TranscriptResult> {
        match self {
            AppSpeechToText::Mock(m) => m.transcribe(audio, content_type).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The S4 fixture in miniature: a 44-byte RIFF/WAVE prelude (16 kHz mono
    /// 16-bit) wrapped around arbitrary bytes. Deliberately the same shape as
    /// `er-server/tests/common/mod.rs::tiny_wav` — the worker gate uploads what
    /// that builds, and these tests pin what the mock does with it.
    fn tiny_wav(payload: &[u8]) -> Vec<u8> {
        let data_len = payload.len() as u32;
        let mut wav = Vec::with_capacity(44 + payload.len());
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&16_000u32.to_le_bytes());
        wav.extend_from_slice(&32_000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.extend_from_slice(payload);
        wav
    }

    #[tokio::test]
    async fn canned_mode_ignores_the_audio_and_reports_a_duration() {
        let stt = MockSpeechToText::default();

        let result = stt
            .transcribe(&tiny_wav(b"anything at all"), "audio/wav")
            .await
            .expect("the canned mock never fails on well-formed input");

        assert_eq!(result.text, CANNED_TRANSCRIPT);
        assert_eq!(result.confidence, Some(0.95));
        // The literal, not MOCK_AUDIO_DURATION_SECS: asserting a constant against
        // itself is an assertion about nothing. The worker turns this into
        // `duration_ms`, so the number is part of the contract, not an internal.
        assert_eq!(result.audio_duration_secs, 30.0);
        assert_eq!(result.processing_duration_secs, 0.0);
    }

    #[tokio::test]
    async fn echo_mode_returns_the_wav_payload_without_the_header() {
        let stt = MockSpeechToText { echo: true };

        let result = stt
            .transcribe(&tiny_wav(b"segment zero."), "audio/wav")
            .await
            .expect("echo mode transcribes any bytes");

        // Exact equality is the point: no RIFF prelude, no trailing padding.
        // This is what lets the worker gate assert an assembled transcript
        // character-for-character instead of hunting for substrings.
        assert_eq!(result.text, "segment zero.");
    }

    #[tokio::test]
    async fn echo_mode_walks_past_chunks_it_does_not_understand() {
        // A LIST chunk (metadata) ahead of `data` — legal RIFF, and what a real
        // encoder emits. A hardcoded 44-byte skip would return garbage here.
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&0u32.to_le_bytes()); // size field: unread by the walker
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"LIST");
        wav.extend_from_slice(&4u32.to_le_bytes());
        wav.extend_from_slice(b"INFO");
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(b"segment one.".len() as u32).to_le_bytes());
        wav.extend_from_slice(b"segment one.");

        let stt = MockSpeechToText { echo: true };
        let result = stt.transcribe(&wav, "audio/wav").await.unwrap();

        assert_eq!(result.text, "segment one.");
    }

    #[tokio::test]
    async fn echo_mode_returns_the_whole_buffer_when_it_is_not_a_container() {
        let stt = MockSpeechToText { echo: true };

        let result = stt
            .transcribe(b"bare bytes, no header", "audio/ogg")
            .await
            .unwrap();

        assert_eq!(result.text, "bare bytes, no header");
    }

    #[tokio::test]
    async fn echo_mode_survives_a_data_chunk_that_lies_about_its_length() {
        // A truncated upload: the header promises 100 bytes, the file carries 5.
        // The mock must clamp, not panic — a panic here would surface as a
        // harness failure rather than as the bad audio it actually is.
        let mut wav = tiny_wav(b"short");
        let len_at = 40;
        wav[len_at..len_at + 4].copy_from_slice(&100u32.to_le_bytes());

        let stt = MockSpeechToText { echo: true };
        let result = stt.transcribe(&wav, "audio/wav").await.unwrap();

        assert_eq!(result.text, "short");
    }

    #[tokio::test]
    async fn a_missing_content_type_is_rejected() {
        let stt = MockSpeechToText { echo: true };

        let error = stt
            .transcribe(&tiny_wav(b"segment zero."), "  ")
            .await
            .expect_err("the chunk row always carries a content type");

        assert!(matches!(error, PipelineError::Transcription(_)));
    }

    #[tokio::test]
    async fn app_dispatch_forwards_to_the_mock_arm() {
        // The AppSpeechToText::Deepgram arm needs a live API key and is
        // exercised manually (E4), exactly as AppSummarizer::Claude is. This
        // proves the enum's own SpeechToText impl routes through to its variant.
        let stt = AppSpeechToText::Mock(MockSpeechToText { echo: true });

        let result = stt
            .transcribe(&tiny_wav(b"routed."), "audio/wav")
            .await
            .unwrap();

        assert_eq!(result.text, "routed.");
    }

    /// Generic on purpose. A *concrete* mock's future is `Send` whether or not
    /// the trait promises it, so spawning one proves nothing; only code written
    /// against the bound actually depends on it. This is the shape any helper in
    /// `er-server::worker` takes the moment it is written against the trait
    /// rather than against `AppSpeechToText`.
    async fn spawn_through_the_trait<S>(stt: S) -> PipelineResult<TranscriptResult>
    where
        S: SpeechToText + 'static,
    {
        tokio::spawn(async move { stt.transcribe(b"detached", "audio/wav").await })
            .await
            .expect("the spawned task does not panic")
    }

    #[tokio::test]
    async fn the_returned_future_is_send() {
        // The worker awaits transcribe() from a task on a multi-threaded runtime.
        // Dropping `+ Send` from the RPITIT bound breaks the helper above — here,
        // in the crate that owns the trait, rather than two steps later in
        // er-server as an inscrutable auto-trait error.
        let result = spawn_through_the_trait(AppSpeechToText::Mock(MockSpeechToText::new()))
            .await
            .unwrap();

        assert_eq!(result.text, CANNED_TRANSCRIPT);
    }
}
