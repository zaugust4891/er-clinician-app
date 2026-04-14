use crate::{audio::AudioCapture, stt::Transcriber};
use crate::events::EventBus;
use crate::models::{PatientId, EncounterId};

use std::sync::Arc;
use chrono::{DateTime, Utc};

pub struct EncounterDricer<A: AudioCapture, T: Transcriber> {
    audio: A,
    transcriber: T,
    bus: Arc<EventBus>,
    store: PatientNoteStore,
    active_encounter: Option<ActiveEncounter>,
}

pub struct ActiveEncounter {
    patient_id: PatientId,
    encounter_id: EncounterId,
    started_at: DateTime<Utc>,
}

pub struct PatientNoteStore;
