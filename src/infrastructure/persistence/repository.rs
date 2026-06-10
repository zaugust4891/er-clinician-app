use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::domain::{PatientId, PatientNote};
use crate::domain::NoteEvent;
use crate::error::{AppError, AppResult};

pub trait Repository: Send + Sync {
    fn append_event(&self, patient_id: PatientId, event: NoteEvent) -> AppResult<u64>;

    fn get_events(&self, patient_id: PatientId) -> AppResult<Vec<StoredEvent>>;

    fn get_events_since(&self, patient_id: PatientId, sequence_id: u64) -> AppResult<Vec<StoredEvent>>;

    fn save_snapshot(&self, patient_id: PatientId, sequence_id: u64, snapshot: &PatientNote) -> AppResult<()>;

    fn get_latest_snapshot(&self, patient_id: PatientId) -> AppResult<Option<(u64, PatientNote)>>;

    fn event_count(&self) -> AppResult<usize>;

    fn snapshot_count(&self) -> AppResult<usize>;
}

pub struct SqliteRepository {
    conn: std::sync::Mutex<rusqlite::Connection>,
}

impl SqliteRepository {
    pub fn new(conn: rusqlite::Connection) -> Self {
        Self {
            conn: std::sync::Mutex::new(conn),
        }
    }
}

/// Test-support escape hatch: lets integration tests corrupt rows to
/// exercise checksum-mismatch fallback paths. Not for production use —
/// becomes a `test-util` feature when this module is extracted to its
/// own crate.
#[doc(hidden)]
impl SqliteRepository {
    pub fn execute_raw(&self, sql: &str, params: &[&dyn rusqlite::types::ToSql]) {
        let conn = self.conn.lock().unwrap();
        conn.execute(sql, params).unwrap();
    }
}

impl Repository for SqliteRepository {
    fn append_event(&self, patient_id: PatientId, event: NoteEvent) -> AppResult<u64> {
        let conn = self.conn.lock().unwrap();
        let payload_json = serde_json::to_string(&event)?;
        let patient_id_text = patient_id.to_string();
        let event_type_text = event.event_type();
        let occurred_at_text = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO events (patient_id, event_type, payload, occurred_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![patient_id_text, event_type_text, payload_json, occurred_at_text],
        )?;
        Ok(conn.last_insert_rowid() as u64)
    }

