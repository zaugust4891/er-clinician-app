//! One-shot privileged membership change inside the VPC, run as the onboarding
//! login. It prints one fixed outcome per entry, in request order: never a
//! subject, a name or a credential.
use er_server::telemetry::{self, StartupFailure};
use std::{process::ExitCode, time::Duration};

use er_server::{
    config::{OnboardingConfig, OnboardingRequest},
    onboarding,
};

const RUN_TIMEOUT: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> ExitCode {
    telemetry::init("onboarding");
    match execute().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            StartupFailure::at("onboarding")(error).report();
            ExitCode::FAILURE
        }
    }
}

async fn execute() -> anyhow::Result<()> {
    let config = OnboardingConfig::from_env()
        .map_err(|_| telemetry::Classified("onboarding_configuration_invalid"))?;
    let pool = config.database.connect().await?;
    let run = async {
        match &config.request {
            OnboardingRequest::Apply(entries) => {
                onboarding::apply(&pool, entries, config.synthetic_only()).await
            }
            OnboardingRequest::Revoke(entries) => onboarding::revoke(&pool, entries).await,
        }
    };
    // A timeout before commit drops the transaction, which rolls it back.
    let result = tokio::time::timeout(RUN_TIMEOUT, run).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), pool.close()).await;
    match result {
        Ok(Ok(outcomes)) => {
            for (index, outcome) in outcomes.iter().enumerate() {
                tracing::info!(entry = index, outcome = outcome.as_str(), "onboarding");
            }
            tracing::info!(status = "complete", "onboarding");
            Ok(())
        }
        Ok(Err(error)) => {
            // apply() and revoke() report a fixed step or check name, never a value.
            tracing::error!(
                status = "failed",
                error_kind = telemetry::error_kind(&error),
                "onboarding"
            );
            Err(anyhow::anyhow!("onboarding_failed"))
        }
        Err(_) => {
            tracing::error!(status = "timeout", "onboarding");
            Err(anyhow::anyhow!("onboarding_failed"))
        }
    }
}
