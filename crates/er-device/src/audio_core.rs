//! Chunk ingestion, segment sealing, interruption evidence and file release.
use super::*;
use crate::audio::{
    BOUNDARY_TOLERANCE_MS, ChunkName, MAX_CHUNK_BYTES, MAX_CHUNK_MS, PARTIAL_SUFFIX,
    container_matches, parse_chunk_name,
};
use er_store::audio::{AudioChunk, ChunkState};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::PathBuf};

/// `<database file>.audio`, beside the SQLite file, so it shares its storage and
/// file protection. Created on open; a symlink or a non-directory is refused.
pub(crate) fn audio_directory_for(database: &Path) -> Result<PathBuf> {
    let name = database.file_name().ok_or(Error::InvalidInput)?;
    let mut dir_name = name.to_os_string();
    dir_name.push(".audio");
    let dir = database.with_file_name(dir_name);
    match std::fs::symlink_metadata(&dir) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(Error::Storage),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&dir).map_err(|_| Error::Storage)?;
        }
        Err(_) => return Err(Error::Storage),
    }
    Ok(dir)
}

/// Where a segment's chunks may come from.
struct SegmentInfo {
    patient: Uuid,
    started_at_ms: i64,
    stopped_at_ms: Option<i64>,
    /// Still capturing (or stopping): a missing position may yet arrive.
    live: bool,
}

#[derive(Default)]
struct Scan {
    by_segment: BTreeMap<Uuid, Vec<(String, ChunkName)>>,
    unclaimed: u64,
    unfinished: u64,
}

impl DeviceCore {
    pub fn audio_directory(&self) -> &Path {
        &self.audio_dir
    }

    fn segments(&self) -> HashMap<Uuid, SegmentInfo> {
        let mut map = HashMap::new();
        for visit in self.state.shifts.iter().flat_map(|s| &s.visits) {
            let last = visit.segments.last().map(|s| s.id);
            for s in &visit.segments {
                map.insert(
                    s.id,
                    SegmentInfo {
                        patient: visit.patient.id,
                        started_at_ms: s.started_at_ms,
                        stopped_at_ms: s.stopped_at_ms,
                        live: s.stopped_at_ms.is_none()
                            && last == Some(s.id)
                            && matches!(
                                visit.recorder,
                                RecorderStatus::Recording | RecorderStatus::PendingStop
                            ),
                    },
                );
            }
        }
        map
    }

