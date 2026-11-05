//! Read-only restore evidence. Full event replay is compared independently of
//! snapshots; only digests and counts may leave the verifier.
use crate::repository::PostgresRepository;
use er_store::Repository;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub struct ReplayEvidence {
    pub event_count: usize,
    pub last_sequence: u64,
    pub events_sha256: String,
    pub note_sha256: String,
}

pub async fn evidence(repo: &PostgresRepository, patient: Uuid) -> anyhow::Result<ReplayEvidence> {
    let events = repo.get_events(patient).await?;
    anyhow::ensure!(!events.is_empty(), "restore_fixture_has_no_events");
    let note_events: Vec<_> = events.iter().map(|row| row.event.clone()).collect();
    // There is no registration event; anchor initial timestamps to the first
    // persisted event rather than PatientNote::new's wall clock.
    let mut replayed = er_domain::PatientNote::new(patient);
    replayed.created_at = events[0].occurred_at;
    replayed.updated_at = events[0].occurred_at;
    for event in &note_events {
        er_domain::replay::apply_event(&mut replayed, event);
    }
    let canonical = serde_json::to_vec(&er_api::NoteView::from(&replayed))?;
    // Event identity, ordering, timestamps and payloads must all survive PITR.
    let mut event_digest = Sha256::new();
    for row in &events {
        event_digest.update(serde_json::to_vec(&(
            row.sequence_id,
            row.event_id,
            &row.event,
            row.occurred_at,
        ))?);
    }
    Ok(ReplayEvidence {
        event_count: events.len(),
        last_sequence: events.last().unwrap().sequence_id,
        events_sha256: format!("{:x}", event_digest.finalize()),
        note_sha256: format!("{:x}", Sha256::digest(canonical)),
    })
}
