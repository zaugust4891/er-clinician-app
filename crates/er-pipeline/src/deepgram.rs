//! The one real [`SpeechToText`] implementation: Deepgram's *pre-recorded*
//! transcription endpoint (Step 6 §C7).
//!
//! This module is the entire contact surface between this system and a
//! speech-to-text vendor. That is a deliberate size: one file, one HTTP call,
//! one pure parse function. Everything else — where the bytes came from, which
//! encounter they belong to, who the patient is — was decided before the seam
//! and is invisible from here. The trait takes audio and a MIME type and
//! nothing else (§C8), which is what makes "swap in Transcribe Medical if
//! Deepgram procurement stalls" a new file and one boot-time match arm rather
//! than a rewrite of the worker.
//!
//! Compiled only under the default-off `cloud-stt` feature, so a Phase 5 device
//! build of `er-pipeline` never pulls in an HTTP client (D6.21).
//!
//! **PHI posture (§C5).** Audio is the rawest PHI in the system and no regex
//! runs on sound, so the controls here are structural:
//!
//! - The request body is audio bytes; the request *metadata* is model
//!   parameters. No display label, no chief complaint, no chunk id, no org, no
//!   patient. A transcriber that is never told who is speaking cannot correlate
//!   what it hears with anyone.
//! - `mip_opt_out=true` is sent on every call, so the audio is excluded from
//!   Deepgram's Model Improvement Program by request rather than by somebody
//!   remembering to tick a box in a console (E2 still signs the BAA — a query
//!   parameter is not a legal instrument).
//! - The API key travels in an `Authorization` header, never in the URL, so it
//!   cannot surface in a `reqwest::Error`'s Display, a redirect log, or a proxy
//!   access log. [`DeepgramTranscriber`]'s `Debug` redacts it for the same
//!   reason.
//! - Errors carry vendor status codes and vendor error bodies, never transcript
//!   text. `log identifiers, never content` applies here as it does in
//!   `routes.rs`.

use std::fmt;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{PipelineError, PipelineResult};
use crate::speech_to_text::SpeechToText;
use crate::stt::TranscriptResult;

/// Deepgram's pre-recorded endpoint. Bytes in, JSON out; no SDK (§C7).
const LISTEN_URL: &str = "https://api.deepgram.com/v1/listen";

/// The medical vocabulary model — it knows "metoprolol" and "STEMI", which a
/// general model spells phonetically and the Phase 3 summarizer then has to
/// guess at.
const MODEL: &str = "nova-3-medical";

/// Per-attempt HTTP deadline, and the number that keeps the inner retry loop
/// inside the outer one.
///
/// The SQS queue this work arrives on has a **300 s visibility timeout**
/// (`er-server/src/aws.rs::AUDIO_JOB_VISIBILITY_TIMEOUT_SECS`). If one job's
/// total work can exceed that window, SQS hands the same message to a second
/// worker while the first is still transcribing — two Deepgram bills, two
/// writes, and a race for the same chunk row. So the whole inner loop must fit
/// with room to spare for the object fetch, the row updates, assembly and the
/// event append:
///
/// ```text
///   3 attempts × 60 s  +  0.5 s + 1.0 s of backoff   =  181.5 s worst case
///   300 s visibility  −  181.5 s                     =  118.5 s of headroom
/// ```
///
/// 60 s is ~10× generous for the ~60 s audio chunks §C3 describes; Deepgram's
/// pre-recorded API returns in a few seconds for those. The guide's original
/// outline said 300 s, which is exactly the visibility timeout and therefore
/// leaves no headroom at all — see D6.24.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(60);

/// One call plus two retries (`phase4-implementation-guide.md` §6.4). The
/// *outer* three attempts are SQS's, and they absorb worker deaths; these inner
/// ones absorb network blips without paying a redelivery round trip (§C2).
const MAX_ATTEMPTS: u32 = 3;

