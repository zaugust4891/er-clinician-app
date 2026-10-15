//! Process-specific, fail-closed startup configuration. Never derive Debug on secrets.
use std::{collections::BTreeMap, ops::Deref, path::PathBuf, time::Duration};

use er_ai::model::{SummarizerConfig, configured_model};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppEnv {
    Dev,
    Staging,
    Prod,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AiTransportMode {
    Direct,
    Bedrock,
}

#[derive(Clone)]
pub enum AuthConfig {
    Dev {
        jwks_path: PathBuf,
    },
    Cognito {
        region: String,
        pool_id: String,
        client_id: String,
    },
}

pub const DEV_ISSUER: &str = "https://dev.local";
impl AuthConfig {
    pub fn issuer(&self) -> String {
        match self {
            Self::Dev { .. } => DEV_ISSUER.to_string(),
            Self::Cognito {
                region, pool_id, ..
            } => format!("https://cognito-idp.{region}.amazonaws.com/{pool_id}"),
        }
    }
    pub fn jwks_url(&self) -> Option<String> {
        match self {
            Self::Dev { .. } => None,
            Self::Cognito { .. } => Some(format!("{}/.well-known/jwks.json", self.issuer())),
        }
    }
}

#[derive(Clone)]
pub struct RuntimeConfig {
    pub app_env: AppEnv,
    pub aws_region: String,
    pub database: crate::db::DatabaseConfig,
    pub s3_endpoint_url: Option<String>,
    pub sqs_endpoint_url: Option<String>,
    pub max_audio_bytes: usize,
}
impl RuntimeConfig {
    pub fn uses_aws_endpoint_overrides(&self) -> bool {
        self.app_env == AppEnv::Dev
            && self.s3_endpoint_url.is_some()
            && self.sqs_endpoint_url.is_some()
    }
}

/// HTTP process: no STT/AI credentials or provider initialization.
#[derive(Clone)]
pub struct Config {
    pub runtime: RuntimeConfig,
    pub bind: String,
    pub auth_mode: AuthConfig,
    pub audio_bucket: String,
    pub stt_queue: String,
}

#[derive(Clone)]
pub struct WorkerConfig {
    pub runtime: RuntimeConfig,
    pub audio_bucket: String,
    pub stt_queue: String,
    pub deepgram_api_key: Option<String>,
    pub ai_transport: AiTransportMode,
    pub anthropic_api_key: Option<String>,
    pub summarizer: SummarizerConfig,
    pub limits: WorkerLimits,
}

/// One-shot schema process; parsing performs no network I/O.
#[derive(Clone)]
pub struct MigrationConfig {
    pub runtime: RuntimeConfig,
}

/// Privileged one-shot setup. Each managed login comes from its staged secret.
pub struct BootstrapConfig {
    pub runtime: RuntimeConfig,
    pub logins: crate::bootstrap::Logins,
}

macro_rules! runtime_access {
    ($($ty:ty),+) => { $(impl Deref for $ty {
        type Target = RuntimeConfig;
        fn deref(&self) -> &Self::Target { &self.runtime }
    })+ };
}
runtime_access!(Config, WorkerConfig, MigrationConfig, BootstrapConfig);

/// One sequential job per worker. The final 10 seconds of the job budget are
/// reserved for terminal bookkeeping; shutdown never extends the job deadline.
#[derive(Clone, Copy, Debug)]
pub struct WorkerLimits {
    pub max_audio_bytes: usize,
    pub download_timeout: Duration,
    pub provider_timeout: Duration,
    pub job_timeout: Duration,
    pub terminal_timeout: Duration,
    pub lease_interval: Duration,
    pub lease_timeout: Duration,
    pub visibility_seconds: i32,
    pub shutdown_drain: Duration,
}
impl Default for WorkerLimits {
    fn default() -> Self {
        Self {
            max_audio_bytes: 16 * 1024 * 1024,
            download_timeout: Duration::from_secs(20),
            provider_timeout: Duration::from_secs(45),
            job_timeout: Duration::from_secs(270),
            terminal_timeout: Duration::from_secs(10),
            lease_interval: Duration::from_secs(60),
            lease_timeout: Duration::from_secs(5),
            visibility_seconds: 300,
            shutdown_drain: Duration::from_secs(100),
        }
    }
}

/// Cloud database settings: manifest placement plus the `username` and `password`
/// fields ECS injects from the process's own login secret.
const DB_PARTS: [&str; 5] = [
    "DB_HOST",
    "DB_PORT",
    "DB_NAME",
    "DB_USERNAME",
    "DB_PASSWORD",
];

type Values = BTreeMap<String, String>;
struct Env<'a>(&'a Values);
impl Env<'_> {
    fn optional(&self, key: &str) -> Option<String> {
        self.0
            .get(key)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    }
    fn required(&self, key: &str) -> anyhow::Result<String> {
        self.optional(key)
            .ok_or_else(|| anyhow::anyhow!("{key} is required"))
    }
    /// A credential's exact value: never trimmed and never echoed in an error.
    fn secret(&self, key: &str) -> anyhow::Result<String> {
        self.0
            .get(key)
            .filter(|s| !s.is_empty())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{key} is required"))
    }
    /// `DATABASE_URL` is the development convenience. Cloud composes non-secret
    /// placement with the two injected secret fields, so no URL-shaped copy of a
    /// password exists to escape incorrectly or go stale after a rotation.
    fn database_source(&self, app_env: AppEnv) -> anyhow::Result<crate::db::DatabaseSource> {
        let url = self.optional("DATABASE_URL");
        // Credential whitespace is significant; any supplied part selects this source.
        let parts = DB_PARTS.iter().any(|key| self.0.contains_key(*key));
        anyhow::ensure!(
            url.is_none() || !parts,
            "DATABASE_URL and DB_* connection settings are mutually exclusive"
        );
        if let Some(url) = url {
            anyhow::ensure!(
                app_env == AppEnv::Dev,
                "DATABASE_URL is development-only; cloud uses the DB_* settings"
            );
            return Ok(crate::db::DatabaseSource::Url(url));
        }
        anyhow::ensure!(
            parts || app_env != AppEnv::Dev,
            "DATABASE_URL or the DB_* settings are required"
        );
        let port = self
            .required("DB_PORT")?
            .parse::<u16>()
            .map_err(|_| anyhow::anyhow!("DB_PORT must be a TCP port"))?;
        Ok(crate::db::DatabaseSource::Parts {
            host: self.required("DB_HOST")?,
            port,
            database: self.required("DB_NAME")?,
            username: self.required("DB_USERNAME")?,
            password: self.secret("DB_PASSWORD")?,
        })
    }
    fn app_env(&self) -> anyhow::Result<AppEnv> {
        match self.required("APP_ENV")?.as_str() {
            "dev" => Ok(AppEnv::Dev),
            "staging" => Ok(AppEnv::Staging),
            "prod" => Ok(AppEnv::Prod),
            _ => anyhow::bail!("APP_ENV must be dev|staging|prod"),
        }
    }
    fn number(&self, key: &str, default: u64, min: u64, max: u64) -> anyhow::Result<u64> {
        let n = match self.optional(key) {
            Some(s) => s
                .parse()
                .map_err(|_| anyhow::anyhow!("{key} must be an integer"))?,
            None => default,
        };
        anyhow::ensure!(
            (min..=max).contains(&n),
            "{key} is outside its supported range"
        );
        Ok(n)
    }
    fn runtime(&self, process: &'static str) -> anyhow::Result<RuntimeConfig> {
        let app_env = self.app_env()?;
        let aws_region = self.required("AWS_REGION")?;
        anyhow::ensure!(
            aws_region.len() <= 32
                && aws_region.contains('-')
                && aws_region
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "AWS_REGION is invalid"
        );
        let s3_endpoint_url = self.optional("S3_ENDPOINT_URL");
        let sqs_endpoint_url = self.optional("SQS_ENDPOINT_URL");
        anyhow::ensure!(
            s3_endpoint_url.is_some() == sqs_endpoint_url.is_some(),
            "S3_ENDPOINT_URL and SQS_ENDPOINT_URL must both be set or both be absent"
        );
        if app_env != AppEnv::Dev {
            for key in self.0.keys() {
                let forbidden = key.starts_with("AWS_ENDPOINT_URL")
                    || key.starts_with("PG")
                    || [
                        "S3_ENDPOINT_URL",
                        "SQS_ENDPOINT_URL",
                        "DEV_JWKS_PATH",
                        "JWKS_URL",
                        "ANTHROPIC_KEY",
                        "ANTHROPIC_API_KEY",
                        "AWS_ACCESS_KEY_ID",
                        "AWS_SECRET_ACCESS_KEY",
                        "AWS_SESSION_TOKEN",
                        "AWS_SECURITY_TOKEN",
                        "AWS_PROFILE",
                        "AWS_DEFAULT_PROFILE",
                        "AWS_CONFIG_FILE",
                        "AWS_SHARED_CREDENTIALS_FILE",
                        "AWS_WEB_IDENTITY_TOKEN_FILE",
                        "AWS_ROLE_ARN",
                        "AWS_CA_BUNDLE",
                        "AWS_BEARER_TOKEN_BEDROCK",
                    ]
                    .contains(&key.as_str());
                anyhow::ensure!(
                    !forbidden || self.optional(key).is_none(),
                    "{key} is forbidden outside dev"
                );
            }
            anyhow::ensure!(
                self.optional("AUTH_MODE").as_deref() != Some("dev"),
                "dev auth is forbidden outside dev"
            );
            if let Some(transport) = self.optional("TRANSPORT") {
                anyhow::ensure!(transport == "bedrock", "cloud TRANSPORT must be bedrock");
            }
            if process != "worker" {
                anyhow::ensure!(
                    self.optional("DEEPGRAM_API_KEY").is_none(),
                    "DEEPGRAM_API_KEY is worker-only"
                );
            }
        }
        let database = crate::db::DatabaseConfig::new(
            self.database_source(app_env)?,
            app_env,
            process,
            self.optional("DB_SSL_ROOT_CERT").map(PathBuf::from),
            self.number(
                "DB_MAX_CONNECTIONS",
                if matches!(process, "migration" | "db-bootstrap") {
                    1
                } else {
                    5
                },
                if process == "worker" { 2 } else { 1 },
                32,
            )? as u32,
            Duration::from_secs(self.number("DB_ACQUIRE_TIMEOUT_SECS", 5, 1, 30)?),
            Duration::from_secs(self.number("DB_CONNECT_TIMEOUT_SECS", 10, 1, 30)?),
        )?;
        Ok(RuntimeConfig {
            app_env,
            aws_region,
            database,
            s3_endpoint_url,
            sqs_endpoint_url,
            max_audio_bytes: self.number(
                "MAX_AUDIO_BYTES",
                16 * 1024 * 1024,
                1,
                64 * 1024 * 1024,
            )? as usize,
        })
    }
}

