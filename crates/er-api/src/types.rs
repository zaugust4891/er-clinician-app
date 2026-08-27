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
pub struct AppendEventsRequest { 
    pub events: Vec<EventUp> 
}

/// Per-event acknowledgment: the server-assigned position in the stream.
/// On a duplicate delivery this is the ORIGINAL sequence_id — same answer every time.
#[derive(Debug, Serialize, Deserialize)]
pub struct EventAck { 
    pub event_id: Uuid,
    pub sequence_id: u64 
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AppendEventsResponse { 
    pub results: Vec<EventAck>
}

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
pub struct EventsPage { 
    pub events: Vec<EventDown>, 
    pub has_more: bool
}

// ---------- Snapshots (S6) ----------

#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotUp   { 
    pub sequence_id: u64,
    pub snapshot: PatientNote
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SnapshotDown { 
    pub sequence_id: u64,
    pub snapshot: PatientNote
}

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
pub struct StatsView {
    pub event_count: u64,
    pub snapshot_count: u64
}
// ---------- Audio chunks (Step 6 S2) ----------

/// The five states an `audio_chunks` row moves through, as they appear on the wire.
///
/// [`ChunkView::status`] is a plain `String` on purpose — house style, like
/// [`ShiftView::status`] and [`NoteView::phase`]. A Rust enum would make an older
/// client *fail to deserialize* a status it has never heard of, turning an additive
/// server change into a client outage. The cost of that choice is that the legal
/// values would otherwise live only in a comment, so they live here instead: in the
/// one crate both sides compile against, rather than as bare literals scattered
/// across the routes (S3/S4), the worker (S7), and the CLI's poll loop (S8).
///
/// The database remains the referee for *transitions* — the S0 migration's
/// `UPDATE … WHERE status = 'pending'` guards decide what may move where. These
/// constants only name the states for the code that reads them.
pub mod chunk_status {
    /// The ledger row exists and a presigned URL was issued; object storage is
    /// still empty. Rows precede bytes, which is why `pending` exists at all (§C1).
    pub const PENDING: &str = "pending";
    /// The device's `PUT` landed and `HeadObject` witnessed it (S4).
    pub const UPLOADED: &str = "uploaded";
    /// A worker holds the SQS lease and is talking to the STT provider (S7).
    pub const TRANSCRIBING: &str = "transcribing";
    /// Terminal, happy: the chunk's transcript is stored server-side.
    pub const DONE: &str = "done";
    /// Terminal, sad: `error` is set and this segment becomes a visible gap in
    /// the assembled transcript — best-effort, never lose the encounter (§C2).
    pub const FAILED: &str = "failed";

    /// Has this chunk stopped moving?
    ///
    /// Assembly (§C3) waits for every index `0..=final` to be terminal, and the
    /// CLI's polling loop (S8) stops on the same condition. One definition means
    /// the worker and the device cannot disagree about when a chunk is finished.
    pub fn is_terminal(status: &str) -> bool {
        status == DONE || status == FAILED
    }
}

/// Ask permission to upload one chunk of an encounter's audio.
///
/// The `chunk_id` is client-stamped (D5.3 doctrine, same as `event_id` and
/// `patient_id`), so re-sending this request is a *re-send*, not a second chunk:
/// the server answers out of the ledger row the first attempt created.
/// `chunk_index` + `is_final` are the §C3 ordering contract, and `content_type`
/// gets signed into the returned URL — whatever you declare here is exactly what
/// your `PUT` must send, byte for byte, or S3 rejects the signature (§C1).
///
/// PHI-minimal by construction (§C5): there is no field for a filename, a label,
/// or any human text — four scalars, two of them opaque ids.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioUploadRequest {
    pub chunk_id: Uuid,
    /// 0-based and contiguous per encounter. `u32`, not the row's `i32`: this is
    /// the one audio field a client *sends*, so serde refusing `-1` at the door is
    /// validation the handler then never has to write.
    pub chunk_index: u32,
    /// Exactly one chunk per encounter carries `true` — the one cut when the
    /// clinician tapped stop, so its index is the highest. Finality is data on a
    /// chunk rather than a separate RPC, which is what lets it survive retries and
    /// out-of-order arrival like everything else (§C3).
    pub is_final: bool,
    /// From S3's allowlist: `audio/wav` | `audio/ogg` | `audio/mp4` | `audio/mpeg`.
    /// Declared per chunk; nothing in the pipeline hardcodes a container (D6.15).
    pub content_type: String,
}

/// The valet key (§C1).
///
/// Nothing here reveals anything about the patient, but `upload_url` *is* a bearer
/// credential for one `PUT` to one key: log the `chunk_id`, never the URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioUploadResponse {
    /// Echoed back so a client that pipelines several intents can match answers to
    /// requests without relying on ordering.
    pub chunk_id: Uuid,
    /// Presigned `PUT` straight to object storage — the API never handles a byte of
    /// audio, and the URL carries its own authorization.
    pub upload_url: String,
    /// 900 (15 minutes). A client that has sat on a URL should re-ask rather than
    /// hoard it: re-issuing an intent is idempotent and costs one round trip.
    pub expires_in_secs: u64,
}