/// First backoff, doubling per retry: 500 ms, then 1 s. Deterministic rather
/// than jittered — one worker retrying one chunk is not a thundering herd, and
/// a fixed schedule is a schedule a test can assert.
const BACKOFF_BASE: Duration = Duration::from_millis(500);

/// How much of a vendor error body is worth keeping. Deepgram's failures read
/// like `{"err_code":"INVALID_AUTH","err_msg":"..."}`; the first couple of
/// hundred bytes carry the cause, and capping the rest keeps a DLQ message
/// legible.
const MAX_ERROR_BODY: usize = 200;

/// Cloud speech-to-text via Deepgram.
///
/// The `reqwest::Client` is built once and reused: it owns the connection pool,
/// and a fresh client per call would mean a fresh TLS handshake per chunk.
pub struct DeepgramTranscriber {
    http: reqwest::Client,
    api_key: String,
}

impl DeepgramTranscriber {
    /// Build a transcriber around an API key.
    ///
    /// The key is passed in rather than read from the environment, unlike
    /// `ClaudeSummarizer::new`. `er-pipeline` is a library that also compiles
    /// for a phone; reading `DEEPGRAM_API_KEY` here would put an env-var
    /// dependency inside a crate whose whole point is to have none. The
    /// server's `Config` already owns the value (`config.deepgram_api_key:
    /// Option<String>`), and the worker's boot decides once between this and
    /// `MockSpeechToText` (D6.12).
    ///
    /// A blank key is refused here rather than at the first call, because
    /// `DEEPGRAM_API_KEY=` in a `.env` file is a common shape of mistake and
    /// its natural consequence — a `401` on the first real encounter's audio,
    /// three strikes later a DLQ message — is a much worse place to learn about
    /// it than boot.
    pub fn new(api_key: impl Into<String>) -> PipelineResult<Self> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(PipelineError::Transcription(
                "a Deepgram API key was supplied but is empty".into(),
            ));
        }

        let http = reqwest::Client::builder()
            .timeout(ATTEMPT_TIMEOUT)
            .build()
            .map_err(|e| {
                PipelineError::Transcription(format!(
                    "could not build the Deepgram HTTP client: {e}"
                ))
            })?;

        Ok(Self { http, api_key })
    }

    /// One HTTP attempt: `Ok` with the parsed JSON body on a 2xx, `Err` with a
    /// verdict on whether trying again could plausibly help.
    ///
    /// Split out from the retry loop so that "did it work" and "should we try
    /// again" are two separate questions with two separate answers, instead of
    /// one tangled `match` on a `reqwest::Response`.
    async fn send_once(&self, audio: &[u8], content_type: &str) -> Result<Value, Attempt> {
        let response = self
            .http
            .post(LISTEN_URL)
            .query(&[
                ("model", MODEL),
                // Punctuation, capitalisation, and "12 mg" instead of "twelve
                // milligrams" — the summarizer reads better prose for free.
                ("smart_format", "true"),
                // Label who is speaking, and group the words into speaker turns.
                // Ambient ED audio is a conversation, and the Phase 3 prompt
                // already understands dialogue-shaped transcripts (§C7).
                ("diarize", "true"),
                ("utterances", "true"),
                // Keep this audio out of Deepgram's Model Improvement Program
                // (§C5). Verify the parameter name when the account is created
                // — vendor privacy flags get renamed.
                ("mip_opt_out", "true"),
            ])
            // A header, not a query parameter: URLs end up in error strings,
            // proxy logs, and redirect traces. Keys must not.
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Token {}", self.api_key),
            )
            // The ledger's declared type, forwarded verbatim. Deepgram sniffs
            // containers, but an honest Content-Type means it does not have to
            // guess, and a mismatch here is a bug worth seeing as a 400.
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(audio.to_vec())
            .send()
            .await;

        let response = match response {
            Ok(response) => response,
            // No status ever arrived: DNS, TLS, connection reset, or our own
            // deadline. `transport_is_retryable` decides which of those is
            // worth a second go.
            Err(e) => {
                return Err(Attempt {
                    retryable: transport_is_retryable(&e),
                    error: PipelineError::Transcription(format!("deepgram request failed: {e}")),
                });
            }
        };

        let status = response.status();
        if !status.is_success() {
            // Read the body before deciding anything: Deepgram explains itself
            // in it, and that explanation is the difference between a DLQ
            // message someone can act on and one that says "500".
            let body = response.text().await.unwrap_or_default();
            return Err(Attempt {
                retryable: is_retryable_status(status.as_u16()),
                error: PipelineError::Transcription(format!(
                    "deepgram {}: {}",
                    status.as_u16(),
                    truncate(body.trim(), MAX_ERROR_BODY)
                )),
            });
        }

        response.json::<Value>().await.map_err(|e| Attempt {
            // A 2xx whose body will not parse is not a blip; it is a contract
            // break or a vendor incident, and hammering it three times only
            // delays the DLQ message that will bring a human.
            retryable: false,
            error: PipelineError::Transcription(format!(
                "deepgram returned a success status with an unreadable body: {e}"
            )),
        })
    }
}

