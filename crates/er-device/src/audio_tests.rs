//! Step 4 audio hand-off on Linux: a file-writing test recorder stands in for
//! the iOS one. Synthetic WAVs are tiny; their names carry the capture times.
use super::*;
use crate::audio::{PARTIAL_SUFFIX, chunk_file_name, synthetic_wav};
use chrono::Utc;
use er_store::audio::ChunkState;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use uuid::Uuid;

struct TestDb(PathBuf);
impl TestDb {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("er-audio-{}.sqlite", Uuid::now_v7())))
    }
    fn config(&self) -> DeviceConfig {
        DeviceConfig {
            database_path: self.0.to_str().unwrap().into(),
            account_scope: "synthetic-account".into(),
        }
    }
    fn sql(&self, sql: &str) {
        rusqlite::Connection::open(&self.0)
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }
}
impl Drop for TestDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", self.0.display(), suffix));
        }
        let _ = std::fs::remove_dir_all(format!("{}.audio", self.0.display()));
    }
}

#[derive(Default)]
struct RecorderState {
    dir: Option<PathBuf>,
    segment: Option<Uuid>,
    next_position: u32,
    trailing: Vec<(i64, i64)>,
    stops: usize,
}
/// Writes chunk files the way the iOS recorder must: `.partial`, then rename.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<RecorderState>>);
impl Recorder {
    fn adapter(&self) -> Box<dyn CaptureAdapter> {
        Box::new(self.clone())
    }
    fn segment(&self) -> Uuid {
        self.0.lock().unwrap().segment.unwrap()
    }
    /// Finish the next chunk of the current segment.
    fn chunk(&self, start: i64, end: i64) -> String {
        let mut s = self.0.lock().unwrap();
        let position = s.next_position;
        s.next_position += 1;
        let (dir, segment) = (s.dir.clone().unwrap(), s.segment.unwrap());
        drop(s);
        write_chunk(
            &dir,
            segment,
            position,
            start,
            end,
            &synthetic_wav(&[1, 2, 3, 4]),
        )
    }
    /// The chunk still being encoded when stop() is called.
    fn trailing(&self, start: i64, end: i64) {
        self.0.lock().unwrap().trailing.push((start, end));
    }
}
impl CaptureAdapter for Recorder {
    fn available(&self) -> bool {
        true
    }
    fn start(&mut self, _: Uuid, segment: Uuid, _: bool, directory: &Path) -> Result<()> {
        let mut s = self.0.lock().unwrap();
        s.dir = Some(directory.to_path_buf());
        s.segment = Some(segment);
        s.next_position = 0;
        Ok(())
    }
    fn stop(&mut self) -> Result<()> {
        let trailing = std::mem::take(&mut self.0.lock().unwrap().trailing);
        if self.0.lock().unwrap().segment.is_some() {
            for (start, end) in trailing {
                self.chunk(start, end);
            }
        }
        self.0.lock().unwrap().stops += 1;
        Ok(())
    }
}
fn write_chunk(
    dir: &Path,
    segment: Uuid,
    position: u32,
    start: i64,
    end: i64,
    bytes: &[u8],
) -> String {
    let name = chunk_file_name(segment, position, start, end, "audio/wav").unwrap();
    let partial = dir.join(format!("{name}{PARTIAL_SUFFIX}"));
    std::fs::write(&partial, bytes).unwrap();
    std::fs::rename(partial, dir.join(&name)).unwrap();
    name
}

const MIN: i64 = 60_000;
fn now() -> i64 {
    Utc::now().timestamp_millis()
}
/// A shift, one patient and granted ambient consent, all at `t0`.
fn ready(core: &mut DeviceCore, t0: i64) -> Uuid {
    core.start_shift(t0).unwrap();
    let p = core::id(
        &core
            .add_patient("Synthetic complaint".into(), Some("Room 4".into()), t0)
            .unwrap(),
    )
    .unwrap();
    core.select_patient(Some(p), t0).unwrap();
    core.set_consent(p, ConsentStatus::Granted, "synthetic-v1".into(), 0, t0)
        .unwrap();
    p
}
fn accepted(rows: &[er_store::audio::AudioChunk]) -> Vec<(u32, u32, bool)> {
    rows.iter()
        .filter_map(|c| c.chunk_index.map(|i| (c.position, i, c.is_final)))
        .collect()
}

