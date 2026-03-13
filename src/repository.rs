use crate::models::{PatientId};
use crate::events::{NoteEvent, EventBus};
use crate::error::AppResult;

pub trait Repository: Send + Sync {
    fn append_event(&self, patient_id: PatientId, event: NoteEvent) -> AppResult<()> {
        todo!()
    }

    fn get_events(&self, patient_id: PatientId) -> Vec<NoteEvent> {
        todo!()
    }

    fn get_events_since(&self, patient_id: PatientId, sequence_id: u64) -> Vec<NoteEvent> {
        todo!()
    }

    fn save_snapshot(&self) {
        todo!()
    }

    fn get_snapshot(&self) {
        todo!()
    }

}

pub struct SqliteRepository {
    conn: std::sync::Mutex<rusqlite::Connection>,
}

impl Repository for SqliteRepository {
    fn append_event(&self, patient_id: PatientId, event: NoteEvent) -> AppResult<()> {
        todo!()
    }

    fn get_events(&self, patient_id: PatientId) -> Vec<NoteEvent> {
        todo!()
    }

    fn get_events_since(&self, patient_id: PatientId, sequence_id: u64) -> Vec<NoteEvent> {
        todo!()
    }

}
