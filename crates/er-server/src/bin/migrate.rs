//! One-shot schema migration. Database roles are bootstrapped separately.
use std::time::Duration;

use er_server::config::MigrationConfig;
use sqlx::{
    PgPool,
    migrate::{MigrateError, Migrator},
};

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");
const RUN_TIMEOUT: Duration = Duration::from_secs(600);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = MigrationConfig::from_env()
        .map_err(|_| anyhow::anyhow!("migration_configuration_invalid"))?;
    let pool = config.database.connect().await?;
    let result = run(&pool).await;
    // On a deadline, exiting the process closes the connection and aborts the
    // outstanding database transaction. Do not wait without a bound here.
    let _ = tokio::time::timeout(Duration::from_secs(5), pool.close()).await;
    result
}

async fn run(pool: &PgPool) -> anyhow::Result<()> {
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
            eprintln!("migration version={version} status=complete");
            Ok(())
        }
        Ok(Err(error)) => {
            report_failure(&error);
            Err(anyhow::anyhow!("migration_failed"))
        }
        Err(_) => {
            eprintln!("migration status=run_timeout");
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
    eprintln!("migration version={version} status={status}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = false)]
    async fn empty_database_then_noop_then_checksum_mismatch(pool: PgPool) {
        run(&pool).await.unwrap();
        let first_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE success")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(first_count as usize, MIGRATOR.iter().count());

        run(&pool).await.unwrap();
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
            run(&pool).await.is_err(),
            "a changed applied migration must block release"
        );
    }

    #[test]
    fn migration_lock_remains_enabled() {
        assert!(MIGRATOR.locking);
        assert!(!MIGRATOR.ignore_missing);
    }
}