#[test]
fn pause_seals_ordered_chunks_into_one_final_segment() {
    let db = TestDb::new();
    let recorder = Recorder::default();
    let mut core = DeviceCore::open_with_capture(db.config(), recorder.adapter()).unwrap();
    let t0 = now() - 30 * MIN;
    let p = ready(&mut core, t0);
    core.start_capture(p, t0 + 1_000).unwrap();
    let segment = recorder.segment();
    recorder.chunk(t0 + 1_000, t0 + MIN);
    recorder.chunk(t0 + MIN, t0 + 2 * MIN);
    let view = core.collect_audio().unwrap();
    assert_eq!(view.awaiting_upload, 2);
    // Still recording: nothing is final, so nothing may upload yet.
    assert!(
        core.audio_rows_for_test(segment)
            .iter()
            .all(|c| !c.is_final)
    );

    recorder.trailing(t0 + 2 * MIN, t0 + 2 * MIN + 30_000);
    core.pause(p, t0 + 2 * MIN + 30_000, "user_pause".into())
        .unwrap();
    let rows = core.audio_rows_for_test(segment);
    assert_eq!(
        accepted(&rows),
        [(0, 0, false), (1, 1, false), (2, 2, true)]
    );
    assert!(
        rows.iter()
            .all(|c| c.patient_id == p && c.state == ChunkState::Queued && c.sha256.len() == 64)
    );
    // Resume starts a new segment with its own indices from zero.
    core.start_capture(p, t0 + 3 * MIN).unwrap();
    let second = recorder.segment();
    assert_ne!(second, segment);
    recorder.trailing(t0 + 3 * MIN, t0 + 3 * MIN + 10_000);
    core.pause(p, t0 + 3 * MIN + 10_000, "user_pause".into())
        .unwrap();
    assert_eq!(accepted(&core.audio_rows_for_test(second)), [(0, 0, true)]);
}

#[test]
fn hour_long_trail_is_contiguous_and_waits_for_late_positions() {
    let db = TestDb::new();
    let recorder = Recorder::default();
    let mut core = DeviceCore::open_with_capture(db.config(), recorder.adapter()).unwrap();
    let t0 = now() - 70 * MIN;
    let p = ready(&mut core, t0);
    core.start_capture(p, t0).unwrap();
    let segment = recorder.segment();
    let dir = core.audio_directory().to_path_buf();
    let wav = synthetic_wav(&[0; 32]);
    let span = |n: u32| (t0 + n as i64 * MIN, t0 + (n as i64 + 1) * MIN);
    // Position 1 is renamed before position 0: the core must wait for 0.
    let (s, e) = span(1);
    write_chunk(&dir, segment, 1, s, e, &wav);
    let view = core.collect_audio().unwrap();
    assert_eq!((view.awaiting_upload, view.not_yet_collected), (0, 1));
    for n in [0, 2, 3, 5, 4] {
        let (s, e) = span(n);
        write_chunk(&dir, segment, n, s, e, &wav);
        core.collect_audio().unwrap();
    }
    assert_eq!(core.audio_rows_for_test(segment).len(), 6);
    for n in 6..59 {
        let (s, e) = span(n);
        write_chunk(&dir, segment, n, s, e, &wav);
        if n % 10 == 0 {
            core.collect_audio().unwrap();
        }
    }
    // The 60th minute is the trailing chunk flushed by stop().
    let (s, e) = span(59);
    write_chunk(&dir, segment, 59, s, e, &wav);
    core.pause(p, t0 + 60 * MIN, "user_pause".into()).unwrap();

    let rows = core.audio_rows_for_test(segment);
    assert_eq!(rows.len(), 60);
    for (n, c) in rows.iter().enumerate() {
        assert_eq!((c.position, c.chunk_index), (n as u32, Some(n as u32)));
        assert_eq!(c.is_final, n == 59);
        assert_eq!((c.started_at_ms, c.ended_at_ms), span(n as u32));
        assert_eq!(c.patient_id, p);
    }
    let covered: i64 = rows.iter().map(|c| c.ended_at_ms - c.started_at_ms).sum();
    assert_eq!(covered, 60 * MIN);
    let view = core.audio_status().unwrap();
    assert_eq!(
        (view.awaiting_upload, view.missing_chunks, view.rejected),
        (60, 0, 0)
    );
}

