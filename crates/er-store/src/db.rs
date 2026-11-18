use rusqlite::Connection;

use crate::error::StoreResult;

/// Initialize the SQLite database with the required schema.
/// Uses WAL mode for crash resilience.
pub fn init_db(conn: &Connection) -> StoreResult<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;

    conn.busy_timeout(std::time::Duration::from_millis(1500))?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    let version: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version > 1 {
        return Err(crate::StoreError::Schema);
    }
    if version == 0 {
        // Legacy events/snapshots are preserved. DDL and version advance roll back together.
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch("
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

        CREATE TABLE IF NOT EXISTS device_scope(singleton INTEGER PRIMARY KEY CHECK(singleton=1), scope TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS device_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1), revision INTEGER NOT NULL, payload TEXT NOT NULL);
        PRAGMA user_version = 1;
        ")?;
        validate_schema(&tx)?;
        tx.commit()?;
    } else {
        validate_schema(conn)?;
    }

    Ok(())
}

fn validate_schema(conn: &Connection) -> StoreResult<()> {
    for (table, columns) in [
        (
            "events",
            "sequence_id,event_id,patient_id,event_type,payload,occurred_at",
        ),
        (
            "snapshots",
            "patient_id,sequence_id,snapshot,checksum,created_at",
        ),
        ("device_scope", "singleton,scope"),
        ("device_state", "singleton,revision,payload"),
    ] {
        let kind: String = conn.query_row(
            "SELECT type FROM sqlite_master WHERE name=?1",
            [table],
            |r| r.get(0),
        )?;
        if kind != "table" {
            return Err(crate::StoreError::Integrity);
        }
        // Only compile known, repository-owned identifiers, never caller input.
        conn.prepare(&format!("SELECT {columns} FROM {table} LIMIT 0"))?;
    }
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
