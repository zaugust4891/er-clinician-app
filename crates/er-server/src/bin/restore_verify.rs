//! Local operator tool: no API listener, queue client, migrations or AI provider.
use er_server::{
    config::AppEnv,
    db::{DatabaseConfig, DatabaseSource},
    recovery,
    repository::PostgresRepository,
};
use std::{path::PathBuf, process::ExitCode, time::Duration};
use uuid::Uuid;

#[tokio::main]
async fn main() -> ExitCode {
    // Never print database driver errors or clinical data.
    match tokio::time::timeout(Duration::from_secs(60), verify()).await {
        Ok(Ok(())) => ExitCode::SUCCESS,
        _ => {
            eprintln!("restore_verification_failed");
            ExitCode::FAILURE
        }
    }
}
fn value(key: &str) -> anyhow::Result<String> {
    Ok(std::env::var(key)?)
}
async fn connect(prefix: &str, env: AppEnv) -> anyhow::Result<sqlx::PgPool> {
    let config = DatabaseConfig::new(
        DatabaseSource::Parts {
            host: value(&format!("{prefix}_DB_HOST"))?,
            port: value(&format!("{prefix}_DB_PORT"))?.parse()?,
            database: value(&format!("{prefix}_DB_NAME"))?,
            username: value(&format!("{prefix}_DB_USERNAME"))?,
            password: value(&format!("{prefix}_DB_PASSWORD"))?,
        },
        env,
        "api",
        std::env::var("DB_SSL_ROOT_CERT").ok().map(PathBuf::from),
        1,
        Duration::from_secs(5),
        Duration::from_secs(10),
    )?;
    let pool = config.connect().await?;
    make_read_only(&pool).await?;
    Ok(pool)
}
/// Set both the existing connection and startup options for replacements. A
/// reconnect must not silently restore PostgreSQL's read/write default.
async fn make_read_only(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    pool.set_connect_options(
        (*pool.connect_options())
            .clone()
            .options([("default_transaction_read_only", "on")]),
    );
    sqlx::query("SET default_transaction_read_only = on")
        .execute(pool)
        .await?;
    Ok(())
}

async fn verify() -> anyhow::Result<()> {
    let env = match value("APP_ENV")?.as_str() {
        "dev" => AppEnv::Dev,
        "staging" => AppEnv::Staging,
        _ => anyhow::bail!("restore_verifier_requires_synthetic_staging"),
    };
    anyhow::ensure!(
        (
            value("SOURCE_DB_HOST")?,
            value("SOURCE_DB_PORT")?,
            value("SOURCE_DB_NAME")?
        ) != (
            value("RESTORE_DB_HOST")?,
            value("RESTORE_DB_PORT")?,
            value("RESTORE_DB_NAME")?
        ),
        "source_and_restore_must_differ"
    );
    let org = Uuid::parse_str(&value("VERIFY_ORG_ID")?)?;
    let patient = Uuid::parse_str(&value("VERIFY_PATIENT_ID")?)?;
    let source = PostgresRepository::for_org(connect("SOURCE", env).await?, org);
    let restored = PostgresRepository::for_org(connect("RESTORE", env).await?, org);
    let expected = recovery::evidence(&source, patient).await?;
    let actual = recovery::evidence(&restored, patient).await?;
    anyhow::ensure!(expected == actual, "restored_replay_differs");
    println!(
        "restore_verified events={} last_sequence={} events_sha256={} note_sha256={}",
        actual.event_count, actual.last_sequence, actual.events_sha256, actual.note_sha256
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test(migrations = "./migrations")]
    async fn verifier_stays_read_only_after_reconnection(pool: sqlx::PgPool) {
        let verifier = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_with((*pool.connect_options()).clone())
            .await
            .unwrap();
        make_read_only(&verifier).await.unwrap();
        for _ in 0..2 {
            let state: String = sqlx::query_scalar("SHOW default_transaction_read_only")
                .fetch_one(&verifier)
                .await
                .unwrap();
            assert_eq!(state, "on");
            let error = sqlx::query("CREATE TABLE must_not_write (id int)")
                .execute(&verifier)
                .await
                .unwrap_err();
            assert_eq!(
                error.as_database_error().unwrap().code().as_deref(),
                Some("25006")
            );
            verifier.acquire().await.unwrap().close().await.unwrap();
        }
    }
}
