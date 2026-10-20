//! One-shot schema migration, then the runtime grants that follow the schema.
//! Database roles are created beforehand by the one-shot `er-db-bootstrap`.
use er_server::telemetry::{self, StartupFailure};
use std::{process::ExitCode, time::Duration};

use er_server::config::{AppEnv, MigrationConfig};
use er_server::privileges;
use sqlx::{
    PgPool,
    migrate::{MigrateError, Migrator},
};

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");
const RUN_TIMEOUT: Duration = Duration::from_secs(600);
const GRANT_TIMEOUT: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> ExitCode {
    telemetry::init("migration");
    match execute().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            StartupFailure::at("migration")(error).report();
            ExitCode::FAILURE
        }
    }
}

async fn execute() -> anyhow::Result<()> {
    let config = MigrationConfig::from_env()
        .map_err(|_| telemetry::Classified("migration_configuration_invalid"))?;
    // `main` prints its error's Debug chain; a driver error can name hosts or SQL.
    let pool = config
        .database
        .connect()
        .await
        .map_err(|_| anyhow::anyhow!("database_connect_failed"))?;
    let result = run(&pool, config.app_env != AppEnv::Dev).await;
    // On a deadline, exiting the process closes the connection and aborts the
    // outstanding database transaction. Do not wait without a bound here.
    let _ = tokio::time::timeout(Duration::from_secs(5), pool.close()).await;
    result
}

/// Grants converge in the same release step as the migration, so a new table
/// never ships without its privilege decision. Cloud requires the bootstrap's
/// runtime roles; a development database without them skips the grants.
async fn run(pool: &PgPool, cloud: bool) -> anyhow::Result<()> {
    migrate(pool).await?;
    let converge = privileges::converge(pool, cloud);
    match tokio::time::timeout(GRANT_TIMEOUT, converge).await {
        Ok(Ok(true)) => tracing::info!(status = "complete", "grants"),
        Ok(Ok(false)) => tracing::info!(status = "skipped_without_runtime_roles", "grants"),
        Ok(Err(_)) => {
            tracing::error!(status = "failed", "grants");
            anyhow::bail!("grants_failed");
        }
        Err(_) => {
            tracing::error!(status = "timeout", "grants");
            anyhow::bail!("grants_failed");
        }
    }
    Ok(())
}

async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    // SQLx's advisory migration lock is enabled by default. It validates
    // previously applied checksums on every run, including a no-op rerun.
    let result = tokio::time::timeout(RUN_TIMEOUT, MIGRATOR.run(pool)).await;
    match result {
        Ok(Ok(())) => {
            let version = MIGRATOR
                .iter()
                .map(|migration| migration.version)
                .max()
                .unwrap_or(0);
            tracing::info!(version, status = "complete", "migration");
            Ok(())
        }
        Ok(Err(error)) => {
            report_failure(&error);
            Err(anyhow::anyhow!("migration_failed"))
        }
        Err(_) => {
            tracing::error!(status = "run_timeout", "migration");
            Err(anyhow::anyhow!("migration_failed"))
        }
    }
}

fn report_failure(error: &MigrateError) {
    // Never print SQLx/driver error chains: they can include SQL and connection
    // details. Migration version and a fixed status are sufficient for CI.
    let (version, status) = match error {
        MigrateError::VersionMismatch(version) => (*version, "checksum_mismatch"),
        MigrateError::VersionMissing(version) => (*version, "version_missing"),
        MigrateError::Dirty(version) => (*version, "partially_applied"),
        MigrateError::ExecuteMigration(_, version) => (*version, "execution_failed"),
        _ => (0, "database_failed"),
    };
    tracing::error!(version, status, "migration");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = false)]
    async fn empty_database_then_noop_then_checksum_mismatch(pool: PgPool) {
        run(&pool, false).await.unwrap();
        let first_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE success")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(first_count as usize, MIGRATOR.iter().count());

        run(&pool, false).await.unwrap();
        let second_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE success")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(second_count, first_count);

        let version = MIGRATOR.iter().next().unwrap().version;
        sqlx::query(
            "UPDATE _sqlx_migrations SET checksum = decode('00', 'hex') WHERE version = $1",
        )
        .bind(version)
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            run(&pool, false).await.is_err(),
            "a changed applied migration must block release"
        );
    }

    #[test]
    fn migration_lock_remains_enabled() {
        assert!(MIGRATOR.locking);
        assert!(!MIGRATOR.ignore_missing);
    }
}
