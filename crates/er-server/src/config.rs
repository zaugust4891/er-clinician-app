use std::env::var;
use std::path::PathBuf;

#[derive(Clone)]
pub enum AppEnv {
    Dev,
    Staging,
    Prod,
}

#[derive(Clone)]
pub enum AuthConfig {
    Dev { jwks_path: PathBuf },
    Cognito { region: String, pool_id: String, client_id: String },
}

pub const DEV_ISSUER: &str = "https//dev.local";


impl AuthConfig {
    pub fn issuer(&self) -> String {
        match self {
            Self::Dev{ .. } => DEV_ISSUER.to_string(),
            Self::Cognito { region, pool_id, .. } => 
                format!("https//cognito-idp.{region}.amazonaws.com/{pool_id}"),
        }
    }

    pub fn jwks_url(&self) -> Option<String> {
        match self {
            Self::Dev{ .. } => None,
            Self::Cognito { .. } => Some(format!("{}/.well-known/jwks.json", self.issuer())),
        }
    }
}

#[derive(Clone)]
pub struct Config {
    pub bind: String,
    pub database_url: String,
    pub s3_endpoint_url: String,
    pub sqs_endpoint_url: String,
    pub auth_mode: AuthConfig,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Config> {
        // Load .env if present; missing file is not an error (e.g. prod/ECS).
        let _ = dotenvy::dotenv();

        let bind = var("BIND_ADDR")?;
        let database_url = var("DATABASE_URL")?;
        let s3_endpoint_url = var("S3_ENDPOINT_URL")?;
        let sqs_endpoint_url = var("SQS_ENDPOINT_URL")?;
        let auth_mode = match var("AUTH_MODE")?.as_str() {
            "dev" => AuthConfig::Dev { jwks_path: var("DEV_JWKS_PATH")?.into() },
            "cognito" => AuthConfig::Cognito {
            region: var("COGNITO_REGION")?,
            pool_id: var("COGNITO_POOL_ID")?,
            client_id: var("COGNITO_CLIENT_ID")?,
        },
        other => anyhow::bail!("AUTH_MODE must be dev|cognito, got {other}"),
    };

        Ok(Config {
            bind,
            database_url,
            s3_endpoint_url,
            sqs_endpoint_url,
            auth_mode,
        })
    }
}
