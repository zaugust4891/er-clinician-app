use crate::auth::{self, AuthedClinician};
use crate::aws::{self, QueueUrls};
use crate::config::{AuthConfig, Config};
use crate::repository::PostgresRepository;

use std::sync::Arc;

/// Resolved audio resource locations. `Config` contains deploy-time names;
/// this contains the bucket and queue URLs that handlers and workers can use
/// directly without another discovery call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioConfig {
    pub bucket: String,
    pub stt_queue_url: String,
    pub stt_dlq_url: String,
}

impl AudioConfig {
    pub fn new(bucket: impl Into<String>, queues: QueueUrls) -> Self {
        Self {
            bucket: bucket.into(),
            stt_queue_url: queues.stt,
            stt_dlq_url: queues.stt_dlq,
        }
    }
}

/// TENANCY INVARIANT (Step 4, deep dive §8): handlers NEVER receive a `PgPool`.
/// They receive a per-request `PostgresRepository::for_org(pool, authed.org_id)`
/// built via `AppState::repo_for`. There is no code path that queries without an org.
#[derive(Clone)]
pub struct AppState {
    pub config: Config,
    pool: sqlx::PgPool,
    pub keys: Arc<auth::keys::KeySet>,
    pub resolver: Arc<auth::resolver::ClinicianResolver>,
    pub s3: aws_sdk_s3::Client,
    pub sqs: aws_sdk_sqs::Client,
    pub audio: Arc<AudioConfig>,
}

impl AppState {
    pub async fn build(config: &Config) -> anyhow::Result<AppState> {
        let pool = config.database.connect().await?;
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
        let clients = aws::configured_clients(&config.runtime).await;
        let dlq = config.stt_dlq();
        let queues = if config.uses_aws_endpoint_overrides() {
            aws::ensure_audio_infra(
                &clients.s3,
                &clients.sqs,
                &config.audio_bucket,
                &config.stt_queue,
                &dlq,
            )
            .await?
        } else {
            aws::resolve_queue_urls(&clients.sqs, &config.stt_queue, &dlq).await?
        };
        let audio = Arc::new(AudioConfig::new(&config.audio_bucket, queues));

        tracing::info!(
            bucket = %audio.bucket,
            stt_queue_url = %audio.stt_queue_url,
            stt_dlq_url = %audio.stt_dlq_url,
            bootstrapped = config.uses_aws_endpoint_overrides(),
            "audio infrastructure ready"
        );

        Ok(AppState {
            config: config.clone(),
            pool,
            keys,
            resolver,
            s3: clients.s3,
            sqs: clients.sqs,
            audio,
        })
    }

    pub fn repo_for(&self, authed: &AuthedClinician) -> PostgresRepository {
        PostgresRepository::for_org(self.pool.clone(), authed.org_id)
    }

    /// Test-only: build a dev-mode `AppState` around an already-connected pool
    /// (e.g. the one `#[sqlx::test]` provides), instead of connecting via env config.
    /// Kept `pub` (not `#[cfg(test)]`) so integration tests in `tests/` — a separate
    /// crate — can see it.
    pub fn for_tests(pool: sqlx::PgPool, keys: Arc<auth::keys::KeySet>) -> AppState {
        let s3_endpoint = std::env::var("S3_ENDPOINT_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "http://localhost:9000".to_string());
        let sqs_endpoint = std::env::var("SQS_ENDPOINT_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "http://localhost:9324".to_string());
        let clients = aws::lazy_test_clients(&s3_endpoint, &sqs_endpoint);
        let audio = AudioConfig::new(
            "er-audio-test",
            QueueUrls {
                stt: format!("{sqs_endpoint}/000000000000/stt-jobs-test"),
                stt_dlq: format!("{sqs_endpoint}/000000000000/stt-jobs-test-dlq"),
            },
        );

        Self::for_tests_with_audio(pool, keys, clients.s3, clients.sqs, audio)
    }

    /// Audio tests inject uniquely named, already-bootstrapped resources while
    /// all older tests retain the simpler `for_tests(pool, keys)` constructor.
    pub fn for_tests_with_audio(
        pool: sqlx::PgPool,
        keys: Arc<auth::keys::KeySet>,
        s3: aws_sdk_s3::Client,
        sqs: aws_sdk_sqs::Client,
        audio: AudioConfig,
    ) -> AppState {
        let resolver = Arc::new(auth::resolver::ClinicianResolver::new(pool.clone()));
        let config = Config {
            bind: "127.0.0.1:0".to_string(),
            runtime: crate::config::MigrationConfig::from_values(
                &[
                    ("APP_ENV".into(), "dev".into()),
                    ("AWS_REGION".into(), "us-east-1".into()),
                    (
                        "DATABASE_URL".into(),
                        "postgres://test:test@localhost/test".into(),
                    ),
                ]
                .into(),
            )
            .expect("synthetic runtime config")
            .runtime,
            auth_mode: AuthConfig::Dev {
                jwks_path: "dev/keys/jwks.json".into(),
            },
            audio_bucket: audio.bucket.clone(),
            stt_queue: "stt-jobs-test".to_string(),
        };
        AppState {
            config,
            pool,
            keys,
            resolver,
            s3,
            sqs,
            audio: Arc::new(audio),
        }
    }
}
