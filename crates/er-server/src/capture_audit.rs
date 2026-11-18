use crate::{auth::AuthedClinician, error::ApiError, state::AppState};
use axum::{
    Json,
    extract::{Path, State},
};
use er_api::{CaptureAuditV1, CaptureAuditView, CaptureAuditWrite};
use uuid::Uuid;

pub async fn read(
    State(state): State<AppState>,
    auth: AuthedClinician,
    Path(patient): Path<Uuid>,
) -> Result<Json<CaptureAuditView>, ApiError> {
    let repo = state.repo_for(&auth);
    if !repo.patient_registered(patient).await? {
        return Err(ApiError::NotFound);
    }
    let mut tx = repo.begin_tenant().await?;
    let row:Option<(i64,serde_json::Value)>=sqlx::query_as("SELECT version,payload FROM device_capture_audit WHERE org_id=$1 AND patient_id=$2 AND clinician_id=$3").bind(auth.org_id).bind(patient).bind(auth.clinician_id).fetch_optional(&mut *tx).await?;
    tx.commit().await?;
    let (version, audit) = match row {
        Some((v, p)) => (
            v as u64,
            serde_json::from_value(p).map_err(er_store::StoreError::from)?,
        ),
        None => (0, CaptureAuditV1::default()),
    };
    Ok(Json(CaptureAuditView {
        version,
        clinician_id: auth.clinician_id,
        audit,
    }))
}
pub async fn write(
    State(state): State<AppState>,
    auth: AuthedClinician,
    Path(patient): Path<Uuid>,
    Json(body): Json<CaptureAuditWrite>,
) -> Result<Json<CaptureAuditView>, ApiError> {
    if !body.audit.valid(chrono::Utc::now()) || body.expected_version >= i64::MAX as u64 {
        return Err(ApiError::Validation("invalid_capture_audit"));
    }
    let repo = state.repo_for(&auth);
    if !repo.patient_registered(patient).await? {
        return Err(ApiError::NotFound);
    }
    // A segment ID from another visit cannot be relabeled through audit metadata.
    use er_store::Repository;
    let events = repo.get_events(patient).await?;
    let mut starts = std::collections::HashMap::new();
    let mut stops = std::collections::HashMap::new();
    for row in events {
        match row.event {
            er_domain::NoteEvent::AudioStarted {
                encounter_id,
                capture_mode,
                clinical_occurred_at,
                ..
            } => {
                starts.insert(
                    encounter_id,
                    (capture_mode, clinical_occurred_at.timestamp_millis()),
                );
            }
            er_domain::NoteEvent::AudioStopped {
                encounter_id,
                clinical_occurred_at,
                ..
            } => {
                stops.insert(encounter_id, clinical_occurred_at.timestamp_millis());
            }
            _ => {}
        }
    }
    if body.audit.segments.iter().any(|s| {
        starts.get(&s.id) != Some(&(s.mode.clone(), s.started_at_ms))
            || s.stopped_at_ms
                .is_some_and(|end| stops.get(&s.id) != Some(&end))
    }) {
        return Err(ApiError::Validation("audit_segment_mismatch"));
    }
    let mut tx = repo.begin_tenant().await?;
    // Serialize creation as well as updates: an absent row cannot be FOR UPDATE locked.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!(
            "audit:{}:{}:{}",
            auth.org_id, patient, auth.clinician_id
        ))
        .execute(&mut *tx)
        .await?;
    let row:Option<(i64,serde_json::Value)>=sqlx::query_as("SELECT version,payload FROM device_capture_audit WHERE org_id=$1 AND patient_id=$2 AND clinician_id=$3 FOR UPDATE").bind(auth.org_id).bind(patient).bind(auth.clinician_id).fetch_optional(&mut *tx).await?;
    let (version, old) = match row {
        Some((v, p)) => (
            v as u64,
            serde_json::from_value::<CaptureAuditV1>(p).map_err(er_store::StoreError::from)?,
        ),
        None => (0, CaptureAuditV1::default()),
    };
    // A response can be lost after commit. An identical resend returns the same version.
    let next = if body.audit == old {
        version
    } else {
        if version != body.expected_version || !body.audit.extends(&old) {
            return Err(ApiError::Conflict("capture_audit_conflict"));
        }
        let next = version + 1;
        sqlx::query("INSERT INTO device_capture_audit(org_id,patient_id,clinician_id,version,payload) VALUES($1,$2,$3,$4,$5) ON CONFLICT(org_id,patient_id,clinician_id) DO UPDATE SET version=excluded.version,payload=excluded.payload").bind(auth.org_id).bind(patient).bind(auth.clinician_id).bind(next as i64).bind(serde_json::to_value(&body.audit).map_err(|_|ApiError::Validation("invalid_capture_audit"))?).execute(&mut *tx).await?;
        next
    };
    tx.commit().await?;
    Ok(Json(CaptureAuditView {
        version: next,
        clinician_id: auth.clinician_id,
        audit: body.audit,
    }))
}