#[test]
fn no_chunk_crosses_a_patient_or_pause_boundary() {
    let db = TestDb::new();
    let recorder = Recorder::default();
    let mut core = DeviceCore::open_with_capture(db.config(), recorder.adapter()).unwrap();
    let t0 = now() - 30 * MIN;
    let a = ready(&mut core, t0);
    let b = core::id(&core.add_patient("Second".into(), None, t0).unwrap()).unwrap();
    core.start_capture(a, t0 + MIN).unwrap();
    let sa = recorder.segment();
    recorder.chunk(t0 + MIN, t0 + 2 * MIN);
    // Switching patients is a hard boundary for A's segment. The trailing chunk
    // ends 1.5 s after the tap: inside the clock tolerance.
    recorder.trailing(t0 + 2 * MIN, t0 + 2 * MIN + 31_500);
    core.select_patient(Some(b), t0 + 2 * MIN + 30_000).unwrap();
    // A chunk for A that appears after A was sealed is refused, not appended.
    let dir = core.audio_directory().to_path_buf();
    let late = write_chunk(
        &dir,
        sa,
        2,
        t0 + 2 * MIN + 31_500,
        t0 + 3 * MIN,
        &synthetic_wav(&[9; 4]),
    );

    core.set_mode(b, true).unwrap();
    core.start_capture(b, t0 + 4 * MIN).unwrap();
    let sb = recorder.segment();
    // Audio stamped before B's segment began belongs to no one: refused.
    recorder.chunk(t0 + 3 * MIN, t0 + 4 * MIN + 30_000);
    recorder.chunk(t0 + 4 * MIN + 30_000, t0 + 5 * MIN);
    // Beyond the pause tap plus tolerance: refused.
    recorder.trailing(t0 + 5 * MIN, t0 + 6 * MIN);
    core.pause(b, t0 + 5 * MIN + 20_000, "user_pause".into())
        .unwrap();
    core.collect_audio().unwrap();

    let ra = core.audio_rows_for_test(sa);
    assert_eq!(accepted(&ra), [(0, 0, false), (1, 1, true)]);
    assert!(ra.iter().all(|c| c.patient_id == a));
    let refused = ra.iter().find(|c| c.file_name == late).unwrap();
    assert_eq!(
        (refused.state, refused.reason.as_deref()),
        (ChunkState::Rejected, Some("late_chunk"))
    );
    let rb = core.audio_rows_for_test(sb);
    assert!(rb.iter().all(|c| c.patient_id == b));
    assert_eq!(accepted(&rb), [(1, 0, true)]);
    let reasons: Vec<_> = rb.iter().filter_map(|c| c.reason.as_deref()).collect();
    assert_eq!(reasons, ["outside_segment", "outside_segment"]);
}