/// Deliberately hand-written. `#[derive(Debug)]` would print the API key the
/// first time anything logs a struct that holds a transcriber.
impl fmt::Debug for DeepgramTranscriber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeepgramTranscriber")
            .field("model", &MODEL)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl SpeechToText for DeepgramTranscriber {
    async fn transcribe(
        &self,
        audio: &[u8],
        content_type: &str,
    ) -> PipelineResult<TranscriptResult> {
        // The same precondition `MockSpeechToText` enforces (D6.22), for the
        // same reason and with more at stake: every chunk row carries a content
        // type, so a caller without one has dropped the ledger's value in
        // transit. Catching it here costs nothing; letting it through costs a
        // vendor round trip that returns a 400 and three SQS strikes.
        if content_type.trim().is_empty() {
            return Err(PipelineError::Transcription(
                "no content type supplied for the audio".into(),
            ));
        }

        // A zero-byte object is a completed upload of nothing. S4's HeadObject
        // check proves an object exists, not that it has contents; this is the
        // second half of that check, and it fails fast rather than paying for a
        // vendor 400.
        if audio.is_empty() {
            return Err(PipelineError::Transcription(
                "refusing to transcribe an empty audio buffer".into(),
            ));
        }

        // Measured across the whole call including retries, because that is
        // what the worker actually waited and what an operator wants to see
        // when a chunk takes a minute.
        let started = Instant::now();
        let mut attempts = 0u32;

        loop {
            match self.send_once(audio, content_type).await {
                Ok(body) => {
                    let mut result = parse_deepgram_response(&body)?;
                    result.processing_duration_secs = started.elapsed().as_secs_f32();
                    return Ok(result);
                }
                Err(attempt) => {
                    attempts += 1;
                    if !attempt.retryable || attempts >= MAX_ATTEMPTS {
                        // Out of inner attempts, or the failure will never get
                        // better. Either way the error propagates to the worker,
                        // which turns it into a strike against the SQS receive
                        // count — the outer loop (§C2) takes it from here.
                        return Err(attempt.error);
                    }
                    tokio::time::sleep(backoff_for(attempts - 1)).await;
                }
            }
        }
    }
}

/// One failed attempt, plus the only thing the retry loop needs to know about
/// it.
struct Attempt {
    retryable: bool,
    error: PipelineError,
}

