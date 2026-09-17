use crate::error::{StoreError, StoreResult};
use chrono::{DateTime, Utc};
use er_domain::NoteEvent;
use er_domain::{PatientId, PatientNote};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[async_trait::async_trait]
pub trait Repository: Send + Sync {
    async fn append_event(
        &self,
        patient_id: PatientId,
        event_id: Uuid,
        event: NoteEvent,
    ) -> StoreResult<u64>;

    async fn get_events(&self, patient_id: PatientId) -> StoreResult<Vec<StoredEvent>>;

    async fn get_events_since(
        &self,
        patient_id: PatientId,
        sequence_id: u64,
    ) -> StoreResult<Vec<StoredEvent>>;

    async fn save_snapshot(
        &self,
        patient_id: PatientId,
        sequence_id: u64,
        snapshot: &PatientNote,
    ) -> StoreResult<()>;

    async fn get_latest_snapshot(
        &self,
        patient_id: PatientId,
    ) -> StoreResult<Option<(u64, PatientNote)>>;

    async fn event_count(&self) -> StoreResult<usize>;

    async fn snapshot_count(&self) -> StoreResult<usize>;
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

    /// Shared row → StoredEvent mapping for the get_events* queries.
    /// `sql` MUST select (sequence_id, event_id, payload, occurred_at) in that order.
    fn query_stored_events(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> StoreResult<Vec<StoredEvent>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(sql)?;

        // The closure can only return rusqlite errors, and the Row borrow ends
        // when it returns — so pull out the *raw* column values here and do the
        // fallible Uuid/JSON/date parsing below, where we can return a StoreError.
        let rows = stmt.query_map(params, |row| {
            Ok((
                row.get::<_, i64>(0)?,    // sequence_id
                row.get::<_, String>(1)?, // event_id (TEXT)
                row.get::<_, String>(2)?, // payload JSON
                row.get::<_, String>(3)?, // occurred_at (RFC3339)
            ))
        })?;

        let mut events = Vec::new();
        for row in rows {
            let (seq_id, event_id_str, payload_json, occurred_at_str) = row?;

            let event_id = Uuid::parse_str(&event_id_str).map_err(|e| StoreError::Parse {
                context: format!("event_id (sequence_id: {seq_id})"),
                source: Box::new(e),
            })?;

            let event: NoteEvent =
                serde_json::from_str(&payload_json).map_err(|e| StoreError::Parse {
                    context: format!("event payload (sequence_id: {seq_id})"),
                    source: Box::new(e),
                })?;

            let occurred_at = DateTime::parse_from_rfc3339(&occurred_at_str)
                .map_err(|e| StoreError::Parse {
                    context: format!("occurred_at (sequence_id: {seq_id})"),
                    source: Box::new(e),
                })?
                .with_timezone(&Utc);

            events.push(StoredEvent {
                sequence_id: seq_id as u64,
                event_id,
                event,
                occurred_at,
            });
        }
        Ok(events)
    }
}

#[async_trait::async_trait]
impl Repository for SqliteRepository {
    async fn append_event(
        &self,
        patient_id: PatientId,
        event_id: Uuid,
        event: NoteEvent,
    ) -> StoreResult<u64> {
        let conn = self.conn.lock().unwrap();
        let payload_json = serde_json::to_string(&event)?;
        let event_id_text = event_id.to_string();
        let patient_id_text = patient_id.to_string();
        let event_type_text = event.event_type();
        let occurred_at_text = event.clinical_occurred_at().to_rfc3339();
        let rows_inserted = conn.execute(
            "INSERT OR IGNORE INTO events \
            (event_id, patient_id, event_type, payload, occurred_at) \
            VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                event_id_text,
                patient_id_text,
                event_type_text,
                payload_json,
                occurred_at_text
            ],
        )?;

        if rows_inserted == 1 {
            // Fresh insert: this connection's last rowid IS the new row's sequence id
            Ok(conn.last_insert_rowid() as u64)
        } else {
            // duplicate event_id (a retry)
            // We must NOT use last_insert_rowid() here: after an ignored insert it
            // still points at whatever this connection last inserted, not at the
            // existing row. So look the original up by its event_id and return that —
            // a duplicate append now returns the SAME sequence_id as the first call
            let seq: i64 = conn.query_row(
                "SELECT sequence_id FROM events WHERE event_id = ?1",
                rusqlite::params![event_id_text],
                |row| row.get(0),
            )?;
            Ok(seq as u64)
        }
    }

    async fn get_events(&self, patient_id: PatientId) -> StoreResult<Vec<StoredEvent>> {
        self.query_stored_events(
            "SELECT sequence_id, event_id, payload, occurred_at \
            FROM events WHERE patient_id = ?1 ORDER BY sequence_id ASC",
            rusqlite::params![patient_id.to_string()],
        )
    }

    async fn get_events_since(
        &self,
        patient_id: PatientId,
        sequence_id: u64,
    ) -> StoreResult<Vec<StoredEvent>> {
        self.query_stored_events(
            "SELECT sequence_id, event_id, payload, occurred_at \
            FROM events WHERE patient_id = ?1 AND sequence_id > ?2 ORDER BY sequence_id ASC",
            rusqlite::params![patient_id.to_string(), sequence_id],
        )
    }

    async fn save_snapshot(
        &self,
        patient_id: PatientId,
        sequence_id: u64,
        snapshot: &PatientNote,
    ) -> StoreResult<()> {
        let conn = self.conn.lock().unwrap();

        // Serialize the aggregate. state: Option<Box<dyn NoteState>> is
        // #[serde(skip)] on PatientNote — hydrate() reconstructs it on load.
        let snapshot_json = serde_json::to_string(snapshot)?;

        // SHA-256 of the serialized JSON, as lowercase hex. Written alongside
        // the row so get_latest_snapshot can detect corruption and fall back
        // to a full event replay.
        let checksum = snapshot_checksum(&snapshot_json);

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

    async fn get_latest_snapshot(
        &self,
        patient_id: PatientId,
    ) -> StoreResult<Option<(u64, PatientNote)>> {
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
            Err(e) => return Err(StoreError::Db(e)),
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

    async fn event_count(&self) -> StoreResult<usize> {
        let conn = self.conn.lock().unwrap();
        let count = conn.query_row("SELECT COUNT(*) FROM events", [], |row| {
            row.get::<_, usize>(0)
        })?;
        Ok(count)
    }

    async fn snapshot_count(&self) -> StoreResult<usize> {
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
    pub event_id: Uuid,
    pub event: NoteEvent,
    pub occurred_at: DateTime<Utc>,
}

pub fn snapshot_checksum(serialized: &str) -> String {
    format!("{:x}", sha2::Sha256::digest(serialized.as_bytes()))
}