#[test]
fn files_are_checked_before_the_core_takes_ownership() {
    let db = TestDb::new();
    let recorder = Recorder::default();
    let mut core = DeviceCore::open_with_capture(db.config(), recorder.adapter()).unwrap();
    let t0 = now() - 30 * MIN;
    let p = ready(&mut core, t0);
    core.start_capture(p, t0).unwrap();
    let segment = recorder.segment();
    let dir = core.audio_directory().to_path_buf();
    let mut mp4 = vec![0, 0, 0, 16];
    mp4.extend_from_slice(b"ftypM4A \0\0\0\0");
    write_chunk(&dir, segment, 0, t0, t0 + 1_000, &mp4);
    write_chunk(&dir, segment, 1, t0 + 1_000, t0 + 2_000, b"");
    let outside = std::env::temp_dir().join(format!("er-audio-outside-{}.wav", Uuid::now_v7()));
    std::fs::write(&outside, synthetic_wav(&[1; 4])).unwrap();
    let link = chunk_file_name(segment, 2, t0 + 2_000, t0 + 3_000, "audio/wav").unwrap();
    std::os::unix::fs::symlink(&outside, dir.join(&link)).unwrap();
    // Six minutes in one file is longer than any chunk the recorder may cut.
    write_chunk(
        &dir,
        segment,
        3,
        t0 + 3_000,
        t0 + 3_000 + 6 * MIN,
        &synthetic_wav(&[1; 4]),
    );
    write_chunk(
        &dir,
        segment,
        4,
        t0 + 6 * MIN,
        t0 + 7 * MIN,
        &synthetic_wav(&[1; 4]),
    );
    // Not chunk files: never interpreted, uploaded or deleted.
    std::fs::write(dir.join("notes.txt"), b"x").unwrap();
    let stranger = chunk_file_name(Uuid::now_v7(), 0, t0, t0 + 1, "audio/wav").unwrap();
    std::fs::write(dir.join(&stranger), synthetic_wav(&[1; 4])).unwrap();
    std::fs::write(dir.join(format!("x{PARTIAL_SUFFIX}")), b"half").unwrap();
    core.pause(p, t0 + 7 * MIN, "user_pause".into()).unwrap();

    let rows = core.audio_rows_for_test(segment);
    let reasons: Vec<_> = rows.iter().map(|c| c.reason.as_deref()).collect();
    assert_eq!(
        reasons,
        [
            Some("container_mismatch"),
            Some("chunk_empty"),
            Some("not_a_file"),
            Some("chunk_too_long"),
            None,
        ]
    );
    assert_eq!(accepted(&rows), [(4, 0, true)]);
    let view = core.audio_status().unwrap();
    assert_eq!(
        (view.rejected, view.unclaimed_files, view.unfinished_files),
        (4, 2, 1)
    );
    // A second file claiming an already-taken position is never collected.
    write_chunk(
        &dir,
        segment,
        4,
        t0 + 6 * MIN,
        t0 + 6 * MIN + 1,
        &synthetic_wav(&[2; 4]),
    );
    let view = core.collect_audio().unwrap();
    assert_eq!(
        (view.not_yet_collected, view.unclaimed_files, view.rejected),
        (0, 3, 4)
    );
    assert!(dir.join("notes.txt").exists() && dir.join(stranger).exists());
    std::fs::remove_file(outside).unwrap();
}

#[test]
fn consent_revocation_seals_at_the_revocation_and_blocks_restart() {
    let db = TestDb::new();
    let recorder = Recorder::default();
    let mut core = DeviceCore::open_with_capture(db.config(), recorder.adapter()).unwrap();
    let t0 = now() - 30 * MIN;
    let p = ready(&mut core, t0);
    core.start_capture(p, t0).unwrap();
    let segment = recorder.segment();
    recorder.chunk(t0, t0 + MIN);
    recorder.trailing(t0 + MIN, t0 + MIN + 15_000);
    core.set_consent(
        p,
        ConsentStatus::Revoked,
        "synthetic-v1".into(),
        1,
        t0 + MIN + 15_000,
    )
    .unwrap();
    assert_eq!(recorder.0.lock().unwrap().stops, 1);
    assert_eq!(
        accepted(&core.audio_rows_for_test(segment)),
        [(0, 0, false), (1, 1, true)]
    );
    assert_eq!(
        core.start_capture(p, t0 + 2 * MIN),
        Err(DeviceError::InvalidState)
    );
    // Solo dictation needs no patient consent and gets its own segment.
    core.set_mode(p, true).unwrap();
    core.start_capture(p, t0 + 2 * MIN).unwrap();
    let solo = recorder.segment();
    recorder.trailing(t0 + 2 * MIN, t0 + 2 * MIN + 5_000);
    core.pause(p, t0 + 2 * MIN + 5_000, "user_pause".into())
        .unwrap();
    assert_eq!(accepted(&core.audio_rows_for_test(solo)), [(0, 0, true)]);
}

