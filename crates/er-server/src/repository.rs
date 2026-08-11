use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use er_domain::{NoteEvent, PatientId, PatientNote};
use er_store::repository::{Repository, StoredEvent};
use er_store::{StoreError, StoreResult, snapshot_checksum};

pub struct PostgresRepository {
    pool: sqlx::PgPool,
    org_id: Uuid,
}

/// The `patients` columns S7's export needs. A named struct rather than a tuple
/// so adding a column later can't silently reorder a caller's bindings.
pub struct PatientRow {
    pub chief_complaint: String,
    pub display_label: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A `shifts` row (S8). Wire-type-free on purpose: the repository stays in storage
/// vocabulary and `routes.rs` maps this onto `er_api::ShiftView`, the same split
/// `PatientRow` uses.
pub struct ShiftRow {
    pub id: Uuid,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub status: String,
}

/// One patient on a shift's roster, already in `position` order.
pub struct RosterEntry {
    pub patient_id: Uuid,
    pub chief_complaint: String,
    pub display_label: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Name of the partial unique index from the S1 migration. It is the referee for
/// "one active shift per clinician" — the check is the database's, not ours, so
/// two simultaneous starts cannot both win.
const ONE_ACTIVE_SHIFT_INDEX: &str = "one_active_shift_per_clinician";

/// Did this failure come from a clinician trying to open a second active shift?
///
/// `one_active_shift_per_clinician` is a partial unique INDEX, not a table
/// CONSTRAINT, so Postgres may leave the constraint field of the error empty and
/// name the index only in the message. Accept either signal rather than betting on
/// which one this server version fills in — but require SQLSTATE 23505 first, so
/// an unrelated error whose text happens to mention the index can't match.
fn is_active_shift_violation(e: &sqlx::Error) -> bool {
    let Some(db) = e.as_database_error() else { return false };
    if db.code().as_deref() != Some("23505") {
        return false; // not unique_violation
    }
    db.constraint() == Some(ONE_ACTIVE_SHIFT_INDEX) || db.message().contains(ONE_ACTIVE_SHIFT_INDEX)
}

impl PostgresRepository {
    pub fn for_org(pool: PgPool, org_id: Uuid) -> Self {
        Self { pool, org_id }
    }

    async fn sequence_for(&self, patient_id: PatientId, event_id: Uuid) -> StoreResult<u64> {
        let seq = sqlx::query_scalar!(
            "SELECT sequence_id FROM events WHERE event_id = $1 AND org_id = $2 AND patient_id = $3",
            event_id, self.org_id, patient_id,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::backend)?;

        match seq {
            Some(s) => Ok(s as u64),
            None => Err(StoreError::Backend(
                "event_id conflict outside this org/patient - refusing".into(),
            )),
        }
    }

    fn to_stored_events_struct(
        sequence_id: i64,
        event_id: Uuid,
        payload: serde_json::Value,
        occurred_at: DateTime<Utc>,
    ) -> StoreResult<StoredEvent>
    {
        let event: NoteEvent = serde_json::from_value(payload)?;
        Ok(StoredEvent { sequence_id: sequence_id as u64, event_id, event, occurred_at })
        
    }

    pub async fn patient_registered(&self, patient_id: Uuid) -> StoreResult<bool> {
        let db_hit = sqlx::query_scalar!(
            "SELECT 1 AS one FROM patients WHERE id = $1 AND org_id = $2",
            patient_id, self.org_id,
        ).fetch_optional(&self.pool).await.map_err(StoreError::backend)?;
        Ok(db_hit.is_some())
    }

    /// The PHI-minimal patient row, org-scoped. `note/export` (S7) needs the
    /// chief complaint and room label to fill a `PatientRecord`; there is
    /// deliberately nothing else here to fetch — no name, no age, no MRN (§C1).
    ///
    /// Returns `None` rather than erroring on a missing row so the caller keeps
    /// the same 404-means-absent vocabulary as every other per-patient route.
    pub async fn patient_row(&self, patient_id: Uuid) -> StoreResult<Option<PatientRow>> {
        let row = sqlx::query!(
            "SELECT chief_complaint, display_label, created_at
               FROM patients WHERE id = $1 AND org_id = $2",
            patient_id, self.org_id,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::backend)?;

        Ok(row.map(|r| PatientRow {
            chief_complaint: r.chief_complaint,
            display_label: r.display_label,
            created_at: r.created_at,
        }))
    }

    // ---- Shifts (S8) ------------------------------------------------------

    /// Open a shift for `clinician_id`. Returns `Ok(None)` when one is already
    /// active — the partial unique index refereed it, and "you're already in a
    /// shift" is an answer, not a failure. Every other database error propagates.
    pub async fn create_shift(&self, clinician_id: Uuid) -> StoreResult<Option<ShiftRow>> {
        let result = sqlx::query!(
            r#"INSERT INTO shifts (id, org_id, clinician_id, status)
               VALUES ($1, $2, $3, 'active')
               RETURNING id, started_at, ended_at, status"#,
            Uuid::now_v7(), self.org_id, clinician_id,
        )
        .fetch_one(&self.pool)
        .await;

        match result {
            Ok(r) => Ok(Some(ShiftRow {
                id: r.id,
                started_at: r.started_at,
                ended_at: r.ended_at,
                status: r.status,
            })),
            Err(e) if is_active_shift_violation(&e) => Ok(None),
            Err(e) => Err(StoreError::backend(e)),
        }
    }

