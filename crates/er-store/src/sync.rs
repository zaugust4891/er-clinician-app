//! Durable delivery records. Clinical events keep their original IDs and bodies.
use crate::{SqliteRepository, StoreError, StoreResult, StoredEvent};
use er_domain::NoteEvent;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncBinding {
    pub origin: String,
    pub organization: Uuid,
    pub clinician: Uuid,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PatientSync {
    pub history_loaded: bool,
    pub registered: bool,
    pub cursor: u64,
    pub push_next: bool,
    pub next_poll_ms: i64,
    pub audit_version: u64,
    pub audit_json: Option<String>,
    pub audit_loaded: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ShiftSync {
    pub create_ready: bool,
    pub ended: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub enum SyncFormat {
    #[default]
    V1,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SyncLedger {
    pub format: SyncFormat,
    pub binding: Option<SyncBinding>,
    pub patients: BTreeMap<Uuid, PatientSync>,
    pub shifts: BTreeMap<Uuid, ShiftSync>,
    pub requests_ms: Vec<i64>,
    pub retry_at_ms: i64,
    pub failures: u32,
    pub status: String,
    pub last_patient: Option<Uuid>,
    pub discovery_at_ms: i64,
}
#[derive(Clone)]
pub struct RemoteEvent {
    pub sequence: u64,
    pub id: Uuid,
    pub event: NoteEvent,
}
#[derive(Clone, Debug, Default)]
pub struct DeliveryCounts {
    pub queued: u64,
    pub awaiting_download: u64,
    pub conflicts: u64,
}

fn checked_sequence(n: u64) -> StoreResult<i64> {
    if n == 0 || n > i64::MAX as u64 {
        Err(StoreError::Integrity)
    } else {
        Ok(n as i64)
    }
}
fn write_ledger(conn: &rusqlite::Connection, ledger: &SyncLedger) -> StoreResult<()> {
    conn.execute("INSERT INTO device_sync(singleton,payload) VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET payload=excluded.payload",[serde_json::to_string(ledger)?])?;
    Ok(())
}
impl SqliteRepository {
    pub fn sync_ledger(&self) -> StoreResult<SyncLedger> {
        let conn = self.conn.lock().unwrap();
        let json: Option<String> = conn
            .query_row(
                "SELECT payload FROM device_sync WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        json.map(|j| serde_json::from_str(&j).map_err(Into::into))
            .unwrap_or_else(|| Ok(SyncLedger::default()))
    }
    pub fn save_sync_ledger(&self, ledger: &SyncLedger) -> StoreResult<()> {
        write_ledger(&self.conn.lock().unwrap(), ledger)
    }
    pub fn pending_events(&self, patient: Uuid) -> StoreResult<Vec<StoredEvent>> {
        self.query_stored_events("SELECT e.sequence_id,e.event_id,e.payload,e.occurred_at FROM events e JOIN device_event_delivery d ON d.event_id=e.event_id WHERE e.patient_id=?1 AND d.server_sequence IS NULL AND d.conflict IS NULL ORDER BY e.sequence_id LIMIT 500",[patient.to_string()])
    }
    pub fn delivery_counts(&self, patient: Uuid) -> StoreResult<DeliveryCounts> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row("SELECT COALESCE(SUM(d.server_sequence IS NULL AND d.conflict IS NULL),0),COALESCE(SUM(d.server_sequence IS NOT NULL AND d.downloaded=0),0),COALESCE(SUM(d.conflict IS NOT NULL AND d.conflict!='discarded'),0) FROM device_event_delivery d JOIN events e ON e.event_id=d.event_id WHERE e.patient_id=?1",[patient.to_string()],|r|Ok(DeliveryCounts{queued:r.get(0)?,awaiting_download:r.get(1)?,conflicts:r.get(2)?}))?)
    }
    /// Acknowledgments do not advance the download cursor. A partial response
    /// acknowledges only its listed IDs; the remainder stays queued unchanged.
    pub fn acknowledge_events(
        &self,
        patient: Uuid,
        sent: &[Uuid],
        acks: &[(Uuid, u64)],
        ledger: &SyncLedger,
    ) -> StoreResult<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let sent: HashSet<_> = sent.iter().copied().collect();
        let mut seen = HashSet::new();
        for (id, sequence) in acks {
            let sequence = checked_sequence(*sequence)?;
            if !sent.contains(id) || !seen.insert(*id) {
                return Err(StoreError::Integrity);
            }
            let (owner,existing):(String,Option<i64>)=tx.query_row("SELECT e.patient_id,d.server_sequence FROM events e JOIN device_event_delivery d ON e.event_id=d.event_id WHERE e.event_id=?1",[id.to_string()],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if owner != patient.to_string() || existing.is_some_and(|s| s != sequence) {
                return Err(StoreError::Integrity);
            }
            tx.execute(
                "UPDATE device_event_delivery SET server_sequence=?1 WHERE event_id=?2",
                params![sequence, id.to_string()],
            )?;
        }
        tx.execute(
            "DELETE FROM snapshots WHERE patient_id=?1",
            [patient.to_string()],
        )?;
        write_ledger(&tx, ledger)?;
        tx.commit()?;
        Ok(())
    }
    /// One page, its ID/order mappings, and its cursor are one transaction.
    pub fn receive_page(
        &self,
        patient: Uuid,
        old_cursor: u64,
        page: &[RemoteEvent],
        has_more: bool,
        mut ledger: SyncLedger,
    ) -> StoreResult<()> {
        if page.len() > 500 || (has_more && page.is_empty()) {
            return Err(StoreError::Integrity);
        }
        let current = ledger.patients.entry(patient).or_default();
        if current.cursor != old_cursor {
            return Err(StoreError::Conflict);
        }
        let mut cursor = old_cursor;
        let mut ids = HashSet::new();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        for row in page {
            let sequence = checked_sequence(row.sequence)?;
            if row.sequence <= cursor
                || row.event.extract_patient_id() != patient
                || row.id.is_nil()
                || !ids.insert(row.id)
            {
                return Err(StoreError::Integrity);
            }
            let body = serde_json::to_string(&row.event)?;
            let existing: Option<(String, String)> = tx
                .query_row(
                    "SELECT patient_id,payload FROM events WHERE event_id=?1",
                    [row.id.to_string()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if existing.is_some_and(|(p, b)| p != patient.to_string() || b != body) {
                return Err(StoreError::Conflict);
            }
            let mapped: Option<Option<i64>> = tx
                .query_row(
                    "SELECT server_sequence FROM device_event_delivery WHERE event_id=?1",
                    [row.id.to_string()],
                    |r| r.get(0),
                )
                .optional()?;
            if mapped.flatten().is_some_and(|s| s != sequence) {
                return Err(StoreError::Integrity);
            }
            tx.execute("INSERT OR IGNORE INTO events(event_id,patient_id,event_type,payload,occurred_at) VALUES(?1,?2,?3,?4,?5)",params![row.id.to_string(),patient.to_string(),row.event.event_type(),body,row.event.clinical_occurred_at().to_rfc3339()])?;
            tx.execute("INSERT INTO device_event_delivery(event_id,server_sequence,downloaded) VALUES(?1,?2,1) ON CONFLICT(event_id) DO UPDATE SET server_sequence=excluded.server_sequence,downloaded=1",params![row.id.to_string(),sequence])?;
            cursor = row.sequence;
        }
        current.history_loaded = !has_more;
        current.cursor = cursor;
        current.push_next = !has_more;
        current.next_poll_ms = 0;
        tx.execute(
            "DELETE FROM snapshots WHERE patient_id=?1",
            [patient.to_string()],
        )?;
        write_ledger(&tx, &ledger)?;
        tx.commit()?;
        Ok(())
    }
    pub fn mark_sync_conflict(&self, event: Uuid, reason: &str) -> StoreResult<()> {
        self.conn.lock().unwrap().execute("UPDATE device_event_delivery SET conflict=?1 WHERE event_id=?2 AND server_sequence IS NULL",params![reason,event.to_string()])?;
        Ok(())
    }
    pub fn discard_sync_conflict(&self, patient: Uuid, event: Uuid) -> StoreResult<()> {
        let conn = self.conn.lock().unwrap();
        let changed=conn.execute("UPDATE device_event_delivery SET conflict='discarded' WHERE event_id=?1 AND conflict IS NOT NULL AND conflict!='discarded' AND EXISTS(SELECT 1 FROM events WHERE event_id=?1 AND patient_id=?2)",params![event.to_string(),patient.to_string()])?;
        if changed != 1 {
            return Err(StoreError::Conflict);
        }
        Ok(())
    }
    pub fn sync_conflicts(&self, patient: Uuid) -> StoreResult<Vec<(String, String)>> {
        let conn = self.conn.lock().unwrap();
        let mut query=conn.prepare("SELECT d.event_id,d.conflict FROM device_event_delivery d JOIN events e ON e.event_id=d.event_id WHERE e.patient_id=?1 AND d.conflict IS NOT NULL AND d.conflict!='discarded' ORDER BY e.sequence_id")?;
        let rows = query.query_map([patient.to_string()], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
    pub fn ordered_delivery(&self, patient: Uuid) -> StoreResult<Vec<(StoredEvent, bool)>> {
        let rows=self.query_stored_events("SELECT e.sequence_id,e.event_id,e.payload,e.occurred_at FROM events e LEFT JOIN device_event_delivery d ON e.event_id=d.event_id WHERE e.patient_id=?1 AND d.conflict IS NULL ORDER BY d.server_sequence IS NULL,d.server_sequence,e.sequence_id",[patient.to_string()])?;
        let conn = self.conn.lock().unwrap();
        rows.into_iter().map(|e|{let pending:bool=conn.query_row("SELECT server_sequence IS NULL FROM device_event_delivery WHERE event_id=?1",[e.event_id.to_string()],|r|r.get(0)).optional()?.unwrap_or(false);Ok((e,pending))}).collect()
    }
}
