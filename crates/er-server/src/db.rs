//! Shared connection bounds and cloud TLS policy; connection errors are redacted.
use crate::config::AppEnv;
use sqlx::{
    ConnectOptions, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions, PgSslMode},
};
use std::{path::PathBuf, str::FromStr, time::Duration};

#[derive(Clone)]
pub struct DatabaseConfig {
    options: PgConnectOptions,
    pub max_connections: u32,
    pub acquire_timeout: Duration,
    pub connect_timeout: Duration,
}

impl DatabaseConfig {
    pub fn new(
        url: String,
        env: AppEnv,
        process: &'static str,
        root_cert: Option<PathBuf>,
        max_connections: u32,
        acquire_timeout: Duration,
        connect_timeout: Duration,
    ) -> anyhow::Result<Self> {
        let parsed =
            reqwest::Url::parse(&url).map_err(|_| anyhow::anyhow!("DATABASE_URL is invalid"))?;
        anyhow::ensure!(
            matches!(parsed.scheme(), "postgres" | "postgresql") && parsed.host_str().is_some(),
            "DATABASE_URL must specify a Postgres host"
        );
        // SQLx logs unrecognized query values while parsing. Reject them BEFORE
        // handing over the URL, and keep cloud connection routing unambiguous.
        for (key, value) in parsed.query_pairs() {
            let allowed = if env != AppEnv::Dev {
                matches!(key.as_ref(), "sslmode" | "ssl-mode" | "application_name")
            } else {
                matches!(
                    key.as_ref(),
                    "sslmode"
                        | "ssl-mode"
                        | "sslrootcert"
                        | "ssl-root-cert"
                        | "ssl-ca"
                        | "sslcert"
                        | "ssl-cert"
                        | "sslkey"
                        | "ssl-key"
                        | "statement-cache-capacity"
                        | "host"
                        | "hostaddr"
                        | "port"
                        | "dbname"
                        | "user"
                        | "password"
                        | "application_name"
                        | "options"
                ) || key.starts_with("options[")
            };
            anyhow::ensure!(
                allowed,
                "DATABASE_URL contains an unsupported connection parameter"
            );
            if env != AppEnv::Dev && matches!(key.as_ref(), "sslmode" | "ssl-mode") {
                anyhow::ensure!(
                    value == "verify-full",
                    "cloud DATABASE_URL must not weaken verify-full TLS"
                );
            }
        }
        if env != AppEnv::Dev {
            anyhow::ensure!(
                !parsed.username().is_empty()
                    && parsed.password().is_some_and(|p| !p.is_empty())
                    && !parsed.path().trim_matches('/').is_empty(),
                "cloud DATABASE_URL must explicitly specify login, password and database"
            );
        }
        let mut options = PgConnectOptions::from_str(&url)
            .map_err(|_| anyhow::anyhow!("DATABASE_URL is invalid"))?
            .application_name(&format!("er-{process}"))
            .disable_statement_logging();
        if process == "migration" {
            // PostgreSQL enforces these on every migration session, including
            // SQLx's advisory-lock wait and each DDL statement. The executable
            // also bounds the whole migration run independently.
            options = options.options([("lock_timeout", "15s"), ("statement_timeout", "300s")]);
        }
        if env != AppEnv::Dev {
            anyhow::ensure!(
                options.get_socket().is_none(),
                "cloud database connections must use verified TLS over TCP"
            );
            let root = root_cert
                .ok_or_else(|| anyhow::anyhow!("DB_SSL_ROOT_CERT is required in cloud"))?;
            anyhow::ensure!(
                root.is_absolute() && root.is_file(),
                "DB_SSL_ROOT_CERT must be an existing absolute CA bundle path"
            );
            options = options.ssl_mode(PgSslMode::VerifyFull).ssl_root_cert(root);
        } else {
            // Only the explicit development path permits plaintext local Postgres.
            options = options.ssl_mode(PgSslMode::Disable);
        }
        Ok(Self {
            options,
            max_connections,
            acquire_timeout,
            connect_timeout,
        })
    }

    pub async fn connect(&self) -> anyhow::Result<PgPool> {
        // SQLx's acquire timeout also bounds new physical connections in this pool.
        let connect = PgPoolOptions::new()
            .max_connections(self.max_connections)
            .min_connections(0)
            .acquire_timeout(self.acquire_timeout)
            .connect_with(self.options.clone());
        tokio::time::timeout(self.connect_timeout, connect)
            .await
            .map_err(|_| anyhow::anyhow!("database_connect_timeout"))?
            .map_err(|_| anyhow::anyhow!("database_connect_failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cloud_options_force_hostname_verified_tls_and_application_name() {
        let config = DatabaseConfig::new(
            "postgres://test:secret@db.example/test?sslmode=verify-full&application_name=untrusted"
                .into(),
            AppEnv::Prod,
            "worker",
            Some(std::env::current_exe().unwrap()),
            5,
            Duration::from_secs(5),
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(matches!(
            config.options.get_ssl_mode(),
            PgSslMode::VerifyFull
        ));
        assert_eq!(config.options.get_application_name(), Some("er-worker"));
        assert_eq!(config.max_connections, 5);
        assert!(
            DatabaseConfig::new(
                "postgres://test:secret@db.example/test".into(),
                AppEnv::Prod,
                "api",
                None,
                5,
                Duration::from_secs(5),
                Duration::from_secs(10)
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn connection_failures_never_return_connection_strings() {
        let config = DatabaseConfig::new(
            "postgres://synthetic:must-not-leak@127.0.0.1:1/test".into(),
            AppEnv::Dev,
            "api",
            None,
            1,
            Duration::from_millis(50),
            Duration::from_millis(100),
        )
        .unwrap();
        let error = config.connect().await.unwrap_err();
        assert!(matches!(
            error.to_string().as_str(),
            "database_connect_failed" | "database_connect_timeout"
        ));
    }
}
