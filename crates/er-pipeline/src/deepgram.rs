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
//!   patient. This minimizes metadata; names and identifying details can still
//!   be spoken in the audio. Both the audio and the response remain PHI.
//! - `mip_opt_out=true` is sent on every call, so the audio is excluded from
//!   Deepgram's Model Improvement Program by request rather than by somebody
//!   remembering to tick a box in a console (E2 still signs the BAA — a query
//!   parameter is not a legal instrument).
//! - The API key travels in a sensitive `Authorization` header, never in the
//!   URL. [`DeepgramTranscriber`]'s `Debug` redacts it. Infrastructure must also
//!   avoid header/body logging; header sensitivity is not proxy configuration.
//! - Errors carry status codes and fixed descriptions only. Vendor bodies and
//!   transport diagnostics are untrusted and may contain PHI or credentials.
//! - HTTPS is required and redirects are refused: even a 307/308 must not
//!   replay the audio to another destination. The BAA, project configuration,
//!   egress/proxy policy, and retention controls still need external evidence.

use std::fmt;
use std::time::{Duration, Instant};

use reqwest::header::HeaderValue;
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
/// The detailed S6 plan chose 60 s; the main guide §6.4 says 30 s. Retain the
/// bounded 60 s policy pending synthetic latency measurements. This calculation
/// covers STT only, not S3/database work or queue lease renewal in S7.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(60);

/// One call plus two retries (`phase4-implementation-guide.md` §6.4). The
/// *outer* three attempts are SQS's, and they absorb worker deaths; these inner
/// ones absorb network blips without paying a redelivery round trip (§C2).
const MAX_ATTEMPTS: u32 = 3;

/// First backoff, doubling per retry: 500 ms, then 1 s. A fixed schedule keeps
/// the existing retry budget predictable. S7's concurrency policy must also
/// account for provider rate limits when multiple workers are active.
const BACKOFF_BASE: Duration = Duration::from_millis(500);

/// Generous for a ~60 s chunk, including word timestamps and speaker turns.
/// Bound bytes before JSON parsing, including responses without Content-Length.
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

fn http_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(ATTEMPT_TIMEOUT)
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
}

/// Cloud speech-to-text via Deepgram.
///
/// The `reqwest::Client` is built once and reused: it owns the connection pool,
/// and a fresh client per call would mean a fresh TLS handshake per chunk.
pub struct DeepgramTranscriber {
    http: reqwest::Client,
    authorization: HeaderValue,
    // The public constructor always uses LISTEN_URL. Only unit tests can
    // redirect the request to a loopback fixture; no runtime endpoint override.
    #[cfg(test)]
    listen_url: String,
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

        let mut authorization = HeaderValue::from_str(&format!("Token {api_key}"))
            .map_err(|_| PipelineError::Transcription("invalid Deepgram API key header".into()))?;
        authorization.set_sensitive(true);

        let http = http_client_builder().build().map_err(|_| {
            PipelineError::Transcription("could not build the Deepgram HTTP client".into())
        })?;

