//! Shared connection bounds, cloud TLS and login policy; connection errors are redacted.
use crate::config::AppEnv;
use sqlx::{
    ConnectOptions, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions, PgSslMode},
};
use std::{path::PathBuf, str::FromStr, time::Duration};

/// Where a process's login comes from. Never derive Debug: `Parts` holds a password.
///
/// `Url` is the development convenience, `DATABASE_URL`. Cloud uses `Parts`: the
/// manifest's non-secret placement plus the `username` and `password` fields ECS
/// injects from the process's own login secret. Composing them with
/// `PgConnectOptions` needs no URL escaping and leaves no second, URL-shaped copy
/// of the password to go stale after a rotation.
pub enum DatabaseSource {
    Url(String),
    Parts {
        host: String,
        port: u16,
        database: String,
        username: String,
        password: String,
    },
}

#[derive(Clone)]
pub struct DatabaseConfig {
    options: PgConnectOptions,
    /// Outside dev, the process whose login `privileges::check_login` vets on connect.
    login_check: Option<&'static str>,
    pub max_connections: u32,
    pub acquire_timeout: Duration,
    pub connect_timeout: Duration,
}

impl DatabaseConfig {
    pub fn new(
        source: DatabaseSource,
        env: AppEnv,
        process: &'static str,
        root_cert: Option<PathBuf>,
        max_connections: u32,
        acquire_timeout: Duration,
        connect_timeout: Duration,
    ) -> anyhow::Result<Self> {
        let options = match source {
            DatabaseSource::Url(url) => {
                anyhow::ensure!(env == AppEnv::Dev, "DATABASE_URL is development-only");
                url_options(&url)?
            }
            DatabaseSource::Parts {
                host,
                port,
                database,
                username,
                password,
            } => parts_options(&host, port, &database, &username, &password)?,
        };
        let mut options = options
            .application_name(&format!("er-{process}"))
            .disable_statement_logging()
            .options([("search_path", "pg_catalog,public")]);
        if matches!(process, "migration" | "db-bootstrap") {
            // PostgreSQL enforces these on every migration and bootstrap session,
            // including SQLx's advisory-lock wait and each DDL statement. Each
            // executable also bounds its whole run independently.
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
        // The bootstrap's RDS master is privileged by design; the bootstrap vets
        // every login it creates instead.
        let vetted = env != AppEnv::Dev && process != "db-bootstrap";
        Ok(Self {
            options,
            login_check: vetted.then_some(process),
            max_connections,
            acquire_timeout,
            connect_timeout,
        })
    }

    pub async fn connect(&self) -> anyhow::Result<PgPool> {
        let connect = async {
            // SQLx's acquire timeout also bounds new physical connections in this pool.
            let pool = PgPoolOptions::new()
                .max_connections(self.max_connections)
                .min_connections(0)
                .acquire_timeout(self.acquire_timeout)
                .connect_with(self.options.clone())
                .await
                .map_err(|_| anyhow::anyhow!("database_connect_failed"))?;
            if let Some(process) = self.login_check {
                crate::privileges::check_login(&pool, process).await?;
            }
            Ok::<_, anyhow::Error>(pool)
        };
        tokio::time::timeout(self.connect_timeout, connect)
            .await
            .map_err(|_| anyhow::anyhow!("database_connect_timeout"))?
    }
}

fn url_options(url: &str) -> anyhow::Result<PgConnectOptions> {
    let invalid = || anyhow::anyhow!("DATABASE_URL is invalid");
    let parsed = reqwest::Url::parse(url).map_err(|_| invalid())?;
    anyhow::ensure!(
        matches!(parsed.scheme(), "postgres" | "postgresql") && parsed.host_str().is_some(),
        "DATABASE_URL must specify a Postgres host"
    );
    // SQLx logs unrecognized query values while parsing. Reject them BEFORE
    // handing over the URL.
    for (key, _) in parsed.query_pairs() {
        let allowed = matches!(
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
        ) || key.starts_with("options[");
        anyhow::ensure!(
            allowed,
            "DATABASE_URL contains an unsupported connection parameter"
        );
    }
    PgConnectOptions::from_str(url).map_err(|_| invalid())
}

fn parts_options(
    host: &str,
    port: u16,
    database: &str,
    username: &str,
    password: &str,
) -> anyhow::Result<PgConnectOptions> {
    // A bare DNS name: no socket path, port, userinfo or URL syntax. Cloud
    // verifies exactly this name against the server certificate.
    let hostname = |b: u8| b.is_ascii_alphanumeric() || b == b'.' || b == b'-';
    anyhow::ensure!(
        (1..=253).contains(&host.len())
            && !host.starts_with(['.', '-'])
            && host.bytes().all(hostname),
        "DB_HOST must be a hostname"
    );
    anyhow::ensure!(port != 0, "DB_PORT must be a TCP port");
    anyhow::ensure!(is_identifier(database), "DB_NAME is invalid");
    anyhow::ensure!(is_identifier(username), "DB_USERNAME is invalid");
    anyhow::ensure!(
        (1..=1024).contains(&password.len()) && !password.chars().any(char::is_control),
        "DB_PASSWORD is invalid"
    );
    // `new_without_pgpass` still reads PG* variables, so every field that routes
    // or authenticates the session is set here. Cloud rejects PG* at startup.
    let options = PgConnectOptions::new_without_pgpass()
        .host(host)
        .port(port)
        .database(database)
        .username(username)
        .password(password);
    Ok(options)
}

/// RDS database and login names: ASCII letters, digits and underscores.
fn is_identifier(value: &str) -> bool {
    let allowed = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    (1..=63).contains(&value.len()) && value.bytes().all(allowed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(host: &str, password: &str) -> DatabaseSource {
        DatabaseSource::Parts {
            host: host.into(),
            port: 5432,
            database: "ernote".into(),
            username: "er_api".into(),
            password: password.into(),
        }
    }

    fn cloud(source: DatabaseSource) -> anyhow::Result<DatabaseConfig> {
        DatabaseConfig::new(
            source,
            AppEnv::Prod,
            "worker",
            Some(std::env::current_exe().unwrap()),
            5,
            Duration::from_secs(5),
            Duration::from_secs(10),
        )
    }

    #[test]
    fn cloud_options_force_hostname_verified_tls_and_application_name() {
        // URL delimiters in a password are composed verbatim, never parsed.
        let config = cloud(parts("db.example", "p@ss/w:rd?#%")).unwrap();
        assert!(matches!(
            config.options.get_ssl_mode(),
            PgSslMode::VerifyFull
        ));
        assert_eq!(config.options.get_application_name(), Some("er-worker"));
        assert_eq!(config.options.get_host(), "db.example");
        assert_eq!(config.options.get_port(), 5432);
        assert_eq!(config.options.get_database(), Some("ernote"));
        assert_eq!(config.options.get_username(), "er_api");
        assert_eq!(config.max_connections, 5);
        assert_eq!(config.login_check, Some("worker"));
        assert!(
            DatabaseConfig::new(
                parts("db.example", "secret"),
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

    #[test]
    fn cloud_rejects_urls_and_non_hostname_placement_without_echoing_values() {
        for source in [
            DatabaseSource::Url("postgres://u:must-not-leak@db.example/test".into()),
            parts("/tmp/must-not-leak", "secret"),
            parts("must-not-leak.example:5432", "secret"),
            parts("user@must-not-leak.example", "secret"),
            parts("", "secret"),
            parts("db.example", ""),
            parts("db.example", "must-not-leak\n"),
        ] {
            let Err(error) = cloud(source) else {
                panic!("unsafe database source accepted");
            };
            assert!(!format!("{error:#}").contains("must-not-leak"));
        }
    }

    #[tokio::test]
    async fn connection_failures_never_return_connection_strings() {
        let url = "postgres://synthetic:must-not-leak@127.0.0.1:1/test";
        let config = DatabaseConfig::new(
            DatabaseSource::Url(url.into()),
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