/// One chunk's ledger state. The completion ack and the status read (both S4,
/// driven by S8's poll loop) return this same shape, so a client has one thing to
/// learn and one thing to render.
///
/// Deliberately *not* a mirror of the `audio_chunks` row: `org_id`, `s3_key`, and
/// the per-chunk `transcript` stay server-side. The device already knows its org,
/// has no use for the key, and does not read transcripts here at all — it reads
/// them the way it reads everything else, as a worker-authored `TranscriptReady`
/// event off the S5 cursor, once the *whole* encounter has been assembled (§C4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkView {
    pub chunk_id: Uuid,
    pub encounter_id: Uuid,
    pub chunk_index: u32,
    pub is_final: bool,
    /// One of [`chunk_status`]. Unrecognized values are possible against a newer
    /// server and should be treated as "still moving", not as an error.
    pub status: String,
    /// Audio length in milliseconds, from the STT provider's metadata; `None` until
    /// a worker finishes the chunk. Step 7's `usage_log` meters from this (D6.13).
    ///
    /// Stays `i32` — the row's own type — while `chunk_index` widens to `u32`,
    /// because the two have different provenance. `chunk_index` came *from* a `u32`
    /// this server accepted, so converting it back is provably safe; `duration_ms`
    /// is written by the worker under no such guarantee, and keeping it in the
    /// storage type means the boundary needs no fallible cast for a value nobody
    /// does arithmetic on.
    pub duration_ms: Option<i32>,
    /// Terminal failure reason, `None` while healthy. Operator-facing text for the
    /// §C2 autopsy — never transcript content.
    pub error: Option<String>,
}

#[cfg(test)]
mod audio_wire_tests {
    use super::*;

    /// The field NAMES are the contract — a rename here is a breaking protocol
    /// change even though Rust would compile it happily. Pin them against a
    /// hand-written JSON object rather than a round-trip, which would pass even if
    /// both sides drifted together.
    #[test]
    fn upload_request_field_names_are_the_wire_names() {
        let json = serde_json::json!({
            "chunk_id": "0198f1c0-0000-7000-8000-000000000001",
            "chunk_index": 0,
            "is_final": true,
            "content_type": "audio/wav",
        });

        let req: AudioUploadRequest = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(req.chunk_index, 0);
        assert!(req.is_final);
        assert_eq!(req.content_type, "audio/wav");
        assert_eq!(serde_json::to_value(&req).unwrap(), json);
    }

    /// The reason `chunk_index` is `u32` and not the row's `i32`: a nonsense index
    /// dies in the deserializer, so no handler has to remember to check for it.
    #[test]
    fn negative_chunk_index_is_rejected_at_the_door() {
        let json = serde_json::json!({
            "chunk_id": "0198f1c0-0000-7000-8000-000000000001",
            "chunk_index": -1,
            "is_final": false,
            "content_type": "audio/wav",
        });

        assert!(serde_json::from_value::<AudioUploadRequest>(json).is_err());
    }

    /// `duration_ms` and `error` are absent for most of a chunk's life. They must
    /// decode as `None` from an explicit `null` AND from a missing key, because the
    /// server writes one and hand-driven curl (E4) writes the other.
    #[test]
    fn chunk_view_tolerates_null_and_missing_optionals() {
        let explicit_null = serde_json::json!({
            "chunk_id": "0198f1c0-0000-7000-8000-000000000001",
            "encounter_id": "0198f1c0-0000-7000-8000-000000000002",
            "chunk_index": 2,
            "is_final": true,
            "status": "pending",
            "duration_ms": null,
            "error": null,
        });
        let view: ChunkView = serde_json::from_value(explicit_null).unwrap();
        assert_eq!(view.duration_ms, None);
        assert_eq!(view.error, None);

        let populated = serde_json::json!({
            "chunk_id": "0198f1c0-0000-7000-8000-000000000001",
            "encounter_id": "0198f1c0-0000-7000-8000-000000000002",
            "chunk_index": 2,
            "is_final": true,
            "status": "done",
            "duration_ms": 42680,
        });
        let view: ChunkView = serde_json::from_value(populated).unwrap();
        assert_eq!(view.duration_ms, Some(42_680));
        assert_eq!(view.status, chunk_status::DONE);
    }

    /// Assembly (§C3) and the CLI poll loop (S8) both branch on this one function.
    #[test]
    fn only_done_and_failed_are_terminal() {
        assert!(chunk_status::is_terminal(chunk_status::DONE));
        assert!(chunk_status::is_terminal(chunk_status::FAILED));

        for moving in [
            chunk_status::PENDING,
            chunk_status::UPLOADED,
            chunk_status::TRANSCRIBING,
        ] {
            assert!(!chunk_status::is_terminal(moving), "{moving} is not terminal");
        }

        // A status from a newer server: unknown means "still moving", never a panic
        // and never an accidental "finished" that would cut assembly short.
        assert!(!chunk_status::is_terminal("some_future_status"));
    }
}