        Ok(Self {
            http,
            authorization,
            #[cfg(test)]
            listen_url: LISTEN_URL.to_owned(),
        })
    }

    /// One HTTP attempt: `Ok` with the parsed JSON body on a 2xx, `Err` with a
    /// verdict on whether trying again could plausibly help.
    ///
    /// Split out from the retry loop so that "did it work" and "should we try
    /// again" are two separate questions with two separate answers, instead of
    /// one tangled `match` on a `reqwest::Response`.
    async fn send_once(&self, audio: &[u8], content_type: &str) -> Result<Value, Attempt> {
        #[cfg(not(test))]
        let listen_url = LISTEN_URL;
        #[cfg(test)]
        let listen_url = &self.listen_url;
        let response = self
            .http
            .post(listen_url)
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
                // (§C5); vendor documentation verified in the S6 review.
                ("mip_opt_out", "true"),
            ])
            // A header, not a query parameter: URLs end up in error strings,
            // proxy logs, and redirect traces. Keys must not.
            .header(reqwest::header::AUTHORIZATION, self.authorization.clone())
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
                return Err(transport_failure(&e));
            }
        };

        let status = response.status();
        if !status.is_success() {
            // Do not read or retain error bodies. Truncation does not redact
            // PHI, and waiting for an error body can also exhaust the deadline.
            return Err(Attempt {
                retryable: is_retryable_status(status.as_u16()),
                error: PipelineError::Transcription(format!("deepgram HTTP {}", status.as_u16())),
            });
        }

        let mut response = response;
        if response
            .content_length()
            .is_some_and(|len| len > MAX_RESPONSE_BYTES as u64)
        {
            return Err(response_too_large());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| transport_failure(&e))? {
            if chunk.len() > MAX_RESPONSE_BYTES - body.len() {
                return Err(response_too_large());
            }
            body.extend_from_slice(&chunk);
        }
        // Body transport failures above may recover. A complete but invalid
        // JSON document is a contract error and must not be retried locally.
        serde_json::from_slice(&body).map_err(|_| Attempt {
            retryable: false,
            error: PipelineError::Transcription(
                "deepgram returned a success status with an unreadable body".into(),
            ),
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

        // Defence in depth: S4 also rejects zero-length objects. Direct library
        // callers must not pay for a vendor request with no audio either.
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

/// Retry connection/request/body failures, including a reset during a 2xx body.
/// Timeouts fail toward the outer SQS retry with a fresh job budget.
fn transport_is_retryable(err: &reqwest::Error) -> bool {
    !err.is_timeout()
        && !err.is_builder()
        // Response::chunk reports interrupted HTTP frames as Decode errors.
        // JSON decoding happens separately and never enters this classifier.
        && (err.is_connect() || err.is_request() || err.is_body() || err.is_decode())
}

fn transport_failure(err: &reqwest::Error) -> Attempt {
    let message = if err.is_timeout() {
        "deepgram request timed out"
    } else if err.is_builder() {
        "invalid Deepgram request configuration"
    } else {
        "deepgram transport failed"
    };
    Attempt {
        retryable: transport_is_retryable(err),
        error: PipelineError::Transcription(message.into()),
    }
}

fn response_too_large() -> Attempt {
    Attempt {
        retryable: false,
        error: PipelineError::Transcription("deepgram response exceeds size limit".into()),
    }
}

/// 500 ms, then 1 s. `attempt` is zero-based.
fn backoff_for(attempt: u32) -> Duration {
    BACKOFF_BASE * 2u32.pow(attempt)
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
/// Unknown fields are ignored. Known transcript fields are validated so schema
/// errors cannot silently remove clinical text. Serde structs also ignore
/// unknown fields by default; Value is retained to keep this small parser local.
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
    let flat = alternative
        .and_then(|alt| alt.get("transcript"))
        .and_then(Value::as_str);
    let text = match utterances {
        Some(utterances) if !utterances.is_empty() => match format_utterances(utterances) {
            Some(turns) if !turns.is_empty() => Some(turns),
            Some(turns) => Some(flat.map(str::to_owned).unwrap_or(turns)),
            // An incomplete array is never accepted as a partial transcript or
            // silence. Prefer a complete, nonblank flat fallback or fail.
            None => flat
                .filter(|text| !text.trim().is_empty())
                .map(str::to_owned),
        },
        _ => flat.map(str::to_owned),
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
            "deepgram returned a success status with no transcript: \
             missing or malformed transcript fields"
                .into(),
        ));
    };

    Ok(TranscriptResult {
        text: text.trim_end().to_string(),
        confidence: alternative
            .and_then(|alt| alt.get("confidence"))
            .and_then(Value::as_f64)
            .filter(|c| (0.0..=1.0).contains(c))
            .map(|c| c as f32),
        // metadata.duration is Deepgram's own measurement of the audio, and it
        // is the value S7's worker will write to `audio_chunks.duration_ms` — the
        // column Step 7's usage log will bill from (D6.13). Absent or invalid,
        // it is 0.0 rather than an error: a missing meter reading is not a
        // reason to throw away a good transcript.
        audio_duration_secs: v
            .get("metadata")
            .and_then(|m| m.get("duration"))
            .and_then(Value::as_f64)
            .filter(|d| *d >= 0.0 && (*d as f32).is_finite())
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
fn format_utterances(utterances: &[Value]) -> Option<String> {
    let lines = utterances
        .iter()
        .map(|utterance| {
            let text = utterance.get("transcript").and_then(Value::as_str)?;
            Some((utterance, text.trim()))
        })
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .filter(|(_, text)| !text.is_empty())
        .map(
            |(utterance, text)| match utterance.get("speaker").and_then(Value::as_u64) {
                Some(speaker) => format!("Speaker {speaker}: {text}"),
                None => text.to_string(),
            },
        )
        .collect::<Vec<_>>()
        .join("\n");
    Some(lines)
}

#[cfg(test)]
mod http_tests;

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
    fn unusable_or_partial_utterances_preserve_the_complete_flat_transcript() {
        for utterances in [
            serde_json::json!([{}]),
            serde_json::json!([{ "transcript": null }]),
            serde_json::json!([{ "transcript": "  " }]),
            serde_json::json!([{ "transcript": "Only part of the answer." }, {}]),
        ] {
            let mut response = flat_response();
            response["results"]["utterances"] = utterances;
            let result = parse_deepgram_response(&response).unwrap();
            assert_eq!(
                result.text,
                "Patient reports chest pain radiating to the left arm."
            );
        }
    }

    #[test]
    fn malformed_utterances_without_a_flat_fallback_are_not_silence() {
        for utterances in [
            serde_json::json!([{}]),
            serde_json::json!([{ "transcript": 12 }]),
            serde_json::json!([{ "transcript": "Valid turn." }, {}]),
        ] {
            assert!(
                parse_deepgram_response(&serde_json::json!({
                    "results": { "utterances": utterances }
                }))
                .is_err()
            );
        }
    }

    #[test]
    fn incomplete_turns_cannot_hide_behind_an_empty_flat_transcript() {
        let response = serde_json::json!({ "results": {
            "utterances": [{ "transcript": "Synthetic speech." }, {}],
            "channels": [{ "alternatives": [{ "transcript": "" }] }]
        } });
        assert!(parse_deepgram_response(&response).is_err());
    }

    #[test]
    fn well_formed_blank_turns_are_silence() {
        let response = serde_json::json!({ "results": {
            "utterances": [{ "speaker": 0, "transcript": "  " }]
        } });
        assert_eq!(parse_deepgram_response(&response).unwrap().text, "");
    }

    #[test]
    fn invalid_numeric_metadata_cannot_escape_into_persistence() {
        for invalid in [-1.0, 1e100] {
            let mut response = flat_response();
            response["metadata"]["duration"] = serde_json::json!(invalid);
            response["results"]["channels"][0]["alternatives"][0]["confidence"] =
                serde_json::json!(invalid);
            let result = parse_deepgram_response(&response).unwrap();
            assert_eq!(result.audio_duration_secs, 0.0);
            assert_eq!(result.confidence, None);
        }
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
        // Pin the STT budget against the documented queue lease. This copied
        // constant does not inspect the configured queue or budget S3/DB work;
        // S7 must test those together and handle at-least-once delivery.
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

        assert_eq!(
            error.to_string(),
            "transcription error: no content type supplied for the audio"
        );
    }

    #[tokio::test]
    async fn empty_audio_is_rejected_before_any_network_call() {
        let stt = DeepgramTranscriber::new("dg_not_a_real_key").unwrap();

        let error = stt
            .transcribe(b"", "audio/wav")
            .await
            .expect_err("a zero-byte object is a completed upload of nothing");

        assert_eq!(
            error.to_string(),
            "transcription error: refusing to transcribe an empty audio buffer"
        );
    }

    #[tokio::test]
    async fn the_deepgram_arm_of_the_dispatch_enum_is_reachable() {
        // The enum's Deepgram variant exists and satisfies the trait. The
        // loopback HTTP suite covers successful dispatch without a live key;
        // this test proves its preconditions travel through the enum too.
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
    fn malformed_key_headers_are_rejected_without_disclosing_the_key() {
        let error = DeepgramTranscriber::new("synthetic-secret\r\nInjected: value").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("invalid Deepgram API key header")
        );
        assert!(!format!("{error:?}").contains("synthetic-secret"));
        let stt = DeepgramTranscriber::new("synthetic-secret").unwrap();
        assert!(stt.authorization.is_sensitive());
        assert!(!format!("{:?}", stt.authorization).contains("synthetic-secret"));
    }
}
