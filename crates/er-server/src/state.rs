use crate::config::{AuthConfig, Config};
use crate::auth::{self, AuthedClinician};
use crate::repository::PostgresRepository;

use std::sync::Arc;

/* Later to add to AppState:
- Postgres pool — let db = PgPool::connect(&config.database_url).await?; This is the canonical reason build is
  async+fallible: connect at boot, blow up immediately if the DB is unreachable.
  - AWS clients — S3 and SQS clients pointed at config.s3_endpoint_url / config.sqs_endpoint_url (so MinIO/ElasticMQ locally,
  real AWS in prod).
  - The AI layer — your AppSummarizer / Claude client.
  - Anything from er-app the handlers call — repository, ShiftManager, EventBus.

  Each becomes a field on AppState. Critical constraint: AppState must stay cheaply Clone (axum clones it per request via
  #[derive(Clone)], which you already have — good). Pools like PgPool are internally Arc'd so they clone cheaply; anything
  that isn't (a bare repository, a client you built) should be wrapped in Arc<...> so cloning AppState is just a refcount
  bump, not a deep copy.



*/

/// TENANCY INVARIANT (Step 4, deep dive §8): handlers NEVER receive a `PgPool`.
/// They receive a per-request `PostgresRepository::for_org(pool, authed.org_id)`
/// built via `AppState::repo_for`. There is no code path that queries without an org.
#[derive(Clone)]
pub struct AppState{
    pub config: Config,
    pool: sqlx::PgPool,
    pub keys: Arc<auth::keys::KeySet>,
    pub resolver: Arc<auth::resolver::ClinicianResolver>
}

impl AppState {
    pub async fn build(config: &Config) -> anyhow::Result<AppState> {
        let pool = sqlx::PgPool::connect(&config.database_url).await?; // fail at boot, not first request.
        let keys = Arc::new(match &config.auth_mode {
            AuthConfig::Dev { jwks_path } => auth::keys::KeySet::from_file(jwks_path)?,
            AuthConfig::Cognito { .. } => {
                let url = config
                    .auth_mode
                    .jwks_url()
                    .expect("cognito auth mode always yields jwks url");
                auth::keys::KeySet::from_url(url).await?
            }
        });
        
        let resolver = Arc::new(auth::resolver::ClinicianResolver::new(pool.clone()));




        Ok(AppState { config: config.clone(), pool, keys, resolver  })
    }

    pub fn repo_for(&self, authed: &AuthedClinician) -> PostgresRepository {
        PostgresRepository::for_org(self.pool.clone(), authed.org_id)
    }
}