use chrono::{DateTime, Utc};

use crate::models::{PatientId, PatientNote};
use crate::events::{NoteEvent};
use crate::error::AppResult;

pub trait Repository: Send + Sync {
    fn append_event(&self, patient_id: PatientId, event: NoteEvent) -> AppResult<u64>;

    fn get_events(&self, patient_id: PatientId) -> AppResult<Vec<StoredEvent>>;

    fn get_events_since(&self, patient_id: PatientId, sequence_id: u64) -> AppResult<Vec<StoredEvent>>;

    fn save_snapshot(&self, patient_id: PatientId, sequence_id: u64, snapshot: &PatientNote) -> AppResult<()>;

    fn get_latest_snapshot(&self, patient_id: PatientId) -> AppResult<Option<(u64, PatientNote)>>;
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

impl Repository for SqliteRepository {
     /*
    Step by step:

  1. Lock the mutex. self.conn.lock().unwrap() gives you a MutexGuard<Connection>, which dereferences to
  &Connection. The .unwrap() will panic if a previous holder panicked — this is intentional for a medical app
  (fail loud, don't continue with possibly-corrupted state).

  2. Serialize the event. serde_json::to_string(&event)? turns the NoteEvent enum into a JSON string. Because
  NoteEvent derives Serialize, serde knows how to do this. The ? propagates any serialization error as
  AppError::Serialization (your error enum has #[from] serde_json::Error).

  For example, this Rust value:
  NoteEvent::AudioStarted {
      patient_id: some_uuid,
      capture_mode: CaptureMode::Ambient,
  }
  becomes this JSON string:
  {"AudioStarted":{"patient_id":"550e8400-...","capture_mode":"Ambient"}}

  3. Get the other column values.
  - patient_id.to_string() — UUID as lowercase hyphenated text: "550e8400-e29b-41d4-a716-446655440000"
  - event.event_type() — your existing helper method returns "AudioStarted", "AudioStopped", etc.
  - Utc::now().to_rfc3339() — the persistence timestamp: "2026-03-16T18:15:25.123456789+00:00"

  4. Execute the INSERT. rusqlite's conn.execute() takes a SQL string and params![]:

  conn.execute(
      "INSERT INTO events (patient_id, event_type, payload, occurred_at) VALUES (?1, ?2, ?3, ?4)",
      rusqlite::params![
          patient_id.to_string(),
          event.event_type(),
          payload_json,          // the serde_json::to_string result
          Utc::now().to_rfc3339(),
      ],
  )?;

  Notice you do NOT specify sequence_id in the INSERT. SQLite fills it automatically via AUTOINCREMENT. The ?
  propagates SQLite errors as AppError::Db (your error enum has #[from] rusqlite::Error).

  5. Get the assigned sequence_id. conn.last_insert_rowid() returns the AUTOINCREMENT value SQLite just
  assigned. It's an i64 (SQLite's native integer type), so cast it: conn.last_insert_rowid() as u64.

  6. Return it. Ok(conn.last_insert_rowid() as u64)

  The mental model: You're writing one row to a table. Four columns come from you (patient_id, event_type,
  payload, occurred_at). One column comes from SQLite (sequence_id). You get that one back.
 */
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


/*
get_events(&self, patient_id: PatientId) -> AppResult<Vec<StoredEvent>>

  This is the read path. You need to pull all events for a patient from the database, deserialize each one, and
   wrap them in StoredEvent.

  Step by step:

  1. Lock the mutex. Same as append.

  2. Prepare the query. conn.prepare(...) compiles the SQL once. rusqlite returns a Statement you can execute
  multiple times (useful if called in a loop, but here it's one-shot):

  let mut stmt = conn.prepare(
      "SELECT sequence_id, payload, occurred_at FROM events WHERE patient_id = ?1 ORDER BY sequence_id ASC"
  )?;

  The ORDER BY sequence_id ASC is critical — replay assumes events are in insertion order. Without it, SQLite
  may return rows in any order.

  3. Execute and map rows. rusqlite's query_map runs the query and gives you a closure that runs per-row. Each
  row is a &Row that you pull columns from:

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

  query_map returns an iterator of Result<(i64, String, String)>. Each row.get() can fail (type mismatch, null
  value), hence the ? inside the closure.

  4. Deserialize each row into a StoredEvent. Iterate over the rows, parse the JSON payload and timestamp:

  let mut events = Vec::new();
  for row in rows {
      let (seq_id, payload_json, occurred_at_str) = row?;

      let event: NoteEvent = serde_json::from_str(&payload_json)?;
      let occurred_at = DateTime::parse_from_rfc3339(&occurred_at_str)
          .map_err(/* ... */)?
          .with_timezone(&Utc);

      events.push(StoredEvent {
          sequence_id: seq_id as u64,
          event,
          occurred_at,
      });
  }

  Here's what's happening per-row:
  - row? — unwrap the rusqlite Result (propagates DB errors)
  - serde_json::from_str(&payload_json)? — the reverse of step 2 in append. Turns {"AudioStarted":{...}} back
  into a NoteEvent::AudioStarted { ... }. The ? propagates as AppError::Serialization.
  - DateTime::parse_from_rfc3339 — parses the RFC 3339 string back into a DateTime. Note: parse_from_rfc3339
  returns DateTime<FixedOffset>, so .with_timezone(&Utc) converts it to DateTime<Utc>.

  5. Return. Ok(events)

  Edge case: empty result. If no events exist for this patient_id, the iterator produces zero rows. You get
  Ok(vec![]). This is correct — "no events" is not an error.

  The mental model: You're reading N rows from a table, and for each row, doing the reverse of what
  append_event did: JSON string → NoteEvent, timestamp string → DateTime, rowid → sequence_id. Then you bundle
  all three into a StoredEvent.


*/
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
    }

    fn get_events_since(&self, patient_id: PatientId, sequence_id: u64) -> Vec<NoteEvent> {
        todo!()
    }

}

#[derive(Debug, Clone)]
pub struct StoredEvent {
    pub sequence_id: u64,
    pub event: NoteEvent,
    pub occurred_at: DateTime<Utc>,
}