/// Retry a status only when the same request could plausibly succeed later.
///
/// `429` means "you are going too fast" and `5xx` means "we are having a bad
/// time" — both are statements about the moment, not about the request. A `400`
/// (bad audio), `401` (bad key), or `413` (too large) is a statement about the
/// request itself and will be exactly as wrong in 500 ms, so it must fail fast
/// toward the SQS strike count rather than burning the visibility budget.
fn is_retryable_status(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

/// A transport failure means no HTTP status ever arrived — DNS, TLS, a reset
/// connection, a proxy hiccup. Those are the network blips the inner loop
/// exists for, so they are retryable, with one exception.
///
/// **A timeout is not retried.** An attempt that hit [`ATTEMPT_TIMEOUT`] has
/// already spent 60 s of the 300 s visibility budget; spending it twice more
/// takes the job to 181 s and leaves the worker racing its own redelivery
/// (§C2). A request that could not finish in ten times the expected duration is
/// also, empirically, not a blip. Fail now, let SQS's outer attempt retry the
/// whole job with a fresh budget.
fn transport_is_retryable(err: &reqwest::Error) -> bool {
    !err.is_timeout() && !err.is_builder()
}

/// 500 ms, then 1 s. `attempt` is zero-based.
fn backoff_for(attempt: u32) -> Duration {
    BACKOFF_BASE * 2u32.pow(attempt)
}

/// Cap a vendor error body, on a character boundary so a multi-byte glyph
/// cannot panic the error path.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let end = (0..=max)
        .rev()
        .find(|i| s.is_char_boundary(*i))
        .unwrap_or(0);
    format!("{}…", &s[..end])
}

