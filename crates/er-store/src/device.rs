//! Versioned local-only metadata. No consent/context/preferences wire format is implied.
use crate::{SqliteRepository, StoreError, StoreResult};
use er_domain::{CaptureMode, NoteEvent, PatientRecord};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConsentStatus {
    Unknown,
    Granted,
    Declined,
    Revoked,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Consent {
    pub status: ConsentStatus,
    pub recorded_at_ms: i64,
    pub actor_scope: String,
    pub policy_version: String,
    pub revision: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RecorderStatus {
    Unavailable,
    Ready,
    Recording,
    Paused,
    Interrupted,
    PendingStop,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Segment {
    pub id: Uuid,
    pub mode: CaptureMode,
    pub source: String,
    pub consent_revision: u64,
    pub started_at_ms: i64,
    pub stopped_at_ms: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ContextCategory {
    History,
    Medication,
    Vital,
    Lab,
    Imaging,
    Treatment,
    Diagnosis,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChartContext {
    pub id: Uuid,
    pub patient_id: Uuid,
    pub version: u64,
    pub category: ContextCategory,
    pub text: String,
    pub source: String,
    pub clinical_at_ms: Option<i64>,
    pub entered_at_ms: i64,
    pub reviewed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Visit {
    pub patient: PatientRecord,
    pub consents: Vec<Consent>,
    pub mode: CaptureMode,
    pub recorder: RecorderStatus,
    pub boundary_at_ms: Option<i64>,
    pub pause_reason: Option<String>,
    pub segments: Vec<Segment>,
    pub context: Vec<ChartContext>,
    pub manual_draft: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceShift {
    pub id: Uuid,
    pub server_shift_id: Option<Uuid>,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    pub selected_patient_id: Option<Uuid>,
    pub visits: Vec<Visit>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum DefinitionKind {
    Template,
    AdmissionMessage,
    DischargeInstructions,
    Custom,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SectionPreference {
    pub section: String,
    pub instructions: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Definition {
    pub id: Uuid,
    pub version: u64,
    pub name: String,
    pub kind: DefinitionKind,
    pub instructions: String,
    pub sections: Vec<SectionPreference>,
    pub archived: bool,
    pub is_default: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceState {
    pub last_control_at_ms: i64,
    pub format_version: u32,
    pub revision: u64,
    pub shifts: Vec<DeviceShift>,
    /// Immutable versions; only a new version changes defaults/archive state.
    pub definitions: Vec<Definition>,
}
impl Default for DeviceState {
    fn default() -> Self {
        Self {
            last_control_at_ms: 0,
            format_version: 1,
            revision: 0,
            shifts: vec![],
            definitions: vec![],
        }
    }
}

impl SqliteRepository {
    /// Scope binding and initial metadata creation are a single transaction.
    pub fn bind_device_scope(&self, scope: &str) -> StoreResult<DeviceState> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT scope FROM device_scope WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if existing.as_deref().is_some_and(|s| s != scope) {
            return Err(StoreError::Scope);
        }
        tx.execute(
            "INSERT OR IGNORE INTO device_scope(singleton,scope) VALUES(1,?1)",
            [scope],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO device_state(singleton,revision,payload) VALUES(1,0,?1)",
            [serde_json::to_string(&DeviceState::default())?],
        )?;
        let (revision, json): (u64, String) = tx.query_row(
            "SELECT revision,payload FROM device_state WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let state: DeviceState = serde_json::from_str(&json)?;
        if state.format_version != 1 {
            return Err(StoreError::Schema);
        }
        if state.revision != revision {
            return Err(StoreError::Integrity);
        }
        tx.commit()?;
        Ok(state)
    }

    /// Compare-and-save the whole typed metadata aggregate; optional clinical
    /// boundary is committed atomically with its consent/source metadata.
    pub fn commit_device_change(
        &self,
        expected: u64,
        state: &DeviceState,
        event: Option<(Uuid, &NoteEvent)>,
    ) -> StoreResult<()> {
        if state.format_version != 1
            || state.revision != expected.checked_add(1).ok_or(StoreError::Integrity)?
        {
            return Err(StoreError::Integrity);
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "UPDATE device_state SET revision=?1,payload=?2 WHERE singleton=1 AND revision=?3",
            params![state.revision, serde_json::to_string(state)?, expected],
        )?;
        if changed != 1 {
            return Err(StoreError::Conflict);
        }
        if let Some((id, event)) = event {
            tx.execute("INSERT INTO events(event_id,patient_id,event_type,payload,occurred_at) VALUES(?1,?2,?3,?4,?5)", params![id.to_string(),event.extract_patient_id().to_string(), event.event_type(), serde_json::to_string(event)?, event.clinical_occurred_at().to_rfc3339()])?;
        }
        tx.commit()?;
        Ok(())
    }
}
