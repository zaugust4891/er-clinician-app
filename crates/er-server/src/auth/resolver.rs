use crate::auth::{AuthError, AuthedClinician};
use sqlx::PgPool;
use std::time::Duration;

#[derive(Clone)]
pub struct ClinicianResolver {
    pool: PgPool,
    cache: moka::future::Cache<String, AuthedClinician>,
}

impl ClinicianResolver {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            cache: moka::future::Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(300))
                .build(),
        }
    }

    pub async fn resolve(&self, sub: &str) -> Result<AuthedClinician, AuthError> {
        if let Some(hit) = self.cache.get(sub).await {
            return Ok(hit);
        }
        let row = sqlx::query!(
            "SELECT id, org_id FROM clinicians WHERE cognito_sub = $1",
            sub
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AuthError::UnknownClinician(sub.to_string()))?; // valid token
        // no row => 401 ("Classic my token works but I get 401")
        let authed = AuthedClinician {
            clinician_id: row.id,
            org_id: row.org_id,
        };
        self.cache.insert(sub.to_string(), authed.clone()).await;

        Ok(authed)
    }
}
