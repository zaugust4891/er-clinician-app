use rusqlite::Connection;

use crate::error::Result;

/// Initialize the SQLite database with the required schema.
/// Uses WAL mode for crash resilience.
pub fn init_db(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;

    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS shifts (
            id              TEXT PRIMARY KEY,
            started_at      TEXT NOT NULL,
            ended_at        TEXT,
            data            TEXT NOT NULL   -- JSON blob of Shift
        );

        CREATE TABLE IF NOT EXISTS patients (
            id              TEXT PRIMARY KEY,
            shift_id        TEXT NOT NULL REFERENCES shifts(id),
            chief_complaint TEXT NOT NULL,
            room            TEXT,
            data            TEXT NOT NULL   -- JSON blob of PatientRecord
        );

        CREATE TABLE IF NOT EXISTS encounters (
            id              TEXT PRIMARY KEY,
            patient_id      TEXT NOT NULL REFERENCES patients(id),
            capture_mode    TEXT NOT NULL,
            started_at      TEXT NOT NULL,
            ended_at        TEXT,
            transcript      TEXT,
            encounter_number INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS results (
            id              TEXT PRIMARY KEY,
            patient_id      TEXT NOT NULL REFERENCES patients(id),
            source          TEXT NOT NULL,
            timestamp       TEXT NOT NULL,
            data            TEXT NOT NULL   -- JSON blob of ClinicalResult
        );

        CREATE TABLE IF NOT EXISTS note_sections (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            patient_id      TEXT NOT NULL REFERENCES patients(id),
            section         TEXT NOT NULL,  -- 'hpi', 'exam', 'mdm', etc.
            content         TEXT NOT NULL,
            version         INTEGER NOT NULL,
            generated_at    TEXT NOT NULL,
            source_encounter_ids TEXT       -- JSON array of UUIDs
        );

        CREATE TABLE IF NOT EXISTS audit_log (
            id              TEXT PRIMARY KEY,
            patient_id      TEXT,
            timestamp       TEXT NOT NULL,
            action          TEXT NOT NULL,
            details         TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_patients_shift ON patients(shift_id);
        CREATE INDEX IF NOT EXISTS idx_encounters_patient ON encounters(patient_id);
        CREATE INDEX IF NOT EXISTS idx_results_patient ON results(patient_id);
        CREATE INDEX IF NOT EXISTS idx_note_sections_patient ON note_sections(patient_id);
        CREATE INDEX IF NOT EXISTS idx_audit_log_timestamp ON audit_log(timestamp);
        ",
    )?;

    Ok(())
}

/// Open (or create) the database at the given path and initialize the schema.
pub fn open_db(path: &str) -> Result<Connection> {
    let conn = Connection::open(path)?;
    init_db(&conn)?;
    Ok(conn)
}

/// Open an in-memory database for testing.
pub fn open_memory_db() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    init_db(&conn)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_creates_successfully() {
        let conn = open_memory_db().expect("should open in-memory db");

        // Verify tables exist by querying sqlite_master.
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"shifts".to_string()));
        assert!(tables.contains(&"patients".to_string()));
        assert!(tables.contains(&"encounters".to_string()));
        assert!(tables.contains(&"results".to_string()));
        assert!(tables.contains(&"note_sections".to_string()));
        assert!(tables.contains(&"audit_log".to_string()));
    }

    #[test]
    fn idempotent_init() {
        let conn = open_memory_db().expect("should open in-memory db");
        // Running init again should not fail.
        init_db(&conn).expect("second init should succeed");
    }
}
