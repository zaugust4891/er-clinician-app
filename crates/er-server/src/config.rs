use std::env::var;
use std::path::PathBuf;

use er_ai::model::{SummarizerConfig, configured_model};

#[derive(Clone)]
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
pub struct Config {
    pub bind: String,
    pub database_url: String,
    /// Custom service endpoints are present in local development (MinIO and
    /// ElasticMQ) and absent in AWS, where the SDK resolves the real endpoints.
    pub s3_endpoint_url: Option<String>,
    pub sqs_endpoint_url: Option<String>,
    pub auth_mode: AuthConfig,
    pub audio_bucket: String,
    pub stt_queue: String,
    pub deepgram_api_key: Option<String>,
    pub ai_transport: AiTransportMode,
    pub anthropic_api_key: Option<String>,
    pub summarizer: SummarizerConfig,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Config> {
        // Load .env if present; missing file is not an error (e.g. prod/ECS).
        let _ = dotenvy::dotenv();

        let bind = required_var("BIND_ADDR")?;
        let database_url = required_var("DATABASE_URL")?;
        let s3_endpoint_url = optional_var("S3_ENDPOINT_URL");
        let sqs_endpoint_url = optional_var("SQS_ENDPOINT_URL");
        if s3_endpoint_url.is_some() != sqs_endpoint_url.is_some() {
            anyhow::bail!(
                "S3_ENDPOINT_URL and SQS_ENDPOINT_URL must either both be set (local development) or both be absent (AWS)"
            );
        }
        let auth_mode = match required_var("AUTH_MODE")?.as_str() {
            "dev" => AuthConfig::Dev {
                jwks_path: required_var("DEV_JWKS_PATH")?.into(),
            },
            "cognito" => AuthConfig::Cognito {
                region: required_var("COGNITO_REGION")?,
                pool_id: required_var("COGNITO_POOL_ID")?,
                client_id: required_var("COGNITO_CLIENT_ID")?,
            },
            other => anyhow::bail!("AUTH_MODE must be dev|cognito, got {other}"),
        };
        let audio_bucket = required_var("AUDIO_BUCKET")?;
        let stt_queue = required_var("STT_QUEUE")?;
        let deepgram_api_key = optional_var("DEEPGRAM_API_KEY");
        let ai_transport = match optional_var("TRANSPORT")
            .unwrap_or_else(|| "direct".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "direct" => AiTransportMode::Direct,
            "bedrock" => AiTransportMode::Bedrock,
            other => anyhow::bail!("TRANSPORT must be direct|bedrock, got {other}"),
        };
        let anthropic_api_key = optional_var("ANTHROPIC_KEY");
        let summary_model_name =
            optional_var("SUMMARY_MODEL").unwrap_or_else(|| "sonnet".to_string());
        let mdm_model_name =
            optional_var("MDM_MODEL").unwrap_or_else(|| "sonnet".to_string());
        let summary_model = configured_model(&summary_model_name)
            .ok_or_else(|| anyhow::anyhow!("SUMMARY_MODEL is not recognized"))?;
        let mdm_model = configured_model(&mdm_model_name)
            .ok_or_else(|| anyhow::anyhow!("MDM_MODEL is not recognized"))?;

        Ok(Config {
            bind,
            database_url,
            s3_endpoint_url,
            sqs_endpoint_url,
            auth_mode,
            audio_bucket,
            stt_queue,
            deepgram_api_key,
            ai_transport,
            anthropic_api_key,
            summarizer: SummarizerConfig {
                summary_model,
                mdm_model,
            },
        })
    }
}

impl Config {
    pub fn stt_dlq(&self) -> String {
        format!("{}-dlq", self.stt_queue)
    }

    /// Endpoint overrides are the environment switch between local protocol
    /// clones and real AWS. Production does not set either override.
    pub fn uses_aws_endpoint_overrides(&self) -> bool {
        self.s3_endpoint_url.is_some() && self.sqs_endpoint_url.is_some()
    }
}

fn optional_var(name: &str) -> Option<String> {
    var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn required_var(name: &str) -> anyhow::Result<String> {
    let value = var(name)?.trim().to_owned();
    if value.is_empty() {
        anyhow::bail!("{name} must not be empty");
    }
    Ok(value)
}