#[test]
fn interrupted_capture_recovers_at_the_last_chunk_handed_over() {
    let db = TestDb::new();
    let recorder = Recorder::default();
    let mut core = DeviceCore::open_with_capture(db.config(), recorder.adapter()).unwrap();
    let t0 = now() - 30 * MIN;
    let p = ready(&mut core, t0);
    core.start_capture(p, t0).unwrap();
    let segment = recorder.segment();
    recorder.chunk(t0, t0 + MIN);
    core.collect_audio().unwrap();
    // Finished but never collected, then the process dies mid-chunk.
    let last_end = t0 + MIN + 40_000;
    recorder.chunk(t0 + MIN, last_end);
    std::fs::write(
        core.audio_directory()
            .join(format!("{segment}_2{PARTIAL_SUFFIX}")),
        b"half",
    )
    .unwrap();
    drop(core);

    let mut core = DeviceCore::open_with_capture(db.config(), recorder.adapter()).unwrap();
    assert_eq!(
        core.view().patients[0].recording.status,
        RecorderStatus::Interrupted
    );
    // Open picked up the finished chunk, but nothing is final until recovery.
    let rows = core.audio_rows_for_test(segment);
    assert_eq!(accepted(&rows), [(0, 0, false), (1, 1, false)]);
    assert_eq!(
        core.reconcile_interrupted(p, t0 + MIN),
        Err(DeviceError::InvalidInput)
    );
    assert_eq!(core.recover_interrupted(p).unwrap(), last_end);
    assert_eq!(
        accepted(&core.audio_rows_for_test(segment)),
        [(0, 0, false), (1, 1, true)]
    );
    let view = core.audio_status().unwrap();
    assert_eq!(view.unfinished_files, 1);
    assert_eq!(
        core.view().patients[0].recording.status,
        RecorderStatus::Paused
    );
}

#[test]
fn local_audio_is_deleted_only_after_the_server_transcript_arrives() {
    let db = TestDb::new();
    let recorder = Recorder::default();
    let mut core = DeviceCore::open_with_capture(db.config(), recorder.adapter()).unwrap();
    let t0 = now() - 30 * MIN;
    let p = ready(&mut core, t0);
    core.start_capture(p, t0).unwrap();
    let segment = recorder.segment();
    let name = recorder.chunk(t0, t0 + MIN);
    core.pause(p, t0 + MIN, "user_pause".into()).unwrap();
    let file = core.audio_directory().join(&name);
    drop(core);

    // The server accepted the chunk, but has not transcribed it yet.
    db.sql("UPDATE device_audio_chunks SET state='completed'");
    let core = DeviceCore::open_with_capture(db.config(), recorder.adapter()).unwrap();
    assert!(file.exists());
    assert_eq!(core.audio_status().unwrap().awaiting_transcript, 1);
    drop(core);

    // The worker's TranscriptReady for this segment arrives in the event log.
    let event = er_domain::NoteEvent::transcript_ready(
        p,
        segment,
        "synthetic transcript".into(),
        Utc::now(),
    );
    db.sql(&format!(
        "INSERT INTO events(event_id,patient_id,event_type,payload,occurred_at) VALUES('{}','{}','TranscriptReady','{}','{}')",
        Uuid::now_v7(),
        p,
        serde_json::to_string(&event).unwrap().replace('\'', "''"),
        Utc::now().to_rfc3339()
    ));
    let core = DeviceCore::open_with_capture(db.config(), recorder.adapter()).unwrap();
    assert!(!file.exists());
    let view = core.audio_status().unwrap();
    assert_eq!((view.awaiting_transcript, view.processed), (0, 1));
}