/// Turn Deepgram's response tree into a [`TranscriptResult`] (§C7).
///
/// Pure, and separate from the HTTP call on purpose: this is where every
/// interesting decision lives — which of two transcript shapes to read, how to
/// format speakers, what counts as an answer at all — and keeping it a function
/// over a `serde_json::Value` means all of it is testable against canned
/// fixtures with no network, no key, and no vendor account.
///
/// The shape it walks, pruned to what is read:
///
/// ```json
/// { "metadata": { "duration": 42.68 },
///   "results": {
///     "channels":   [ { "alternatives": [ { "transcript": "…", "confidence": 0.98 } ] } ],
///     "utterances": [ { "speaker": 0, "transcript": "What brings you in today?" } ] } }
/// ```
///
/// Everything is read defensively — `get(...).and_then(...)` rather than a
/// `#[derive(Deserialize)]` struct — because a strict struct would reject the
/// *whole* response over one added or renamed field, and a vendor is free to
/// add fields. `serde_json::Value` here is not laziness; it is the same
/// forward-compatibility argument D6.16 makes for keeping `ChunkView::status` a
/// wire `String`.
fn parse_deepgram_response(v: &Value) -> PipelineResult<TranscriptResult> {
    let results = v.get("results");

    // channels[0].alternatives[0] — the flat, undiarized transcript and the
    // only place a confidence score lives.
    let alternative = results
        .and_then(|r| r.get("channels"))
        .and_then(Value::as_array)
        .and_then(|channels| channels.first())
        .and_then(|channel| channel.get("alternatives"))
        .and_then(Value::as_array)
        .and_then(|alternatives| alternatives.first());

    let utterances = results
        .and_then(|r| r.get("utterances"))
        .and_then(Value::as_array);

    // Prefer speaker turns when they are there. `utterances=true` is requested
    // on every call, but some model/parameter combinations return none, and a
    // transcript is more useful flat than missing.
    let text = match utterances {
        Some(utterances) if !utterances.is_empty() => Some(format_utterances(utterances)),
        _ => alternative
            .and_then(|alt| alt.get("transcript"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    };

    // Neither shape present. A 200 that carries no transcript field at all is
    // the vendor agreeing to a request it did not answer — a contract break, or
    // our own parse walking the wrong path after a schema change. Surfacing it
    // as an error puts a readable cause in the DLQ; swallowing it would write a
    // silently empty section into a medical record.
    //
    // Note the deliberate line between *absent* and *empty* (D6.23): a present
    // `"transcript": ""` is a successful transcription of silence, and silence
    // happens — a chunk cut while nobody was talking. That returns `Ok` with an
    // empty string, so a quiet moment costs the note a blank, not a gap marker
    // and a dead-letter message.
    let Some(text) = text else {
        return Err(PipelineError::Transcription(
            "deepgram returned a success status with no transcript: neither \
             results.utterances nor results.channels[0].alternatives[0].transcript was present"
                .into(),
        ));
    };

    Ok(TranscriptResult {
        text: text.trim_end().to_string(),
        confidence: alternative
            .and_then(|alt| alt.get("confidence"))
            .and_then(Value::as_f64)
            .map(|c| c as f32),
        // metadata.duration is Deepgram's own measurement of the audio, and it
        // is the value the worker writes to `audio_chunks.duration_ms` — the
        // column Step 7's usage log will bill from (D6.13). Absent, it is 0.0
        // rather than an error: a missing meter reading is not a reason to
        // throw away a good transcript.
        audio_duration_secs: v
            .get("metadata")
            .and_then(|m| m.get("duration"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0) as f32,
        // Filled in by `transcribe`, which is the only thing holding a clock.
        processing_duration_secs: 0.0,
    })
}

/// Speaker turns → `"Speaker 0: …"` lines, one per turn.
///
/// The dialogue shape matters downstream: `MockTranscriber`'s canned sentence
/// and the Phase 3 summarizer prompt both already assume a conversation, so
/// emitting turns keeps the cloud path producing the same *kind* of text the
/// device path does.
///
/// Blank turns are dropped (a bare `"Speaker 1:"` line teaches the summarizer
/// nothing), and a turn with no `speaker` field keeps its text without a label
/// rather than being attributed to an invented speaker 0.
fn format_utterances(utterances: &[Value]) -> String {
    utterances
        .iter()
        .filter_map(|utterance| {
            let text = utterance.get("transcript").and_then(Value::as_str)?.trim();
            if text.is_empty() {
                return None;
            }
            Some(match utterance.get("speaker").and_then(Value::as_u64) {
                Some(speaker) => format!("Speaker {speaker}: {text}"),
                None => text.to_string(),
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A response with diarization, trimmed to the fields §C7 documents. Two
    /// speakers, so the formatting rule has something to get wrong.
    fn diarized_response() -> Value {
        serde_json::json!({
            "metadata": { "duration": 42.68, "channels": 1 },
            "results": {
                "channels": [ { "alternatives": [ {
                    "transcript": "What brings you in today? I have been having chest pain since this morning.",
                    "confidence": 0.98
                } ] } ],
                "utterances": [
                    { "speaker": 0, "transcript": "What brings you in today?", "confidence": 0.99 },
                    { "speaker": 1, "transcript": "I have been having chest pain since this morning.", "confidence": 0.97 }
                ]
            }
        })
    }

    /// The same call, from a model/parameter combination that returned no
    /// utterances — the fallback shape.
    fn flat_response() -> Value {
        serde_json::json!({
            "metadata": { "duration": 12.5 },
            "results": { "channels": [ { "alternatives": [ {
                "transcript": "Patient reports chest pain radiating to the left arm.",
                "confidence": 0.91
            } ] } ] }
        })
    }

    #[test]
    fn utterances_become_speaker_labelled_lines() {
        let result = parse_deepgram_response(&diarized_response())
            .expect("a diarized response carries a transcript");

        assert_eq!(
            result.text,
            "Speaker 0: What brings you in today?\n\
             Speaker 1: I have been having chest pain since this morning."
        );
        // Confidence still comes from the channel alternative even when the
        // text came from the utterances — they are different fields of the same
        // answer.
        assert_eq!(result.confidence, Some(0.98));
        assert_eq!(result.audio_duration_secs, 42.68);
    }

    #[test]
    fn a_response_without_utterances_falls_back_to_the_flat_transcript() {
        let result =
            parse_deepgram_response(&flat_response()).expect("the flat shape is still an answer");

        assert_eq!(
            result.text,
            "Patient reports chest pain radiating to the left arm."
        );
        assert_eq!(result.confidence, Some(0.91));
        assert_eq!(result.audio_duration_secs, 12.5);
    }

    #[test]
    fn an_empty_utterances_array_falls_back_rather_than_returning_nothing() {
        // `utterances: []` is structurally present but says nothing. Reading it
        // as "the transcript is empty" would throw away the flat transcript
        // sitting right next to it.
        let mut response = flat_response();
        response["results"]["utterances"] = serde_json::json!([]);

        let result = parse_deepgram_response(&response).expect("the fallback still applies");

        assert_eq!(
            result.text,
            "Patient reports chest pain radiating to the left arm."
        );
    }

    #[test]
    fn a_two_hundred_with_no_transcript_at_all_is_an_error() {
        // The "200 but empty" case: the vendor agreed to a request it did not
        // answer. Better a readable DLQ message than a silently empty HPI.
        let response = serde_json::json!({ "metadata": { "duration": 30.0 }, "results": {} });

        let error = parse_deepgram_response(&response)
            .expect_err("a response with neither transcript shape is not an answer");

        assert!(matches!(error, PipelineError::Transcription(_)));
        assert!(
            error.to_string().contains("no transcript"),
            "the cause must be readable in a dead-letter message, got: {error}"
        );
    }

    #[test]
    fn silence_transcribes_to_an_empty_string_not_an_error() {
        // D6.23: *absent* is a contract break, *empty* is silence. A chunk cut
        // while nobody was talking must not cost the encounter a gap marker and
        // a dead-letter message.
        let mut response = flat_response();
        response["results"]["channels"][0]["alternatives"][0]["transcript"] = serde_json::json!("");

        let result = parse_deepgram_response(&response).expect("silence is a successful result");

        assert_eq!(result.text, "");
    }

    #[test]
    fn blank_turns_are_dropped_and_unlabelled_turns_keep_their_text() {
        let response = serde_json::json!({
            "metadata": { "duration": 3.0 },
            "results": { "utterances": [
                { "speaker": 0, "transcript": "Deep breath for me." },
                { "speaker": 1, "transcript": "   " },
                { "transcript": "Any allergies?" }
            ] }
        });

        let result = parse_deepgram_response(&response).unwrap();

        assert_eq!(
            result.text,
            "Speaker 0: Deep breath for me.\nAny allergies?"
        );
    }

    #[test]
    fn a_missing_duration_is_zero_rather_than_a_thrown_away_transcript() {
        let mut response = flat_response();
        response["metadata"] = serde_json::json!({});

        let result = parse_deepgram_response(&response).unwrap();

        assert_eq!(result.audio_duration_secs, 0.0);
        assert!(!result.text.is_empty());
    }

    #[test]
    fn only_429_and_5xx_are_worth_retrying() {
        // Retryable: statements about the moment.
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(502));
        assert!(is_retryable_status(503));
        assert!(is_retryable_status(599));

        // Not retryable: statements about the request. Retrying these only
        // burns the visibility budget before the inevitable strike.
        assert!(!is_retryable_status(400)); // bad audio
        assert!(!is_retryable_status(401)); // bad key
        assert!(!is_retryable_status(403));
        assert!(!is_retryable_status(413)); // too large
        assert!(!is_retryable_status(200));
        assert!(!is_retryable_status(600));
    }

    #[test]
    fn the_retry_budget_fits_inside_the_sqs_visibility_timeout() {
        // The invariant that makes the inner loop safe (§C2): if a single job's
        // work could outlast the 300 s visibility timeout, SQS would hand the
        // message to a second worker mid-flight. This is that arithmetic,
        // asserted — so raising ATTEMPT_TIMEOUT or MAX_ATTEMPTS without raising
        // the queue attribute fails here rather than in production at 2 a.m.
        const SQS_VISIBILITY_TIMEOUT: Duration = Duration::from_secs(300);

        let backoff: Duration = (0..MAX_ATTEMPTS - 1).map(backoff_for).sum();
        let worst_case = ATTEMPT_TIMEOUT * MAX_ATTEMPTS + backoff;

        assert_eq!(backoff, Duration::from_millis(1_500));
        assert_eq!(worst_case, Duration::from_millis(181_500));
        assert!(
            worst_case < SQS_VISIBILITY_TIMEOUT,
            "the inner retry loop ({worst_case:?}) must leave headroom inside the \
             {SQS_VISIBILITY_TIMEOUT:?} visibility timeout for the object fetch, \
             the row writes, assembly, and the event append"
        );
    }

    #[test]
    fn backoff_doubles_from_half_a_second() {
        assert_eq!(backoff_for(0), Duration::from_millis(500));
        assert_eq!(backoff_for(1), Duration::from_millis(1_000));
    }

    #[test]
    fn a_blank_api_key_is_refused_at_construction() {
        let error = DeepgramTranscriber::new("   ")
            .expect_err("an empty DEEPGRAM_API_KEY must not reach the first encounter");

        assert!(matches!(error, PipelineError::Transcription(_)));
    }

    #[test]
    fn debug_never_prints_the_api_key() {
        let stt = DeepgramTranscriber::new("dg_super_secret_key").unwrap();

        let rendered = format!("{stt:?}");

        assert!(!rendered.contains("dg_super_secret_key"), "got: {rendered}");
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains(MODEL));
    }

    #[tokio::test]
    async fn a_missing_content_type_is_rejected_before_any_network_call() {
        // No network reached: the precondition short-circuits, which is why
        // this test can run in CI with a fake key.
        let stt = DeepgramTranscriber::new("dg_not_a_real_key").unwrap();

        let error = stt
            .transcribe(b"RIFF....WAVE", "  ")
            .await
            .expect_err("the chunk row always carries a content type");

        assert!(matches!(error, PipelineError::Transcription(_)));
    }

    #[tokio::test]
    async fn empty_audio_is_rejected_before_any_network_call() {
        let stt = DeepgramTranscriber::new("dg_not_a_real_key").unwrap();

        let error = stt
            .transcribe(b"", "audio/wav")
            .await
            .expect_err("a zero-byte object is a completed upload of nothing");

        assert!(matches!(error, PipelineError::Transcription(_)));
    }

    #[tokio::test]
    async fn the_deepgram_arm_of_the_dispatch_enum_is_reachable() {
        // The enum's Deepgram variant exists and satisfies the trait. The
        // happy path needs a live key and is exercised manually (E4), exactly
        // as `AppSummarizer::Claude` is; what is proved here is that the
        // variant routes and that its preconditions travel with it.
        use crate::speech_to_text::AppSpeechToText;

        let stt = AppSpeechToText::Deepgram(DeepgramTranscriber::new("dg_not_a_real_key").unwrap());

        let error = stt
            .transcribe(b"", "audio/wav")
            .await
            .expect_err("the empty-audio guard applies through the enum too");

        assert!(matches!(error, PipelineError::Transcription(_)));
    }

    #[test]
    fn the_transcriber_is_send_and_sync() {
        // `SpeechToText: Send + Sync` and the worker holds one in shared state
        // across a multi-threaded runtime. A future field (an `Rc`, a `Cell`)
        // that broke this would otherwise surface a step later, in er-server,
        // as an auto-trait error about a type that is not mentioned.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<DeepgramTranscriber>();
    }

    #[test]
    fn a_long_vendor_error_body_is_truncated_on_a_character_boundary() {
        // A multi-byte glyph straddling the cap must not panic the error path —
        // the error path is what carries the cause to the DLQ.
        //
        // The 3-byte "€" is chosen so that MAX_ERROR_BODY is *not* a multiple of
        // the character width: 200 % 3 == 2, so byte 200 lands mid-character and
        // a naive `&s[..max]` panics. A 2-byte glyph would have landed exactly on
        // a boundary and let that mutation through.
        let body = "€".repeat(MAX_ERROR_BODY);

        let capped = truncate(&body, MAX_ERROR_BODY);

        assert!(capped.ends_with('…'));
        assert!(capped.len() <= MAX_ERROR_BODY + '…'.len_utf8());
    }

    #[test]
    fn a_short_error_body_is_left_alone() {
        assert_eq!(
            truncate("{\"err_code\":\"INVALID_AUTH\"}", MAX_ERROR_BODY),
            "{\"err_code\":\"INVALID_AUTH\"}"
        );
    }
}
