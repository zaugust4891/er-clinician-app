use std::sync::Arc;
use tokio::sync::broadcast;

use crate::domain::{NoteEvent, NotePhase, PatientNote};
use crate::error::AppResult;
use crate::infrastructure::persistence::repository::Repository;
use crate::application::load::PatientNoteStore;
use crate::domain::replay::apply_event;

/// Broadcast event bus — multiple consumers, single producer per event.
/// Persists first before broadcasting to in-memory subscribers - durable before ephemeral
pub struct EventBus {
    sender: broadcast::Sender<NoteEvent>,
    repository: Option<Arc<dyn Repository>>,
}
///
/// EventBus::publish()` takes a `&PatientNoteStore` parameter and updates
/// the in-memory projection under the same control flow that persists the event.
/// Snapshots fire when the resulting phase is `Idle`, `Finalizing`, or `Signed`.

impl EventBus {
    pub fn new(capacity: usize, repository: Option<Arc<dyn Repository>>) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender, repository }
    }

    pub fn publish(&self, event: NoteEvent, store: &PatientNoteStore) -> AppResult<u64> {
        let patient_id = event.extract_patient_id();

        // 1. Persist-first (durable before ephemeral).
        let seq_id = if let Some(repo) = &self.repository {
            repo.append_event(patient_id, event.clone())?
        } else {
            0
        };

        // 2. Synchronously project to the in-memory store.
        // This is creative improvement #7 — the DashMap and SQLite stay consistent because
        // they are updated in the same code path. Broadcast subscribers never touch state.

        let phase_after = {
            let mut entry = store.entry(patient_id).or_insert_with(|| {
                PatientNote::new(patient_id)
            });
            apply_event(&mut *entry, &event);
            entry.phase.clone()
            // DashMap write lock on this shard is dropped - NotePhase escapes

        };

        // 3. Trigger snapshot on significant phase transitions.
        //    Only when we actually persisted (seq_id != 0) and a repo exists.
        if seq_id != 0 {
            if let Some(repo) = &self.repository {
                if matches!(
                    phase_after,
                    NotePhase::Idle | NotePhase::Finalizing | NotePhase::Signed
                ) {
                    // Read the full note back from the store under a short-lived
                    // borrow using store.get(&patient_id )for serialization. save_snapshot serializes
                    // under its own lock, so we release the DashMap lock
                    // first by cloning. Clones the PatientNote out of the ref immediately,
                    // which then drops the read lock before save_snapshot is called.
                    if let Some(snapshot) = store.get(&patient_id).map(|r| r.clone()) {
                        // Best-effort — a snapshot failure should not kill the publish path. Log it and move on.
                        if let Err(e) = repo.save_snapshot(patient_id, seq_id, &snapshot) {
                            tracing::warn!(
                                patient_id = %patient_id,
                                sequence_id = %seq_id,
                                error = ?e,
                                "snapshot save failed - continuing without"
                            );
                        }
                    }
                }
            }
        }

        // 4. Broadcast (notifications only).
        let _ = self.sender.send(event);
        Ok(seq_id)
    }
    // ***NOTE***
    /// Subscribers get notified, but they're explicitly not responsible for state updates.
    /// ***UI code*** reads from the DashMap, which is now guaranteed current.
    pub fn subscribe(&self) -> broadcast::Receiver<NoteEvent> {
        self.sender.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(256, None)
    }
}