    pub async fn get_shift(&self, shift_id: Uuid) -> StoreResult<Option<ShiftRow>> {
        let row = sqlx::query!(
            "SELECT id, started_at, ended_at, status
               FROM shifts WHERE id = $1 AND org_id = $2",
            shift_id, self.org_id,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::backend)?;

        Ok(row.map(|r| ShiftRow {
            id: r.id,
            started_at: r.started_at,
            ended_at: r.ended_at,
            status: r.status,
        }))
    }

    /// "What am I in the middle of?" — scoped to the caller's own clinician_id,
    /// which comes from the token and is never a parameter (§C6).
    pub async fn active_shift(&self, clinician_id: Uuid) -> StoreResult<Option<ShiftRow>> {
        let row = sqlx::query!(
            "SELECT id, started_at, ended_at, status
               FROM shifts
              WHERE clinician_id = $1 AND org_id = $2 AND status = 'active'",
            clinician_id, self.org_id,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::backend)?;

        Ok(row.map(|r| ShiftRow {
            id: r.id,
            started_at: r.started_at,
            ended_at: r.ended_at,
            status: r.status,
        }))
    }

    /// End a shift, idempotently. The UPDATE is conditional on `status = 'active'`,
    /// so a retry matches no row and `ended_at` is stamped exactly once — a second
    /// call returns the identical body rather than sliding the timestamp forward
    /// (§C2: every mutation a client might resend answers the same way twice).
    ///
    /// `Ok(None)` means missing or another org's — the caller turns that into 404.
    pub async fn end_shift(&self, shift_id: Uuid) -> StoreResult<Option<ShiftRow>> {
        let updated = sqlx::query!(
            r#"UPDATE shifts SET ended_at = now(), status = 'ended'
                WHERE id = $1 AND org_id = $2 AND status = 'active'
            RETURNING id, started_at, ended_at, status"#,
            shift_id, self.org_id,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::backend)?;

        if let Some(r) = updated {
            return Ok(Some(ShiftRow {
                id: r.id,
                started_at: r.started_at,
                ended_at: r.ended_at,
                status: r.status,
            }));
        }
        // Matched nothing: either already ended (hand it back unchanged) or
        // missing/foreign (None). One org-scoped read settles which.
        self.get_shift(shift_id).await
    }

    /// The shift's patients in `position` order — ShiftManager's stable-list
    /// invariant, now enforced by the database rather than by in-memory ordering.
    pub async fn shift_roster(&self, shift_id: Uuid) -> StoreResult<Vec<RosterEntry>> {
        let rows = sqlx::query!(
            "SELECT p.id, p.chief_complaint, p.display_label, p.created_at
               FROM shift_patients sp
               JOIN patients p ON p.id = sp.patient_id
              WHERE sp.shift_id = $1 AND p.org_id = $2
              ORDER BY sp.position ASC",
            shift_id, self.org_id,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::backend)?;

        Ok(rows
            .into_iter()
            .map(|r| RosterEntry {
                patient_id: r.id,
                chief_complaint: r.chief_complaint,
                display_label: r.display_label,
                created_at: r.created_at,
            })
            .collect())
    }

    /// Idempotent insert + attach to the clinician's active shift, atomically.
    /// A transaction makes it both-or-neither: no registered-but-unlisted
    /// patients if the process dies between the two inserts.
    pub async fn register_patient(
        &self,
        patient_id: Uuid,
        created_by: Uuid,
        chief_complaint: &str,
        display_label: Option<&str>,
    ) -> StoreResult<()> {
        let mut tx = self.pool.begin().await.map_err(StoreError::backend)?;

        sqlx::query!(
            r#"INSERT INTO patients (id, org_id, created_by, chief_complaint, display_label)
               VALUES ($1, $2, $3, $4, $5)
               ON CONFLICT (id) DO NOTHING"#,             // retry-safe (§C2)
            patient_id, self.org_id, created_by, chief_complaint, display_label,
        ).execute(&mut *tx).await.map_err(StoreError::backend)?;

        // Attach to the active shift if one exists; next free position.
        // COALESCE(x, 0) = "x, or 0 if x is NULL" — first patient gets position 0.
        sqlx::query!(
            r#"INSERT INTO shift_patients (shift_id, patient_id, position)
               SELECT s.id, $2,
                      COALESCE((SELECT MAX(sp.position) + 1
                                  FROM shift_patients sp WHERE sp.shift_id = s.id), 0)
                 FROM shifts s
                WHERE s.clinician_id = $1 AND s.status = 'active'
               ON CONFLICT DO NOTHING"#,
            created_by, patient_id,
        ).execute(&mut *tx).await.map_err(StoreError::backend)?;

        tx.commit().await.map_err(StoreError::backend)?;
        Ok(())
    }
}

#[async_trait]
impl Repository for PostgresRepository {
    async fn append_event(&self, patient_id: PatientId, event_id: Uuid, event: NoteEvent) -> StoreResult<u64> {
        let payload = serde_json::to_value(&event)?;
        let occurred = event.clinical_occurred_at();
        let mut tx = self.pool.begin().await.map_err(StoreError::backend)?;

        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(patient_id.to_string())
            .execute(&mut *tx).await.map_err(StoreError::backend)?; 


        let rec = sqlx::query!(
            r#"INSERT INTO events
                    (event_id, org_id, patient_id, event_type, payload, clinical_occurred_at)
                VALUES ($1, $2, $3, $4, $5, $6)
                ON CONFLICT (event_id) DO NOTHING
                RETURNING sequence_id"#,
                event_id,
                self.org_id,
                patient_id,
                event.event_type(),
                payload,
                occurred,
        )
        .fetch_optional(&mut *tx).await.map_err(StoreError::backend)?;
        
        tx.commit().await.map_err(StoreError::backend)?;

        match rec {
            Some(r) => Ok(r.sequence_id as u64),
            None => self.sequence_for(patient_id, event_id).await,
        }
    }

