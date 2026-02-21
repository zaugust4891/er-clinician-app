use er_note_app::db;
use er_note_app::models::{PatientNote, PatientRecord, Shift};
use er_note_app::state;

fn main() {
    tracing_subscriber::fmt::init();

    tracing::info!("ED Note App starting");

    // Initialize in-memory database for demo.
    let conn = db::open_memory_db().expect("failed to init database");
    tracing::info!("database initialized ({} tables)", count_tables(&conn));

    // Create a shift and a patient.
    let mut shift = Shift::new();
    let patient = PatientRecord::new("chest pain");
    shift.add_patient(patient.id);

    tracing::info!(
        shift_id = %shift.id,
        patient_id = %patient.id,
        cc = %patient.chief_complaint,
        "shift started, patient registered"
    );

    // Create a note and verify the state machine.
    let store = state::new_store();
    let note = PatientNote::new(patient.id);
    store.insert(patient.id, note);

    tracing::info!(
        patient_count = store.len(),
        "note store ready"
    );

    tracing::info!("skeleton operational — all modules loaded");
}

fn count_tables(conn: &rusqlite::Connection) -> usize {
    conn.prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='table'")
        .and_then(|mut stmt| stmt.query_row([], |row| row.get::<_, usize>(0)))
        .unwrap_or(0)
}
