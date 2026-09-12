//! Audio stays in memory, with independent metadata, byte-count and time limits.
use tokio::io::{AsyncRead, AsyncReadExt};

pub const ALLOWED_AUDIO_TYPES: &[&str] = &["audio/wav", "audio/ogg", "audio/mp4", "audio/mpeg"];

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct AudioError(pub &'static str);

pub fn validate_metadata(
    length: Option<i64>,
    actual_type: Option<&str>,
    declared_type: &str,
    max_bytes: usize,
    require_length: bool,
) -> Result<(), AudioError> {
    if !ALLOWED_AUDIO_TYPES.contains(&declared_type) || actual_type != Some(declared_type) {
        return Err(AudioError("chunk_content_type_mismatch"));
    }
    match length {
        Some(n) if n <= 0 => Err(AudioError("chunk_empty")),
        Some(n) if n as u64 > max_bytes as u64 => Err(AudioError("chunk_too_large")),
        None if require_length => Err(AudioError("chunk_length_missing")),
        _ => Ok(()),
    }
}

/// Read no more than the cap plus one byte, even with absent or dishonest length
/// metadata. A small stack buffer avoids accepting an arbitrarily large frame.
pub async fn read_bounded(
    reader: impl AsyncRead + Unpin,
    max_bytes: usize,
) -> Result<Vec<u8>, AudioError> {
    let mut reader = reader.take(max_bytes as u64 + 1);
    let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
    let mut buffer = [0_u8; 8192];
    loop {
        let n = reader
            .read(&mut buffer)
            .await
            .map_err(|_| AudioError("audio_download_failed"))?;
        if n == 0 {
            break;
        }
        if n > max_bytes.saturating_sub(bytes.len()) {
            return Err(AudioError("chunk_too_large"));
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    if bytes.is_empty() {
        return Err(AudioError("chunk_empty"));
    }
    Ok(bytes)
}

pub async fn download(
    client: &aws_sdk_s3::Client,
    bucket: &str,
    key: &str,
    declared_type: &str,
    limits: &crate::config::WorkerLimits,
) -> Result<Vec<u8>, AudioError> {
    tokio::time::timeout(limits.download_timeout, async {
        let object = client
            .get_object()
            .bucket(bucket)
            .key(key)
            .send()
            .await
            .map_err(|_| AudioError("audio_download_failed"))?;
        let length = object.content_length();
        validate_metadata(
            length,
            object.content_type(),
            declared_type,
            limits.max_audio_bytes,
            false,
        )?;
        let bytes = read_bounded(object.body.into_async_read(), limits.max_audio_bytes).await?;
        if length.is_some_and(|n| n as u64 != bytes.len() as u64) {
            return Err(AudioError("chunk_length_mismatch"));
        }
        Ok(bytes)
    })
    .await
    .map_err(|_| AudioError("audio_download_timeout"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_is_not_trusted() {
        for (length, actual, declared, require, code) in [
            (
                Some(5),
                Some("audio/wav"),
                "audio/wav",
                true,
                "chunk_too_large",
            ),
            (Some(0), Some("audio/wav"), "audio/wav", true, "chunk_empty"),
            (
                None,
                Some("audio/wav"),
                "audio/wav",
                true,
                "chunk_length_missing",
            ),
            (
                Some(1),
                Some("text/plain"),
                "audio/wav",
                true,
                "chunk_content_type_mismatch",
            ),
            (
                Some(1),
                None,
                "audio/wav",
                false,
                "chunk_content_type_mismatch",
            ),
            (
                Some(1),
                Some("text/plain"),
                "text/plain",
                false,
                "chunk_content_type_mismatch",
            ),
        ] {
            assert_eq!(
                validate_metadata(length, actual, declared, 4, require)
                    .unwrap_err()
                    .0,
                code
            );
        }
        assert!(validate_metadata(None, Some("audio/wav"), "audio/wav", 4, false).is_ok());
    }
    #[tokio::test]
    async fn streaming_cap_does_not_depend_on_metadata() {
        assert_eq!(read_bounded(&b"1234"[..], 4).await.unwrap(), b"1234");
        assert_eq!(
            read_bounded(&b"12345"[..], 4).await.unwrap_err().0,
            "chunk_too_large"
        );
        assert_eq!(
            read_bounded(&b""[..], 4).await.unwrap_err().0,
            "chunk_empty"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_unknown_length_stalled_stream_can_be_cancelled() {
        let (reader, _writer) = tokio::io::duplex(8);
        let started = tokio::time::Instant::now();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(20), read_bounded(reader, 4))
                .await
                .is_err()
        );
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(20));
    }
}
