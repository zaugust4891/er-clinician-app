use rusqlite::Connection;

use crate::error::StoreResult;

/// Initialize the SQLite database with the required schema.
/// Uses WAL mode for crash resilience.
pub fn init_db(conn: &Connection) -> StoreResult<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS events (
            sequence_id     INTEGER PRIMARY KEY AUTOINCREMENT,
            event_id        TEXT NOT NULL UNIQUE,
            patient_id      TEXT NOT NULL,
            event_type      TEXT NOT NULL,
            payload         TEXT NOT NULL,
            occurred_at     TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS snapshots (
            patient_id      TEXT PRIMARY KEY,
            sequence_id     INTEGER NOT NULL,
            snapshot        TEXT NOT NULL,
            checksum        TEXT NOT NULL, -- SHA-256 of snapshot text
            created_at      TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_events_patient ON events(patient_id);
        CREATE INDEX IF NOT EXISTS idx_snapshots_patient ON snapshots(patient_id);
        ",
    )?;

    Ok(())
}

/// Open (or create) the database at the given path and initialize the schema.
pub fn open_db(path: &str) -> StoreResult<Connection> {
    let conn = Connection::open(path)?;
    init_db(&conn)?;
    Ok(conn)
}

/// Open an in-memory database for testing.
pub fn open_memory_db() -> StoreResult<Connection> {
    let conn = Connection::open_in_memory()?;
    init_db(&conn)?;
    Ok(conn)
}

// #[cfg(test)]
// mod tests {
//     use super::*;

//     #[test]
// }
