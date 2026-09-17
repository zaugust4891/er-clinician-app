//! The background-job vocabulary: what the API puts on the queue and what the
//! worker takes off it (Step 6 S4).
//!
//! This module is deliberately tiny and dependency-light. It is the *producer*
//! half of the §C2 contract — an HTTP handler records an obligation durably and
//! answers `200` instead of holding a connection open across a transcription —
//! and the S7 worker is the consumer half. Both halves compile against the enum
//! below, so a job the API can send is a job the worker can decode.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One unit of deferred work.
///
/// **Internally tagged** (`{"kind":"TranscribeChunk","chunk_id":"…"}`) so Step 7's
/// `GenerateMdm` can join the same queue without a protocol change, and so a
/// worker that predates a new variant rejects the message loudly — serde fails on
/// an unknown tag — rather than silently mis-decoding it as one it does know.
///
/// **PHI-free by construction (§C5).** A job carries opaque identifiers only. The
/// audio lives in object storage, the transcript lives in Postgres, and neither
/// ever passes through a queue message — so an SQS console, a DLQ dump, or a log
/// line that quotes a message body exposes nothing clinical.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Job {
    /// Fetch one uploaded chunk from S3, transcribe it, and — when the whole
    /// encounter is terminal — assemble and publish `TranscriptReady` (S7).
    TranscribeChunk { chunk_id: Uuid },
    /// Generate and persist the MDM for a patient. The organization is derived
    /// from the authenticated receipt row, never trusted from the queue.
    GenerateMdm { patient_id: Uuid, job_id: Uuid },
}

/// Put one job on a queue.
///
/// Returns `anyhow::Result` rather than `ApiError`: this is transport plumbing
/// with no opinion about HTTP status codes, and the caller is the only layer that
/// knows what a send failure means for the request it is answering.
pub async fn send_job(sqs: &aws_sdk_sqs::Client, queue_url: &str, job: &Job) -> anyhow::Result<()> {
    let body = serde_json::to_string(job).context("serialize background job")?;
    sqs.send_message()
        .queue_url(queue_url)
        .message_body(body)
        .send()
        .await
        .with_context(|| format!("failed to enqueue job on {queue_url}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The queue message is a wire format between two binaries that are deployed
    /// separately (one image, two ECS services). Pinning the JSON here means a
    /// rename cannot quietly strand messages that are already in flight.
    #[test]
    fn transcribe_chunk_is_tagged_and_carries_only_an_id() {
        let chunk_id = Uuid::parse_str("0198f1c0-0000-7000-8000-000000000001").unwrap();
        let encoded = serde_json::to_value(Job::TranscribeChunk { chunk_id }).unwrap();

        assert_eq!(
            encoded,
            serde_json::json!({
                "kind": "TranscribeChunk",
                "chunk_id": "0198f1c0-0000-7000-8000-000000000001",
            }),
            "the worker decodes this exact shape"
        );
        assert_eq!(
            encoded.as_object().unwrap().len(),
            2,
            "a job body must never grow a field that could carry clinical text"
        );

        let decoded: Job = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, Job::TranscribeChunk { chunk_id });
    }

    /// A worker must refuse a job kind it does not understand instead of
    /// guessing — the reason the enum is tagged rather than untagged.
    #[test]
    fn generate_mdm_is_tagged_and_carries_only_opaque_ids() {
        let patient_id = Uuid::parse_str("0198f1c0-0000-7000-8000-000000000001").unwrap();
        let job_id = Uuid::parse_str("0198f1c0-0000-7000-8000-000000000002").unwrap();
        let encoded = serde_json::to_value(Job::GenerateMdm { patient_id, job_id }).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!({
                "kind": "GenerateMdm",
                "patient_id": patient_id,
                "job_id": job_id,
            })
        );
        assert_eq!(encoded.as_object().unwrap().len(), 3);
    }

    #[test]
    fn an_unknown_job_kind_fails_to_decode() {
        let future_job = serde_json::json!({ "kind": "FutureJob", "job_id": "x" });
        assert!(serde_json::from_value::<Job>(future_job).is_err());
    }
}