    fn get_events(&self, patient_id: PatientId) -> AppResult<Vec<StoredEvent>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT sequence_id, payload, occurred_at FROM events WHERE patient_id = ?1 ORDER BY sequence_id ASC"
        )?;
        let rows = stmt.query_map(
            rusqlite::params![patient_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,         // sequence_id (column index 0)
                    row.get::<_, String>(1)?,       // payload JSON (column index 1)
                    row.get::<_, String>(2)?,       // occurred_at string (column index 2)
                ))
            },
        )?;

        let mut events = Vec::new();
        for row in rows {
            let (seq_id, payload_json, occurred_at_str) = row?;
            let event: NoteEvent = serde_json::from_str(&payload_json)?;
            let occurred_at = DateTime::parse_from_rfc3339(&occurred_at_str)
                .map_err(|e| AppError::Db(rusqlite::Error::InvalidParameterName(e.to_string())))?
                .with_timezone(&Utc);
            events.push(StoredEvent {
                sequence_id: seq_id as u64,
                event,
                occurred_at,
            });
        }
        Ok(events)
    }

    fn get_events_since(&self, patient_id: PatientId, sequence_id: u64) -> AppResult<Vec<StoredEvent>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT sequence_id, payload, occurred_at 
            FROM events 
            WHERE patient_id = ?1 
            AND sequence_id > ?2 
            ORDER BY sequence_id ASC"
        )?;

        let rows = stmt.query_map(
            rusqlite::params![patient_id.to_string(), sequence_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )?;

        let mut events = Vec::new();
        for row in rows {
            let (seq_id, payload_json, occurred_at_str) = row?;
            // Contextual JSON Parsing
            let event: NoteEvent = serde_json::from_str(&payload_json)
                .map_err(|e| AppError::ParseError {
                    context: format!("Event payload (sequence_id: {})", seq_id),
                    source: Box::new(e),
                })?;

            // Contextual Date Parsing
            let occurred_at = DateTime::parse_from_rfc3339(&occurred_at_str)
                .map_err(|e| AppError::ParseError {
                    context: format!("occurred_at timestamp (sequence_id: {})", seq_id),
                    source: Box::new(e),
                })?
                .with_timezone(&Utc);
            events.push(StoredEvent{
                sequence_id: seq_id as u64,
                event,
                occurred_at,
            });
        }
        Ok(events)
    }

    fn save_snapshot(
        &self,
        patient_id: PatientId,
        sequence_id: u64,
        snapshot: &PatientNote,
    ) -> AppResult<()> {
        let conn = self.conn.lock().unwrap();

        // Serialize the aggregate. state: Option<Box<dyn NoteState>> is
        // #[serde(skip)] on PatientNote — hydrate() reconstructs it on load.
        let snapshot_json = serde_json::to_string(snapshot)?;

        // SHA-256 of the serialized JSON, as lowercase hex. Written alongside
        // the row so get_latest_snapshot can detect corruption and fall back
        // to a full event replay.
        let checksum = format!("{:x}", Sha256::digest(snapshot_json.as_bytes()));

        // Upsert: one snapshot per patient, latest wins. patient_id is the
        // PRIMARY KEY on the snapshots table.
        conn.execute(
            "INSERT OR REPLACE INTO snapshots \
             (patient_id, sequence_id, snapshot, checksum, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                patient_id.to_string(),
                sequence_id as i64,
                snapshot_json,
                checksum,
                Utc::now().to_rfc3339(),
            ],
        )?;

        Ok(())
    }

    fn get_latest_snapshot(
        &self,
        patient_id: PatientId,
    ) -> AppResult<Option<(u64, PatientNote)>> {
        let conn = self.conn.lock().unwrap();

        // 0 or 1 row — patient_id is the PRIMARY KEY.
        let row_result = conn.query_row(
            "SELECT sequence_id, snapshot, checksum \
             FROM snapshots WHERE patient_id = ?1",
            rusqlite::params![patient_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,    // sequence_id
                    row.get::<_, String>(1)?, // snapshot JSON
                    row.get::<_, String>(2)?, // stored checksum (hex)
                ))
            },
        );

        let (seq_id, snapshot_json, stored_checksum) = match row_result {
            Ok(tuple) => tuple,
            // No snapshot for this patient is a normal state — caller falls
            // back to full event-stream replay. NOT an error.
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => return Err(AppError::Db(e)),
        };

        // Verify integrity. A mismatch means the row was corrupted on disk
        // (or never written cleanly). The event stream is the source of
        // truth — return Ok(None) so load_patient_note triggers a full
        // replay instead of propagating an error to the clinician.
        let computed = format!("{:x}", Sha256::digest(snapshot_json.as_bytes()));
        if computed != stored_checksum {
            tracing::warn!(
                patient_id = %patient_id,
                sequence_id = seq_id,
                expected = %stored_checksum,
                computed = %computed,
                "snapshot checksum mismatch — falling back to full replay"
            );
            return Ok(None);
        }

        // Deserialize, then hydrate() to rebuild the state trait object
        // (PatientNote::state is #[serde(skip)], so it comes back as None
        // and must be reconstructed from the phase enum before the note is
        // usable for any state-machine call).
        let mut note: PatientNote = serde_json::from_str(&snapshot_json)?;
        note.hydrate();

        Ok(Some((seq_id as u64, note)))
    }

    fn event_count(&self) -> AppResult<usize> {
        let conn = self.conn.lock().unwrap();
        let count = conn.query_row("SELECT COUNT(*) FROM events", [], |row| {
            row.get::<_, usize>(0)
        })?;
        Ok(count)
    }

    fn snapshot_count(&self) -> AppResult<usize> {
        let conn = self.conn.lock().unwrap();
        let count = conn.query_row("SELECT COUNT(*) FROM snapshots", [], |row| {
            row.get::<_, usize>(0)
        })?;
        Ok(count)
    }

}

#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub sequence_id: u64,
    pub event: NoteEvent,
    pub occurred_at: DateTime<Utc>,
}
