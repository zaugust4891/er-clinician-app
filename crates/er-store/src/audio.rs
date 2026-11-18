//! Phone audio chunk ledger (Phase 5 Step 4).
//!
//! One row per chunk file the device took ownership of. A row never holds audio
//! bytes or a presigned URL (that URL is a bearer credential); it holds the IDs
//! that bind the chunk to one patient and one capture segment, the capture
//! times, and a SHA-256 of the file so a changed file is caught before upload.
//!
//! States move forward only, each by compare-and-set:
//! `queued` → `uploaded` (bytes PUT) → `completed` (server verified) →
//! `processed` (the server's transcript arrived; the file may be deleted).
//! `uploaded` may fall back to `queued` when the server says the object is
//! missing. `rejected` is terminal and keeps its reason visible.
use crate::{SqliteRepository, StoreError, StoreResult};
use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChunkState {
    Queued,
    Uploaded,
    Completed,
    Processed,
    Rejected,
}
impl ChunkState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Uploaded => "uploaded",
            Self::Completed => "completed",
            Self::Processed => "processed",
            Self::Rejected => "rejected",
        }
    }
    fn parse(s: &str) -> StoreResult<Self> {
        Ok(match s {
            "queued" => Self::Queued,
            "uploaded" => Self::Uploaded,
            "completed" => Self::Completed,
            "processed" => Self::Processed,
            "rejected" => Self::Rejected,
            _ => return Err(StoreError::Integrity),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioChunk {
    pub chunk_id: Uuid,
    pub patient_id: Uuid,
    pub segment_id: Uuid,
    /// The recorder's own 0-based sequence number, taken from the file name.
    pub position: u32,
    /// Contiguous server index among accepted chunks; `None` only for a file
    /// rejected when it was first examined.
    pub chunk_index: Option<u32>,
    pub file_name: String,
    pub content_type: String,
    pub byte_len: u64,
    pub sha256: String,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
    pub is_final: bool,
    pub state: ChunkState,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioCounts {
    pub queued: u64,
    pub uploaded: u64,
    pub completed: u64,
    pub processed: u64,
    pub rejected: u64,
}

const COLUMNS: &str = "chunk_id,patient_id,segment_id,position,chunk_index,file_name,content_type,byte_len,sha256,started_at_ms,ended_at_ms,is_final,state,reason";

fn uuid(row: &Row, i: usize) -> rusqlite::Result<Uuid> {
    let text: String = row.get(i)?;
    Uuid::parse_str(&text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
    })
}
fn read(row: &Row) -> rusqlite::Result<(AudioChunk, String)> {
    Ok((
        AudioChunk {
            chunk_id: uuid(row, 0)?,
            patient_id: uuid(row, 1)?,
            segment_id: uuid(row, 2)?,
            position: row.get(3)?,
            chunk_index: row.get(4)?,
            file_name: row.get(5)?,
            content_type: row.get(6)?,
            byte_len: row.get(7)?,
            sha256: row.get(8)?,
            started_at_ms: row.get(9)?,
            ended_at_ms: row.get(10)?,
            is_final: row.get(11)?,
            state: ChunkState::Queued,
            reason: row.get(13)?,
        },
        row.get(12)?,
    ))
}

impl SqliteRepository {
    fn query_chunks(
        &self,
        filter: &str,
        params: impl rusqlite::Params,
    ) -> StoreResult<Vec<AudioChunk>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM device_audio_chunks WHERE {filter}"
        ))?;
        let rows = stmt.query_map(params, read)?;
        let mut chunks = Vec::new();
        for row in rows {
            let (mut chunk, state) = row?;
            chunk.state = ChunkState::parse(&state)?;
            chunks.push(chunk);
        }
        Ok(chunks)
    }

    pub fn audio_file_known(&self, file_name: &str) -> StoreResult<bool> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT 1 FROM device_audio_chunks WHERE file_name=?1",
                [file_name],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Every chunk row of one capture segment, in recorder order.
    pub fn segment_chunks(&self, segment: Uuid) -> StoreResult<Vec<AudioChunk>> {
        self.query_chunks("segment_id=?1 ORDER BY position", [segment.to_string()])
    }

    pub fn chunks_in_state(&self, state: ChunkState) -> StoreResult<Vec<AudioChunk>> {
        self.query_chunks("state=?1 ORDER BY segment_id,position", [state.as_str()])
    }

    /// Record a new chunk. The table's unique keys refuse a second row for the
    /// same file, recorder position, or server index of one segment.
    pub fn insert_audio_chunk(&self, chunk: &AudioChunk) -> StoreResult<()> {
        let rejected = chunk.state == ChunkState::Rejected;
        if rejected != chunk.chunk_index.is_none()
            || chunk.is_final
            || !matches!(chunk.state, ChunkState::Queued | ChunkState::Rejected)
        {
            return Err(StoreError::Integrity);
        }
        let conn = self.conn.lock().unwrap();
        conn.execute(
            &format!("INSERT INTO device_audio_chunks({COLUMNS}) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,0,?12,?13)"),
            params![
                chunk.chunk_id.to_string(),
                chunk.patient_id.to_string(),
                chunk.segment_id.to_string(),
                chunk.position,
                chunk.chunk_index,
                chunk.file_name,
                chunk.content_type,
                chunk.byte_len,
                chunk.sha256,
                chunk.started_at_ms,
                chunk.ended_at_ms,
                chunk.state.as_str(),
                chunk.reason,
            ],
        )?;
        Ok(())
    }

    /// Close a stopped segment: its highest accepted index becomes the one final
    /// chunk. Idempotent; returns whether the segment is sealed afterwards (false
    /// when it has no accepted audio at all).
    pub fn seal_segment(&self, segment: Uuid) -> StoreResult<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let sealed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM device_audio_chunks WHERE segment_id=?1 AND is_final=1)",
            [segment.to_string()],
            |r| r.get(0),
        )?;
        if !sealed {
            tx.execute(
                "UPDATE device_audio_chunks SET is_final=1 WHERE segment_id=?1 AND chunk_index=(SELECT MAX(chunk_index) FROM device_audio_chunks WHERE segment_id=?1)",
                [segment.to_string()],
            )?;
        }
        let sealed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM device_audio_chunks WHERE segment_id=?1 AND is_final=1)",
            [segment.to_string()],
            |r| r.get(0),
        )?;
        tx.commit()?;
        Ok(sealed)
    }

    /// The next chunk of this patient that needs network work: only chunks of
    /// sealed segments, lowest segment then lowest index first, so a segment's
    /// final chunk is always its last upload.
    pub fn next_audio_work(&self, patient: Uuid) -> StoreResult<Option<AudioChunk>> {
        Ok(self
            .query_chunks(
                "patient_id=?1 AND state IN ('queued','uploaded') AND segment_id IN (SELECT segment_id FROM device_audio_chunks WHERE is_final=1) ORDER BY segment_id,chunk_index LIMIT 1",
                [patient.to_string()],
            )?
            .pop())
    }

    pub fn audio_work_pending(&self, patient: Uuid) -> StoreResult<bool> {
        Ok(self.next_audio_work(patient)?.is_some())
    }

    /// Move one chunk forward. Fails with `Conflict` unless it is in `from`.
    pub fn set_audio_state(
        &self,
        chunk: Uuid,
        from: &[ChunkState],
        to: ChunkState,
        reason: Option<&str>,
    ) -> StoreResult<()> {
        // A chunk rejected after acceptance keeps its server index: the server
        // may already hold an intent for it, and renumbering would break the
        // index contract. The gap stays visible instead.
        if to == ChunkState::Rejected
            && from
                .iter()
                .any(|s| !matches!(s, ChunkState::Queued | ChunkState::Uploaded))
        {
            return Err(StoreError::Integrity);
        }
        let conn = self.conn.lock().unwrap();
        let states: Vec<String> = from.iter().map(|s| format!("'{}'", s.as_str())).collect();
        let changed = conn.execute(
            &format!(
                "UPDATE device_audio_chunks SET state=?1,reason=?2 WHERE chunk_id=?3 AND state IN ({})",
                states.join(",")
            ),
            params![to.as_str(), reason, chunk.to_string()],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict);
        }
        Ok(())
    }

    /// Remember why the last network attempt for a chunk failed, without moving it.
    pub fn note_audio_retry(&self, chunk: Uuid, reason: &str) -> StoreResult<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE device_audio_chunks SET reason=?1 WHERE chunk_id=?2 AND state IN ('queued','uploaded')",
            params![reason, chunk.to_string()],
        )?;
        Ok(())
    }

    pub fn audio_counts(&self) -> StoreResult<AudioCounts> {
        let conn = self.conn.lock().unwrap();
        let mut counts = AudioCounts::default();
        let mut stmt =
            conn.prepare("SELECT state,COUNT(*) FROM device_audio_chunks GROUP BY state")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))?;
        for row in rows {
            let (state, n) = row?;
            match ChunkState::parse(&state)? {
                ChunkState::Queued => counts.queued = n,
                ChunkState::Uploaded => counts.uploaded = n,
                ChunkState::Completed => counts.completed = n,
                ChunkState::Processed => counts.processed = n,
                ChunkState::Rejected => counts.rejected = n,
            }
        }
        Ok(counts)
    }
}
