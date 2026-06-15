use crate::config::Config;


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
#[derive(Clone)]
pub struct AppState{
    config: Config,
}

impl AppState {
    pub async fn build(config: &Config) -> anyhow::Result<AppState> {
        Ok(AppState { config: config.clone()  })
    }
}