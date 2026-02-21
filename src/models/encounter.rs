use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::patient::PatientId;

pub type EncounterId = Uuid;

/// How the audio was captured — affects summarization strategy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CaptureMode {
    /// In the room with the patient — may include patient speech, ambient noise.
    Ambient,
    /// Clinician dictating privately (hallway, workstation) — single-speaker, structured.
    Dictation,
}

/// A single encounter snippet — one recording session for one patient.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Encounter {
    pub id: EncounterId,
    pub patient_id: PatientId,
    pub capture_mode: CaptureMode,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    /// Raw transcript text from STT.
    pub transcript: Option<String>,
    /// Encounter number for this patient (1st visit, 2nd visit, etc.)
    pub encounter_number: u32,
}

impl Encounter {
    pub fn new(patient_id: PatientId, capture_mode: CaptureMode, encounter_number: u32) -> Self {
        Self {
            id: Uuid::new_v4(),
            patient_id,
            capture_mode,
            started_at: Utc::now(),
            ended_at: None,
            transcript: None,
            encounter_number,
        }
    }

    pub fn finish(&mut self, transcript: String) {
        self.ended_at = Some(Utc::now());
        self.transcript = Some(transcript);
    }
}
