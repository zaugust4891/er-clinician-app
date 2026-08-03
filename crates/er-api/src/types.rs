// (S0: every request/response body, serde-only)

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use er_domain::{Diagnosis, Disposition, Medication, ClinicalResult, NoteEvent, PatientNote};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventUp {
    pub event_id: Uuid,
    pub event: NoteEvent,          
    // externally-tagged JSON, same as the payload column (§C1)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AppendEventsRequest { pub events: Vec<EventUp> }

/// Per-event acknowledgment: the server-assigned position in the stream.
/// On a duplicate delivery this is the ORIGINAL sequence_id — same answer every time.
#[derive(Debug, Serialize, Deserialize)]
pub struct EventAck { pub event_id: Uuid, pub sequence_id: u64 }

#[derive(Debug, Serialize, Deserialize)]
pub struct AppendEventsResponse { pub results: Vec<EventAck> }

/// One event coming down — the wire twin of er_store's StoredEvent envelope.
#[derive(Debug, Serialize, Deserialize)]
pub struct EventDown {
    pub sequence_id: u64,
    pub event_id: Uuid,
    pub event: NoteEvent,
    pub occurred_at: DateTime<Utc>,   
    // clinical time, not server-received time
}

/// A page of the stream. `has_more` tells the client to keep looping (§C3).
#[derive(Debug, Serialize, Deserialize)]
pub struct EventsPage { pub events: Vec<EventDown>, pub has_more: bool }

// ---------- Snapshots (S6) ----------

#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotUp   { pub sequence_id: u64, pub snapshot: PatientNote }
#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotDown { pub sequence_id: u64, pub snapshot: PatientNote }

// ---------- Patients (S3) ----------

/// PHI-minimal BY CONSTRUCTION (§C1): no name, no age, no MRN — no fields to put them in.
/// `display_label` is "Room 4 – chest pain" territory, never an identity.
#[derive(Debug, Serialize, Deserialize)]
pub struct RegisterPatientRequest {
    pub patient_id: Uuid,              // client-stamped, like event_id — see S3 for why
    pub chief_complaint: String,
    pub display_label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatientView {
    pub patient_id: Uuid,
    pub chief_complaint: String,
    pub display_label: Option<String>,
    pub created_at: DateTime<Utc>,
}

// ---------- Shifts (S8) ----------

#[derive(Debug, Serialize, Deserialize)]
pub struct ShiftView {
    pub shift_id: Uuid,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub status: String,                 // "active" | "ended"
    pub patients: Vec<PatientView>,     // ordered by position — ShiftManager's stable-list invariant
}

// ---------- Note projection (S7) ----------

/// The read model for clients. A deliberate DTO (§C1): PatientNote can gain/rename
/// fields freely; only NoteView::from must keep up, and the compiler enforces that.
#[derive(Debug, Serialize, Deserialize)]
pub struct NoteView {
    pub patient_id: Uuid,
    pub phase: String,                  // "Registered" | "Encountering" | "Idle" | ...
    pub hpi: Option<String>,
    pub ros: Option<String>,
    pub exam: Option<String>,
    pub mdm: Option<String>,
    pub disposition: Option<Disposition>,
    pub medications: Vec<Medication>,
    pub diagnoses: Vec<Diagnosis>,
    pub results: Vec<ClinicalResult>,
    pub data_reviewed: Vec<String>,
    pub encounter_count: u32,
    pub version: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<&PatientNote> for NoteView {
    fn from(n: &PatientNote) -> Self {
        Self {
            patient_id: n.patient_id,
            phase: format!("{:?}", n.phase),
            hpi: n.note_sections.hpi.clone(),
            ros: n.note_sections.ros.clone(),
            exam: n.note_sections.exam.clone(),
            mdm: n.note_sections.mdm.clone(),
            disposition: n.disposition.clone(),
            medications: n.medication.clone(),
            diagnoses: n.diagnoses.clone(),
            results: n.results.clone(),
            data_reviewed: n.data_reviewed.clone(),
            encounter_count: n.encounter_count,
            version: n.version,
            created_at: n.created_at,
            updated_at: n.updated_at,
        }
    }
}

// ---------- Ops (S7) ----------

#[derive(Debug, Serialize, Deserialize)]
pub struct StatsView { pub event_count: u64, pub snapshot_count: u64 }