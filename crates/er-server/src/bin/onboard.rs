//! One-shot privileged membership change inside the VPC, run as the onboarding
//! login. It prints one fixed outcome per entry, in request order: never a
//! subject, a name or a credential.
use std::time::Duration;

use er_server::{
    config::{OnboardingConfig, OnboardingRequest},
    onboarding,
};

const RUN_TIMEOUT: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = OnboardingConfig::from_env()
        .map_err(|_| anyhow::anyhow!("onboarding_configuration_invalid"))?;
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
                eprintln!("onboarding entry={index} outcome={}", outcome.as_str());
            }
            eprintln!("onboarding status=complete");
            Ok(())
        }
        Ok(Err(error)) => {
            // apply() and revoke() report a fixed step or check name, never a value.
            eprintln!("onboarding status=failed reason={error}");
            Err(anyhow::anyhow!("onboarding_failed"))
        }
        Err(_) => {
            eprintln!("onboarding status=timeout");
            Err(anyhow::anyhow!("onboarding_failed"))
        }
    }
}
