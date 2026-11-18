//! The audio hand-off contract between the platform recorder and the Rust core.
//!
//! The recorder (Swift on iOS, a test adapter on Linux) encodes audio into chunk
//! files inside the directory the core passes to `CaptureAdapter::start`. It
//! writes each chunk under a temporary name ending in [`PARTIAL_SUFFIX`] and,
//! once the encoder has closed and flushed it, renames it to [`chunk_file_name`].
//! The rename is the hand-off: from then on the file belongs to the core, which
//! validates it, records it in SQLite, uploads it, and deletes it after the
//! server's transcript for that segment arrives.
//!
//! No audio and no file path crosses the bridge as a function argument, so a
//! chunk finished while the core is busy (for example, in the middle of a pause)
//! cannot race it: the core picks the file up on its next scan.
//!
//! Rules the recorder must follow:
//! - one segment ID per capture start; never reuse it;
//! - positions start at 0 and increase by 1, renamed in that order;
//! - `started_at_ms`/`ended_at_ms` are wall-clock capture times of the audio in the
//!   file, not file-system times;
//! - `CaptureAdapter::stop` returns only after every chunk, including the trailing
//!   partial one, has been renamed into place;
//! - never modify a file after renaming it.
use uuid::Uuid;

pub const PARTIAL_SUFFIX: &str = ".partial";
/// The server's default object-size cap (`MAX_AUDIO_BYTES`).
pub const MAX_CHUNK_BYTES: u64 = 16 * 1024 * 1024;
/// Longest audio one chunk may span. The handoff targets 30–60 second chunks.
pub const MAX_CHUNK_MS: i64 = 5 * 60_000;
/// Slack between control taps (pause/resume) and the recorder's audio clock.
pub const BOUNDARY_TOLERANCE_MS: i64 = 2_000;

const CONTAINERS: &[(&str, &str)] = &[
    ("wav", "audio/wav"),
    ("m4a", "audio/mp4"),
    ("ogg", "audio/ogg"),
    ("mp3", "audio/mpeg"),
];

