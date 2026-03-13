use crate::models::{PatientId};
use crate::events::{NoteEvent, EventBus};

pub trait Repository: Send + Sync {
    fn append_event(&self, patient_id: PatientId, event: NoteEvent) {
        todo!()
    }

    fn get_events(&self, patient_id: PatientId) -> Vec<NoteEvent> {
        todo!()
    }

    fn get_events_since(&self, patient_id: PatientId, sequence_id: u64) -> Vec<NoteEvent> {
        todo!()
    }

}
