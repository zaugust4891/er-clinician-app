use std::env::var;

#[derive(Clone)]
pub enum AppEnv {
    Dev,
    Staging,
    Prod,
}

#[derive(Clone)]
pub struct Config {
    pub bind: String,
    pub database_url: String,
    pub s3_endpoint_url: String,
    pub sqs_endpoint_url: String,
    pub auth_mode: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Config> {
        // Load .env if present; missing file is not an error (e.g. prod/ECS).
        let _ = dotenvy::dotenv();

        let bind = var("BIND_ADDR")?;
        let database_url = var("DATABASE_URL")?;
        let s3_endpoint_url = var("S3_ENDPOINT_URL")?;
        let sqs_endpoint_url = var("SQS_ENDPOINT_URL")?;
        let auth_mode = var("AUTH_MODE")?;

        Ok(Config {
            bind,
            database_url,
            s3_endpoint_url,
            sqs_endpoint_url,
            auth_mode,
        })
    }
}
