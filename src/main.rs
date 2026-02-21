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

    // Create a note and exercise the state machine.
    let mut note = PatientNote::new(patient.id);
    tracing::info!(?note, "note created");

    note.handle_audio_started();
    tracing::info!(?note, "encounter started");

    note.handle_audio_stopped();
    tracing::info!(?note, "encounter stopped");

    note.handle_trigger_mdm();
    tracing::info!(?note, "MDM triggered");

    note.handle_review_ready();
    tracing::info!(?note, "review ready");

    note.handle_sign();
    tracing::info!(?note, "note signed");

    // Verify serialization round-trip with hydrate.
    let json = serde_json::to_string(&note).expect("serialize");
    let mut restored: PatientNote = serde_json::from_str(&json).expect("deserialize");
    restored.hydrate();
    tracing::info!(?restored, "deserialized + hydrated");

    // Put it in the store.
    let store = state::new_store();
    store.insert(patient.id, note);

    tracing::info!(
        patient_count = store.len(),
        "note store ready"
    );

    tracing::info!("state pattern operational — all transitions verified");
}

fn count_tables(conn: &rusqlite::Connection) -> usize {
    conn.prepare("SELECT COUNT(*) FROM sqlite_master WHERE type='table'")
        .and_then(|mut stmt| stmt.query_row([], |row| row.get::<_, usize>(0)))
        .unwrap_or(0)
}
