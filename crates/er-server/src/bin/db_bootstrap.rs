//! One-shot privileged database setup inside the VPC, run with the RDS master
//! credential before the first migration and again to apply staged logins.
//! It prints a fixed status only: never SQL, a login name or a password.
use er_server::telemetry::{self, StartupFailure};
use std::{process::ExitCode, time::Duration};

use er_server::{bootstrap, config::BootstrapConfig};

const RUN_TIMEOUT: Duration = Duration::from_secs(120);

#[tokio::main]
async fn main() -> ExitCode {
    telemetry::init("db-bootstrap");
    match execute().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            StartupFailure::at("db-bootstrap")(error).report();
            ExitCode::FAILURE
        }
    }
}

async fn execute() -> anyhow::Result<()> {
    let config = BootstrapConfig::from_env()
        .map_err(|_| telemetry::Classified("bootstrap_configuration_invalid"))?;
    let pool = config.database.connect().await?;
    let apply = bootstrap::apply(&pool, &config.logins);
    let result = tokio::time::timeout(RUN_TIMEOUT, apply).await;
    // Before commit, dropping a timed-out transaction rolls it back. A timeout
    // during authentication checks can leave staged credentials applied.
    let _ = tokio::time::timeout(Duration::from_secs(5), pool.close()).await;
    match result {
        Ok(Ok(())) => {
            tracing::info!(status = "complete", "bootstrap");
            Ok(())
        }
        Ok(Err(error)) => {
            // apply() reports a fixed step or check name, never a value.
            tracing::error!(
                status = "failed",
                error_kind = telemetry::error_kind(&error),
                "bootstrap"
            );
            Err(anyhow::anyhow!("bootstrap_failed"))
        }
        Err(_) => {
            tracing::error!(status = "timeout", "bootstrap");
            Err(anyhow::anyhow!("bootstrap_failed"))
        }
    }
}
