use crate::auth::{AuthError, AuthedClinician};
use sqlx::PgPool;

#[derive(Clone)]
pub struct ClinicianResolver {
    pool: PgPool,
}

impl ClinicianResolver {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Read membership on every request: disabling a clinician or changing their
    /// role takes effect on the next authentication check on every API replica.
    pub async fn resolve(&self, sub: &str) -> Result<AuthedClinician, AuthError> {
        let (clinician_id, org_id) = sqlx::query_as::<_, (uuid::Uuid, uuid::Uuid)>(
            "SELECT id, org_id FROM clinicians WHERE cognito_sub = $1
             AND disabled_at IS NULL AND role = 'physician'",
        )
        .bind(sub)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AuthError::UnknownClinician(sub.to_string()))?;
        Ok(AuthedClinician {
            clinician_id,
            org_id,
        })
    }
}