fn load_env() -> anyhow::Result<Values> {
    // APP_ENV must originate outside .env. Cloud never reads developer files.
    let values: Values = std::env::vars().collect();
    if Env(&values).app_env()? == AppEnv::Dev {
        dotenvy::dotenv()
            .or_else(|error| {
                if error.not_found() {
                    Ok(PathBuf::new())
                } else {
                    Err(error)
                }
            })
            .map_err(|_| anyhow::anyhow!("development dotenv could not be loaded"))?;
    }
    Ok(std::env::vars().collect())
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_values(&load_env()?)
    }
    pub fn from_values(values: &Values) -> anyhow::Result<Self> {
        let env = Env(values);
        let runtime = env.runtime("api")?;
        let auth_mode = match env.required("AUTH_MODE")?.as_str() {
            "dev" if runtime.app_env == AppEnv::Dev => AuthConfig::Dev {
                jwks_path: env.required("DEV_JWKS_PATH")?.into(),
            },
            "cognito" => {
                let region = env.required("COGNITO_REGION")?;
                let pool_id = env.required("COGNITO_POOL_ID")?;
                let client_id = env.required("COGNITO_CLIENT_ID")?;
                anyhow::ensure!(
                    region == runtime.aws_region
                        && pool_id.starts_with(&format!("{region}_"))
                        && pool_id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                        && client_id.bytes().all(|b| b.is_ascii_alphanumeric()),
                    "Cognito identifiers or region are invalid"
                );
                AuthConfig::Cognito {
                    region,
                    pool_id,
                    client_id,
                }
            }
            _ => anyhow::bail!("AUTH_MODE must be dev (development only) or cognito"),
        };
        let bind = env.required("BIND_ADDR")?;
        bind.parse::<std::net::SocketAddr>()
            .map_err(|_| anyhow::anyhow!("BIND_ADDR must be a socket address"))?;
        Ok(Self {
            runtime,
            bind,
            auth_mode,
            audio_bucket: env.required("AUDIO_BUCKET")?,
            stt_queue: env.required("STT_QUEUE")?,
        })
    }
    pub fn stt_dlq(&self) -> String {
        format!("{}-dlq", self.stt_queue)
    }
}