    fn scan_audio(&self, segments: &HashMap<Uuid, SegmentInfo>) -> Result<Scan> {
        let mut scan = Scan::default();
        for entry in std::fs::read_dir(&self.audio_dir).map_err(|_| Error::Storage)? {
            let entry = entry.map_err(|_| Error::Storage)?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                scan.unclaimed += 1;
                continue;
            };
            if name.ends_with(PARTIAL_SUFFIX) {
                scan.unfinished += 1;
                continue;
            }
            match parse_chunk_name(&name) {
                Some(parsed) if segments.contains_key(&parsed.segment) => scan
                    .by_segment
                    .entry(parsed.segment)
                    .or_default()
                    .push((name, parsed)),
                // Unknown files, including audio from a segment whose start never
                // committed, are never uploaded and never deleted here.
                _ => {
                    if !self.repo.audio_file_known(&name)? {
                        scan.unclaimed += 1;
                    }
                }
            }
        }
        Ok(scan)
    }

    /// Take ownership of every finished chunk file that is next in its segment.
    fn ingest_audio(&mut self) -> Result<()> {
        let segments = self.segments();
        let scan = self.scan_audio(&segments)?;
        let now = Utc::now().timestamp_millis();
        for (segment, mut files) in scan.by_segment {
            let info = &segments[&segment];
            files.sort_by_key(|(_, c)| c.position);
            let rows = self.repo.segment_chunks(segment)?;
            let sealed = rows.iter().any(|c| c.is_final);
            let mut next_position = rows.iter().map(|c| c.position + 1).max().unwrap_or(0);
            let mut next_index = rows
                .iter()
                .filter_map(|c| c.chunk_index)
                .map(|i| i + 1)
                .max()
                .unwrap_or(0);
            let mut previous_end = rows
                .iter()
                .filter(|c| c.chunk_index.is_some())
                .map(|c| c.ended_at_ms)
                .max();
            for (name, chunk) in files {
                if self.repo.audio_file_known(&name)? || chunk.position < next_position {
                    continue;
                }
                // Wait for an earlier chunk the recorder has not handed over yet.
                if chunk.position > next_position && info.live {
                    break;
                }
                let checked = if sealed {
                    Err("late_chunk")
                } else {
                    self.check_chunk(&name, &chunk, info, previous_end, now)
                };
                let (index, state, reason, byte_len, sha256) = match checked {
                    Ok((len, digest)) => (Some(next_index), ChunkState::Queued, None, len, digest),
                    Err(reason) => (None, ChunkState::Rejected, Some(reason), 0, String::new()),
                };
                self.repo.insert_audio_chunk(&AudioChunk {
                    chunk_id: Uuid::now_v7(),
                    patient_id: info.patient,
                    segment_id: segment,
                    position: chunk.position,
                    chunk_index: index,
                    file_name: name,
                    content_type: chunk.content_type.into(),
                    byte_len,
                    sha256,
                    started_at_ms: chunk.started_at_ms,
                    ended_at_ms: chunk.ended_at_ms,
                    is_final: false,
                    state,
                    reason: reason.map(str::to_owned),
                })?;
                if index.is_some() {
                    next_index += 1;
                    previous_end = Some(chunk.ended_at_ms);
                }
                next_position = chunk.position + 1;
            }
        }
        Ok(())
    }

    /// Validate one chunk against its segment's boundaries and its own bytes.
    fn check_chunk(
        &self,
        name: &str,
        chunk: &ChunkName,
        info: &SegmentInfo,
        previous_end: Option<i64>,
        now: i64,
    ) -> std::result::Result<(u64, String), &'static str> {
        let latest = info.stopped_at_ms.unwrap_or(now.saturating_add(5_000));
        if chunk.started_at_ms < info.started_at_ms - BOUNDARY_TOLERANCE_MS
            || chunk.ended_at_ms > latest + BOUNDARY_TOLERANCE_MS
        {
            return Err("outside_segment");
        }
        if chunk.ended_at_ms - chunk.started_at_ms > MAX_CHUNK_MS {
            return Err("chunk_too_long");
        }
        if previous_end.is_some_and(|end| chunk.started_at_ms < end - BOUNDARY_TOLERANCE_MS) {
            return Err("overlaps_previous");
        }
        let path = self.audio_dir.join(name);
        let meta = std::fs::symlink_metadata(&path).map_err(|_| "file_unreadable")?;
        if !meta.is_file() {
            return Err("not_a_file");
        }
        if meta.len() == 0 {
            return Err("chunk_empty");
        }
        if meta.len() > MAX_CHUNK_BYTES {
            return Err("chunk_too_large");
        }
        let bytes = std::fs::read(&path).map_err(|_| "file_unreadable")?;
        if bytes.len() as u64 != meta.len() {
            return Err("file_changed");
        }
        if !container_matches(chunk.content_type, &bytes) {
            return Err("container_mismatch");
        }
        Ok((bytes.len() as u64, format!("{:x}", Sha256::digest(&bytes))))
    }

    /// A stopped segment's highest accepted chunk becomes its one final chunk.
    fn seal_stopped_segments(&self) -> Result<()> {
        for (segment, info) in self.segments() {
            if info.stopped_at_ms.is_some() {
                self.repo.seal_segment(segment)?;
            }
        }
        Ok(())
    }

    /// The server's assembled transcript for a segment is the durable processing
    /// acknowledgment for its chunks: only then is local audio deleted. Retention
    /// beyond that point is the Step 9 policy decision.
    pub(crate) fn release_transcribed_audio(&self) -> Result<()> {
        for chunk in self.repo.chunks_in_state(ChunkState::Completed)? {
            let transcribed = self.notes.get(&chunk.patient_id).is_some_and(|n| {
                n.encounters
                    .iter()
                    .any(|e| e.id == chunk.segment_id && e.transcript.is_some())
            });
            if transcribed {
                self.repo.set_audio_state(
                    chunk.chunk_id,
                    &[ChunkState::Completed],
                    ChunkState::Processed,
                    None,
                )?;
            }
        }
        // Marked first, removed second: a crash in between leaves a file that the
        // next pass removes, never a deleted file whose chunk still looks pending.
        for chunk in self.repo.chunks_in_state(ChunkState::Processed)? {
            let path = self.audio_dir.join(&chunk.file_name);
            if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
                let _ = std::fs::remove_file(path);
            }
        }
        Ok(())
    }

    /// Ingest, seal and release. Called after every stop, on open, and by
    /// [`Self::collect_audio`].
    pub(crate) fn settle_audio(&mut self) -> Result<()> {
        self.ingest_audio()?;
        self.seal_stopped_segments()?;
        self.release_transcribed_audio()
    }

    /// The recorder's "a chunk is ready" nudge. Carries no data, so a duplicate
    /// or late nudge is harmless.
    pub fn collect_audio(&mut self) -> Result<AudioView> {
        self.settle_audio()?;
        self.audio_status()
    }

    /// The last moment the recorder durably proved it captured for a segment:
    /// the end of its last accepted chunk, or the segment start if none.
    fn evidence_boundary(&self, segment: &Segment) -> Result<i64> {
        Ok(self
            .repo
            .segment_chunks(segment.id)?
            .iter()
            .filter(|c| c.chunk_index.is_some())
            .map(|c| c.ended_at_ms)
            .max()
            .unwrap_or(segment.started_at_ms)
            .max(segment.started_at_ms))
    }

    /// Close an interrupted segment at its evidence boundary (Step 2 left the
    /// boundary to the caller; the chunk files now supply it).
    pub fn recover_interrupted(&mut self, patient: Uuid) -> Result<i64> {
        if self.visit(patient)?.recorder != RecorderStatus::Interrupted {
            return Err(Error::InvalidState);
        }
        self.ingest_audio()?;
        let segment = self
            .visit(patient)?
            .segments
            .last()
            .ok_or(Error::Recovery)?;
        let boundary = self.evidence_boundary(segment)?;
        self.reconcile_interrupted(patient, boundary)?;
        Ok(boundary)
    }

    /// A caller-supplied interruption boundary may not cut off audio the
    /// recorder already handed over.
    pub(crate) fn check_interruption_boundary(&mut self, patient: Uuid, at_ms: i64) -> Result<()> {
        let _ = self.ingest_audio();
        let segment = self
            .visit(patient)?
            .segments
            .last()
            .ok_or(Error::Recovery)?;
        if at_ms < self.evidence_boundary(segment)? {
            return Err(Error::InvalidInput);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn audio_rows_for_test(&self, segment: Uuid) -> Vec<AudioChunk> {
        self.repo.segment_chunks(segment).unwrap()
    }

    pub fn audio_status(&self) -> Result<AudioView> {
        let segments = self.segments();
        let scan = self.scan_audio(&segments)?;
        let counts = self.repo.audio_counts()?;
        let mut missing = 0u64;
        let mut without_audio = 0u64;
        for (segment, info) in &segments {
            if info.stopped_at_ms.is_none() {
                continue;
            }
            let rows = self.repo.segment_chunks(*segment)?;
            // A segment recorded on another phone has no rows here; once its
            // transcript has arrived it is not "without audio".
            let transcribed = self.notes.get(&info.patient).is_some_and(|n| {
                n.encounters
                    .iter()
                    .any(|e| e.id == *segment && e.transcript.is_some())
            });
            if !transcribed && rows.iter().all(|c| c.chunk_index.is_none()) {
                without_audio += 1;
            }
            if let Some(last) = rows.iter().map(|c| c.position).max() {
                missing += u64::from(last + 1) - rows.len() as u64;
            }
        }
        // Files waiting behind a gap are still to come; a file for a position
        // that is already taken will never be collected and is not ours.
        let (mut waiting, mut stale) = (0u64, 0u64);
        for (segment, files) in &scan.by_segment {
            let next = self
                .repo
                .segment_chunks(*segment)?
                .iter()
                .map(|c| c.position + 1)
                .max()
                .unwrap_or(0);
            for (name, chunk) in files {
                if self.repo.audio_file_known(name)? {
                    continue;
                }
                if chunk.position >= next {
                    waiting += 1;
                } else {
                    stale += 1;
                }
            }
        }
        Ok(AudioView {
            directory: self.audio_dir.to_string_lossy().into_owned(),
            awaiting_upload: counts.queued + counts.uploaded,
            awaiting_transcript: counts.completed,
            processed: counts.processed,
            rejected: counts.rejected,
            not_yet_collected: waiting,
            unclaimed_files: scan.unclaimed + stale,
            unfinished_files: scan.unfinished,
            missing_chunks: missing,
            segments_without_audio: without_audio,
        })
    }
}