    async fn get_events(&self, patient_id: PatientId) -> StoreResult<Vec<StoredEvent>> {
        let rows = sqlx::query!(
            r#"
            SELECT
                sequence_id,
                event_id,
                payload,
                clinical_occurred_at
            FROM events
            WHERE org_id = $1 AND patient_id = $2 
            ORDER BY sequence_id ASC
            "#,
            self.org_id,
            patient_id,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::backend)?;
        
        rows.into_iter()
            .map(|r| Self::to_stored_events_struct(r.sequence_id, r.event_id, r.payload, r.clinical_occurred_at))
            .collect()
        
    }

    async fn get_events_since(&self, patient_id: PatientId, sequence_id: u64) -> StoreResult<Vec<StoredEvent>> {
        let rows = sqlx::query!(
            r#"
            SELECT
                sequence_id,
                event_id,
                payload,
                clinical_occurred_at
            FROM events
            WHERE org_id = $1 AND patient_id = $2 AND sequence_id > $3
            ORDER BY sequence_id ASC
            "#,
            self.org_id,
            patient_id,
            sequence_id as i64,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(StoreError::backend)?;
        
        rows.into_iter()
            .map(|r| Self::to_stored_events_struct(r.sequence_id, r.event_id, r.payload, r.clinical_occurred_at))
            .collect()
        
    }

    async fn save_snapshot(
        &self,
        patient_id: PatientId,
        sequence_id: u64,
        snapshot: &PatientNote,
    ) -> StoreResult<()>
    {
        let snapshot_json = serde_json::to_string(snapshot)?;
        let checksum = snapshot_checksum(&snapshot_json);

        sqlx::query!(
            r#"
            INSERT INTO snapshots (patient_id, org_id, sequence_id, snapshot, checksum, created_at)
            VALUES ($1, $2, $3, $4, $5, now())
            ON CONFLICT(patient_id) DO UPDATE
                SET org_id = EXCLUDED.org_id,
                    sequence_id = EXCLUDED.sequence_id,
                    snapshot = EXCLUDED.snapshot,
                    checksum = EXCLUDED.checksum,
                    created_at = EXCLUDED.created_at
            "#,
            patient_id,
            self.org_id,
            sequence_id as i64,
            snapshot_json,
            checksum,
        )
        .execute(&self.pool)
        .await
        .map_err(StoreError::backend)?;
        
        Ok(())
        
    }

    async fn get_latest_snapshot(
        &self,
        patient_id: PatientId,
    ) -> StoreResult<Option<(u64, PatientNote)>> {
        let rec = sqlx::query!(
            r#"SELECT sequence_id, snapshot, checksum
               FROM snapshots
               WHERE org_id = $1 AND patient_id = $2"#,
            self.org_id,
            patient_id,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::backend)?;

        let Some(r) = rec else { return Ok(None); };   // no snapshot → caller does full replay

        // Integrity check, identical to SQLite. Mismatch ⇒ treat as "no snapshot"
        // so load_patient_note falls back to replaying the event stream.
        let computed = snapshot_checksum(&r.snapshot);
        if computed != r.checksum {
            tracing::warn!(
                patient_id = %patient_id,
                sequence_id = r.sequence_id,
                "snapshot checksum mismatch — falling back to full replay"
            );
            return Ok(None);
        }

        let mut note: PatientNote = serde_json::from_str(&r.snapshot)?;
        note.hydrate();   // rebuild the #[serde(skip)] state trait object from the phase enum
        Ok(Some((r.sequence_id as u64, note)))
    }

    async fn event_count(&self) -> StoreResult<usize> {
        let n = sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!" FROM events WHERE org_id = $1"#,
            self.org_id,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::backend)?;
        Ok(n as usize)
    }

    async fn snapshot_count(&self) -> StoreResult<usize> {
        let n = sqlx::query_scalar!(
            r#"SELECT COUNT(*) AS "count!" FROM snapshots WHERE org_id = $1"#,
            self.org_id,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::backend)?;
        Ok(n as usize)
    }

}
    
