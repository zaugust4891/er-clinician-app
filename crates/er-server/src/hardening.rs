//! Authenticated quotas and durable read/export access-attempt evidence.
use crate::{auth::AuthedClinician, repository::PostgresRepository};
use axum::{
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use uuid::Uuid;

/// Fixed UTC minute windows, atomically shared by every API process. A denied
/// bucket does not increment. General admission is consumed even if an expensive
/// operation is denied; retries are requests too. No client-supplied identity.
pub async fn admit(
    repo: &PostgresRepository,
    clinician: Uuid,
    bucket: &str,
    limit: i32,
) -> Result<bool, sqlx::Error> {
    let mut tx = repo.begin_tenant().await?;
    let admitted = sqlx::query_scalar::<_, i32>(
        "INSERT INTO request_quotas (org_id, clinician_id, bucket, window_start, used)
         VALUES (current_setting('app.org_id')::uuid, $1, $2, date_trunc('minute', statement_timestamp()), 1)
         ON CONFLICT (org_id, clinician_id, bucket) DO UPDATE
         SET window_start = EXCLUDED.window_start,
             used = CASE WHEN request_quotas.window_start < EXCLUDED.window_start THEN 1 ELSE request_quotas.used + 1 END
         WHERE request_quotas.window_start < EXCLUDED.window_start OR request_quotas.used < $3
         RETURNING used"
    ).bind(clinician).bind(bucket).bind(limit).fetch_optional(&mut *tx).await?.is_some();
    tx.commit().await?;
    Ok(admitted)
}

pub async fn record_read(
    repo: &PostgresRepository,
    clinician: Uuid,
    route: &str,
    resource: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    let mut tx = repo.begin_tenant().await?;
    sqlx::query(
        "INSERT INTO access_audit (id, org_id, clinician_id, route, resource_id)
                 VALUES ($1, current_setting('app.org_id')::uuid, $2, $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(clinician)
    .bind(route)
    .bind(resource)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// Called only after JWT validation and current database membership resolution.
/// Audit the attempt before releasing data; a failed audit insert fails closed.
pub async fn guard(
    repo: &PostgresRepository,
    authed: &AuthedClinician,
    method: Method,
    route: String,
    resource: Option<Uuid>,
) -> Result<(), Response> {
    let check = async {
        if !admit(repo, authed.clinician_id, "general", 60).await? {
            return Ok(false);
        }
        let expensive = if method == Method::POST && route == "/v1/patients/{id}/mdm" {
            Some(("mdm", 5))
        } else if method == Method::POST && route == "/v1/patients/{id}/encounters/{eid}/audio" {
            Some(("audio", 30))
        } else {
            None
        };
        if let Some((bucket, limit)) = expensive
            && !admit(repo, authed.clinician_id, bucket, limit).await?
        {
            return Ok(false);
        }
        if method == Method::GET && route != "/v1/me" {
            record_read(repo, authed.clinician_id, &route, resource).await?;
        }
        Ok::<_, sqlx::Error>(true)
    }
    .await;
    match check {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "60")],
            axum::Json(serde_json::json!({"error": "rate_limited"})),
        )
            .into_response()),
        Err(_) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({"error": "unavailable"})),
        )
            .into_response()),
    }
}
