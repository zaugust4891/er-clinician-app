use std::sync::Arc;

use dashmap::DashMap;

use crate::error::AppResult;
use er_domain::patient::PatientId;
use er_domain::replay::{apply_event, replay};
use er_domain::{NoteEvent, PatientNote};
use er_store::repository::Repository;

// ---------------------------------------------------------------------------
// Patient note store
// ---------------------------------------------------------------------------

/// Concurrent store for all active patient notes.
/// Uses DashMap for lock-free reads — the common case is reading note state for UI.
pub type PatientNoteStore = Arc<DashMap<PatientId, PatientNote>>;

/// Create a new empty store.
pub fn new_store() -> PatientNoteStore {
    Arc::new(DashMap::new())
}

/// Load a PatientNote from the repository. Uses the snapshot fast-path
/// when available, falls back to full event replay when not. Returns
/// None if no events exist for this patient (fresh/unknown patient).
pub async fn load_patient_note(
    repo: &dyn Repository,
    patient_id: PatientId,
) -> AppResult<Option<PatientNote>> {
    // Fast path: snapshot exists and checksum validates.
    if let Some((sequence_id, mut note)) = repo.get_latest_snapshot(patient_id).await? {
        let delta = repo.get_events_since(patient_id, sequence_id).await?;
        for stored in &delta {
            apply_event(&mut note, &stored.event);
        }
        return Ok(Some(note));
    }

    // Slow path: no snapshot (or checksum mismatch -> Ok(None) from repo).
    // Full replay from the event stream.
    let events = repo.get_events(patient_id).await?;
    if events.is_empty() {
        return Ok(None);
    }

    let event_refs: Vec<&NoteEvent> = events.iter().map(|se| &se.event).collect();
    // replay() takes a slice of NoteEvent, not &NoteEvent - collect into owned.
    let owned: Vec<NoteEvent> = event_refs.into_iter().cloned().collect();
    Ok(Some(replay(patient_id, &owned)))

    /* NOTE:
    The `clone` is a minor inefficiency. If it becomes a hot path (it shouldn't — this runs at cold start, not per-event),
    switch `replay` to take `&[&NoteEvent]` or an iterator.

    Dependency: This function must be importable without a circular dep. The application
    layer depends on domain (NoteEvent, replay) and on repository::{Repository, StoredEvent}.
    That's fine — `repository.rs` depends on domain (via `PatientNote`),
    but only for types, not for this function. The dependency graph stays acyclic.
    */
}
