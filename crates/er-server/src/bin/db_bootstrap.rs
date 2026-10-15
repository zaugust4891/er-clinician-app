//! One-shot privileged database setup inside the VPC, run with the RDS master
//! credential before the first migration and again to apply staged logins.
//! It prints a fixed status only: never SQL, a login name or a password.
use std::time::Duration;

use er_server::{bootstrap, config::BootstrapConfig};

const RUN_TIMEOUT: Duration = Duration::from_secs(120);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = BootstrapConfig::from_env()
        .map_err(|_| anyhow::anyhow!("bootstrap_configuration_invalid"))?;
    let pool = config.database.connect().await?;
    let apply = bootstrap::apply(&pool, &config.logins);
    let result = tokio::time::timeout(RUN_TIMEOUT, apply).await;
    // Before commit, dropping a timed-out transaction rolls it back. A timeout
    // during authentication checks can leave staged credentials applied.
    let _ = tokio::time::timeout(Duration::from_secs(5), pool.close()).await;
    match result {
        Ok(Ok(())) => {
            eprintln!("bootstrap status=complete");
            Ok(())
        }
        Ok(Err(error)) => {
            // apply() reports a fixed step or check name, never a value.
            eprintln!("bootstrap status=failed reason={error}");
            Err(anyhow::anyhow!("bootstrap_failed"))
        }
        Err(_) => {
            eprintln!("bootstrap status=timeout");
            Err(anyhow::anyhow!("bootstrap_failed"))
        }
    }
}
