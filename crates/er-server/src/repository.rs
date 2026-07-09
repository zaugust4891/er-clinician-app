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

impl PostgresRepository {
    pub fn for_org(pool: PgPool, org_id: Uuid) -> Self {
        Self { pool, org_id }
    }

    async fn sequence_for(&self, event_id: Uuid) -> StoreResult<u64> {
        let seq = sqlx::query_scalar!(
            "SELECT sequence_id FROM events WHERE event_id = $1",
            event_id,
        )
        .fetch_one(&self.pool)
        .await
        .map_err(StoreError::backend)?;
        Ok(seq as u64)
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
}

#[async_trait]
impl Repository for PostgresRepository {
    async fn append_event(&self, patient_id: PatientId, event_id: Uuid, event: NoteEvent) -> StoreResult<u64> {
        let payload = serde_json::to_value(&event)?;
        let occurred = event.clinical_occurred_at();
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
        .fetch_optional(&self.pool)
        .await
        .map_err(StoreError::backend)?;

        match rec {
            Some(r) => Ok(r.sequence_id as u64),
            None => self.sequence_for(event_id).await,
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
    