impl WorkerConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_values(&load_env()?)
    }
    pub fn from_values(values: &Values) -> anyhow::Result<Self> {
        let env = Env(values);
        let runtime = env.runtime("worker")?;
        let cloud = runtime.app_env != AppEnv::Dev;
        let ai_transport = match env.optional("TRANSPORT").as_deref() {
            Some("bedrock") => AiTransportMode::Bedrock,
            Some("direct") | None if !cloud => AiTransportMode::Direct,
            _ => anyhow::bail!("TRANSPORT must explicitly be bedrock in cloud"),
        };
        let deepgram_api_key = if cloud {
            Some(env.required("DEEPGRAM_API_KEY")?)
        } else {
            env.optional("DEEPGRAM_API_KEY")
        };
        let model = |key: &str, override_key: &str| -> anyhow::Result<er_ai::model::ModelId> {
            let name = if cloud {
                env.required(key)?
            } else {
                env.optional(key).unwrap_or_else(|| "sonnet".into())
            };
            anyhow::ensure!(
                !cloud || name == "sonnet",
                "{key} must explicitly be sonnet for the cloud pilot"
            );
            let mut model = configured_model(&name)
                .ok_or_else(|| anyhow::anyhow!("{key} is not recognized"))?;
            // Cloud names its Bedrock route explicitly instead of inheriting the
            // in-Region default: that default is not offered in every Region, and
            // the choice decides where prompts may be processed and what they cost.
            let bedrock_id = env.optional(override_key);
            anyhow::ensure!(
                !cloud || bedrock_id.is_some(),
                "{override_key} must be set explicitly in cloud"
            );
            if let Some(id) = bedrock_id {
                anyhow::ensure!(
                    ai_transport == AiTransportMode::Bedrock && model.set_bedrock_id(&id),
                    "{override_key} is not an approved, priced identifier for this family"
                );
            }
            Ok(model)
        };
        let summarizer = SummarizerConfig {
            summary_model: model("SUMMARY_MODEL", "BEDROCK_SUMMARY_MODEL_ID")?,
            mdm_model: model("MDM_MODEL", "BEDROCK_MDM_MODEL_ID")?,
        };
        let limits = WorkerLimits {
            max_audio_bytes: runtime.max_audio_bytes,
            ..Default::default()
        };
        Ok(Self {
            runtime,
            audio_bucket: env.required("AUDIO_BUCKET")?,
            stt_queue: env.required("STT_QUEUE")?,
            deepgram_api_key,
            ai_transport,
            anthropic_api_key: env.optional("ANTHROPIC_KEY"),
            summarizer,
            limits,
        })
    }
    pub fn stt_dlq(&self) -> String {
        format!("{}-dlq", self.stt_queue)
    }
}

impl MigrationConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_values(&load_env()?)
    }
    pub fn from_values(values: &Values) -> anyhow::Result<Self> {
        Ok(Self {
            runtime: Env(values).runtime("migration")?,
        })
    }
}

impl BootstrapConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_values(&load_env()?)
    }

    pub fn from_values(values: &Values) -> anyhow::Result<Self> {
        let env = Env(values);
        let login = |prefix: &str| -> anyhow::Result<crate::bootstrap::Login> {
            Ok(crate::bootstrap::Login {
                username: env.required(&format!("{prefix}_DB_USERNAME"))?,
                password: env.secret(&format!("{prefix}_DB_PASSWORD"))?,
            })
        };
        let logins = crate::bootstrap::Logins {
            owner: login("OWNER")?,
            api: login("API")?,
            worker: login("WORKER")?,
            onboarding: login("ONBOARDING")?,
        };
        logins.validate()?;
        Ok(Self {
            runtime: env.runtime("db-bootstrap")?,
            logins,
        })
    }
}