/// `<segment>_<position>_<started_at_ms>_<ended_at_ms>.<ext>`, e.g.
/// `0198…_3_1763500000000_1763500060000.m4a`. Returns `None` for an unsupported
/// content type or an inverted time range.
pub fn chunk_file_name(
    segment: Uuid,
    position: u32,
    started_at_ms: i64,
    ended_at_ms: i64,
    content_type: &str,
) -> Option<String> {
    let ext = CONTAINERS.iter().find(|(_, t)| *t == content_type)?.0;
    (started_at_ms >= 0 && ended_at_ms >= started_at_ms)
        .then(|| format!("{segment}_{position}_{started_at_ms}_{ended_at_ms}.{ext}"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChunkName {
    pub segment: Uuid,
    pub position: u32,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub content_type: &'static str,
}

/// Parse only the canonical form [`chunk_file_name`] produces.
pub(crate) fn parse_chunk_name(name: &str) -> Option<ChunkName> {
    let (stem, ext) = name.rsplit_once('.')?;
    let content_type = CONTAINERS.iter().find(|(e, _)| *e == ext)?.1;
    let mut parts = stem.split('_');
    let segment = Uuid::parse_str(parts.next()?).ok()?;
    let position = digits(parts.next()?)?;
    let started_at_ms = digits(parts.next()?)?;
    let ended_at_ms = digits(parts.next()?)?;
    let parsed = ChunkName {
        segment,
        position: u32::try_from(position).ok()?,
        started_at_ms: i64::try_from(started_at_ms).ok()?,
        ended_at_ms: i64::try_from(ended_at_ms).ok()?,
        content_type,
    };
    // One spelling per chunk: anything else is not ours to interpret.
    (parts.next().is_none()
        && !segment.is_nil()
        && chunk_file_name(
            segment,
            parsed.position,
            parsed.started_at_ms,
            parsed.ended_at_ms,
            content_type,
        )
        .as_deref()
            == Some(name))
    .then_some(parsed)
}
fn digits(s: &str) -> Option<u64> {
    (!s.is_empty() && s.len() <= 19 && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// Structural check that the bytes are a complete file of the declared
/// container. This is not decoding; it catches a mislabeled file, a truncated
/// WAV, and an MP4 whose index (`moov`) was never written because the encoder
/// did not finish, which is the usual result of a killed recorder.
pub(crate) fn container_matches(content_type: &str, bytes: &[u8]) -> bool {
    match content_type {
        "audio/wav" => wav_complete(bytes),
        "audio/mp4" => mp4_complete(bytes),
        "audio/ogg" => bytes.len() > 27 && bytes.starts_with(b"OggS") && bytes[4] == 0,
        "audio/mpeg" => {
            bytes.starts_with(b"ID3")
                || (bytes.len() > 4 && bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0)
        }
        _ => false,
    }
}
fn u32_le(b: &[u8], at: usize) -> Option<u64> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as u64)
}
fn wav_complete(b: &[u8]) -> bool {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return false;
    }
    let mut at = 12usize;
    let mut format = false;
    while let (Some(id), Some(size)) = (b.get(at..at + 4), u32_le(b, at + 4)) {
        let body = at + 8;
        let Some(end) = body.checked_add(size as usize) else {
            return false;
        };
        if end > b.len() {
            return false;
        }
        match id {
            b"fmt " => format = size >= 16,
            b"data" => return format && size > 0,
            _ => {}
        }
        // RIFF chunks are padded to an even length.
        at = end + (size as usize & 1);
    }
    false
}
fn mp4_complete(b: &[u8]) -> bool {
    let (mut at, mut first, mut moov, mut mdat) = (0usize, true, false, false);
    while at < b.len() {
        let Some(size) = b
            .get(at..at + 4)
            .map(|s| u32::from_be_bytes(s.try_into().unwrap()) as u64)
        else {
            return false;
        };
        let Some(kind) = b.get(at + 4..at + 8) else {
            return false;
        };
        let size = match size {
            0 => (b.len() - at) as u64,
            1 => match b.get(at + 8..at + 16) {
                Some(s) => u64::from_be_bytes(s.try_into().unwrap()),
                None => return false,
            },
            n => n,
        };
        if size < 8 || at as u64 + size > b.len() as u64 || (first && kind != b"ftyp") {
            return false;
        }
        moov |= kind == b"moov";
        mdat |= kind == b"mdat";
        first = false;
        at += size as usize;
    }
    moov && mdat
}

/// A 16 kHz mono 16-bit PCM WAV around `samples`, for tests and test adapters.
#[cfg(any(test, feature = "test-support"))]
pub fn synthetic_wav(samples: &[u8]) -> Vec<u8> {
    let mut w = b"RIFF".to_vec();
    w.extend_from_slice(&(36 + samples.len() as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&[1, 0, 1, 0, 0x80, 0x3E, 0, 0, 0, 0x7D, 0, 0, 2, 0, 16, 0]);
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    w.extend_from_slice(samples);
    w
}

#[cfg(test)]
mod tests {
    use super::*;
    use synthetic_wav as wav;

    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = ((8 + body.len()) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
    }

    #[test]
    fn names_round_trip_in_one_canonical_spelling() {
        let s = Uuid::now_v7();
        let name = chunk_file_name(s, 7, 1_000, 61_000, "audio/mp4").unwrap();
        assert!(name.ends_with("_7_1000_61000.m4a"));
        let parsed = parse_chunk_name(&name).unwrap();
        assert_eq!(
            (parsed.segment, parsed.position, parsed.content_type),
            (s, 7, "audio/mp4")
        );
        for bad in [
            format!("{s}_07_1000_61000.m4a"),
            format!("{}_7_1000_61000.m4a", s.to_string().to_uppercase()),
            format!("{s}_7_1000_61000.aac"),
            format!("{s}_7_1000_61000.m4a.partial"),
            format!("{s}_-7_1000_61000.m4a"),
            format!("{s}_7_61000_1000.m4a"),
            format!("{s}_7_1000_61000_9.m4a"),
            format!("{}_7_1000_61000.m4a", Uuid::nil()),
        ] {
            assert_eq!(parse_chunk_name(&bad), None, "{bad}");
        }
        assert_eq!(chunk_file_name(s, 0, 2, 1, "audio/wav"), None);
        assert_eq!(chunk_file_name(s, 0, 1, 2, "audio/opus"), None);
    }

    #[test]
    fn containers_must_be_complete_and_match_the_label() {
        let good = wav(&[0, 1, 2, 3]);
        assert!(container_matches("audio/wav", &good));
        assert!(!container_matches("audio/wav", &good[..good.len() - 1]));
        assert!(!container_matches("audio/wav", &wav(&[])));
        assert!(!container_matches("audio/mp4", &good));

        let mut m4a = mp4_box(b"ftyp", b"M4A \0\0\0\0");
        m4a.extend(mp4_box(b"mdat", &[1, 2, 3]));
        assert!(
            !container_matches("audio/mp4", &m4a),
            "no index: unplayable"
        );
        m4a.extend(mp4_box(b"moov", &[0; 8]));
        assert!(container_matches("audio/mp4", &m4a));
        assert!(!container_matches("audio/mp4", &m4a[..m4a.len() - 2]));
        assert!(!container_matches("audio/wav", &m4a));

        assert!(container_matches(
            "audio/ogg",
            &[b"OggS".as_slice(), &[0; 28]].concat()
        ));
        assert!(container_matches("audio/mpeg", b"ID3\x04rest"));
        assert!(!container_matches("audio/opus", &good));
    }
}
