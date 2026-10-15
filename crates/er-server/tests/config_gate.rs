//! S1: every negative case fails synchronously, before DB/JWKS/AWS/provider I/O.
use er_server::config::{AppEnv, BootstrapConfig, Config, MigrationConfig, WorkerConfig};
use std::collections::BTreeMap;

fn values() -> BTreeMap<String, String> {
    [
        ("APP_ENV", "dev"),
        ("AWS_REGION", "us-east-1"),
        ("DATABASE_URL", "postgres://synthetic:secret@localhost/test"),
        ("BIND_ADDR", "127.0.0.1:0"),
        ("AUTH_MODE", "dev"),
        ("DEV_JWKS_PATH", "unused"),
        ("AUDIO_BUCKET", "test-audio"),
        ("STT_QUEUE", "test-queue"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect()
}

fn cloud() -> BTreeMap<String, String> {
    let mut vars = values();
    vars.remove("DEV_JWKS_PATH");
    vars.remove("DATABASE_URL");
    for (key, value) in [
        ("APP_ENV", "staging"),
        ("AUTH_MODE", "cognito"),
        ("COGNITO_REGION", "us-east-1"),
        ("COGNITO_POOL_ID", "us-east-1_synthetic"),
        ("COGNITO_CLIENT_ID", "synthetic"),
        ("TRANSPORT", "bedrock"),
        ("SUMMARY_MODEL", "sonnet"),
        ("MDM_MODEL", "sonnet"),
        // Manifest placement plus the two fields ECS injects from the process's own
        // login secret. URL delimiters in the password must stay inert.
        ("DB_HOST", "db.synthetic.internal"),
        ("DB_PORT", "5432"),
        ("DB_NAME", "ernote"),
        ("DB_USERNAME", "er_api"),
        ("DB_PASSWORD", "synthetic-p@ss/w:rd?#%"),
    ] {
        vars.insert(key.into(), value.into());
    }
    // Parsing checks file presence; real certificate trust/hostname is tested at
    // connection time. This is intentionally NOT a deployable CA bundle.
    vars.insert(
        "DB_SSL_ROOT_CERT".into(),
        std::env::current_exe().unwrap().display().to_string(),
    );
    vars
}

#[test]
fn environment_is_explicit_and_dev_providers_remain_optional() {
    let mut vars = values();
    assert_eq!(Config::from_values(&vars).unwrap().app_env, AppEnv::Dev);
    assert!(
        WorkerConfig::from_values(&vars)
            .unwrap()
            .deepgram_api_key
            .is_none()
    );
    for value in [None, Some(""), Some("production")] {
        vars.remove("APP_ENV");
        if let Some(value) = value {
            vars.insert("APP_ENV".into(), value.into());
        }
        assert!(Config::from_values(&vars).is_err());
    }
}

#[test]
fn unsafe_cloud_configuration_is_rejected_without_echoing_values() {
    for (key, value) in [
        ("AUTH_MODE", "dev"),
        ("DEV_JWKS_PATH", "synthetic-secret"),
        ("JWKS_URL", "https://synthetic-secret"),
        ("TRANSPORT", "direct"),
        ("ANTHROPIC_KEY", "synthetic-secret"),
        ("S3_ENDPOINT_URL", "https://synthetic-secret"),
        ("SQS_ENDPOINT_URL", "https://synthetic-secret"),
        ("AWS_ENDPOINT_URL", "https://synthetic-secret"),
        ("AWS_ENDPOINT_URL_S3", "https://synthetic-secret"),
        ("AWS_ENDPOINT_URL_SQS", "https://synthetic-secret"),
        (
            "AWS_ENDPOINT_URL_BEDROCK_RUNTIME",
            "https://synthetic-secret",
        ),
        ("AWS_ACCESS_KEY_ID", "synthetic-secret"),
        ("AWS_SECRET_ACCESS_KEY", "synthetic-secret"),
        ("AWS_SESSION_TOKEN", "synthetic-secret"),
        ("AWS_PROFILE", "synthetic-secret"),
        ("AWS_CONFIG_FILE", "synthetic-secret"),
        ("AWS_SHARED_CREDENTIALS_FILE", "synthetic-secret"),
        ("AWS_CA_BUNDLE", "synthetic-secret"),
        ("AWS_WEB_IDENTITY_TOKEN_FILE", "synthetic-secret"),
        ("DEEPGRAM_API_KEY", "synthetic-secret"),
        ("DB_MAX_CONNECTIONS", "0"),
        ("MAX_AUDIO_BYTES", "0"),
        ("MAX_AUDIO_BYTES", "67108865"),
        ("DB_ACQUIRE_TIMEOUT_SECS", "0"),
        ("DB_CONNECT_TIMEOUT_SECS", "31"),
        (
            "DATABASE_URL",
            "postgres://user:synthetic-secret@host/db?sslmode=verify-full",
        ),
        ("DB_HOST", "/tmp/synthetic-secret"),
        ("DB_HOST", "synthetic-secret.example:5432"),
        ("DB_PORT", "synthetic-secret"),
        ("DB_PORT", "0"),
        ("DB_NAME", "synthetic-secret;"),
        ("DB_USERNAME", "synthetic-secret@db"),
        ("DB_PASSWORD", ""),
        ("PGHOST", "/tmp/synthetic-secret"),
        ("AWS_BEARER_TOKEN_BEDROCK", "synthetic-secret"),
        ("COGNITO_REGION", "evil.example/path"),
    ] {
        let mut vars = cloud();
        vars.insert(key.into(), value.into());
        let error = Config::from_values(&vars)
            .err()
            .unwrap_or_else(|| panic!("accepted {key}"));
        assert!(
            !format!("{error:#}").contains("synthetic-secret"),
            "leaked {key}"
        );
    }
}

#[test]
fn cloud_roles_require_only_their_own_dependencies() {
    let mut vars = cloud();
    assert!(Config::from_values(&vars).is_ok());
    vars.remove("BIND_ADDR");
    vars.remove("AUTH_MODE");
    for key in ["COGNITO_REGION", "COGNITO_POOL_ID", "COGNITO_CLIENT_ID"] {
        vars.remove(key);
    }
    for key in ["BEDROCK_SUMMARY_MODEL_ID", "BEDROCK_MDM_MODEL_ID"] {
        vars.insert(key.into(), "us.anthropic.claude-sonnet-4-6".into());
    }
    assert!(
        WorkerConfig::from_values(&vars).is_err(),
        "no mock STT in cloud"
    );
    vars.insert("DEEPGRAM_API_KEY".into(), "synthetic-key".into());
    assert!(WorkerConfig::from_values(&vars).is_ok());
    for key in [
        "TRANSPORT",
        "SUMMARY_MODEL",
        "MDM_MODEL",
        "BEDROCK_SUMMARY_MODEL_ID",
        "BEDROCK_MDM_MODEL_ID",
    ] {
        let mut missing = vars.clone();
        missing.remove(key);
        assert!(
            WorkerConfig::from_values(&missing).is_err(),
            "must explicitly set {key}"
        );
    }
    vars.remove("DEEPGRAM_API_KEY");
    vars.remove("AUDIO_BUCKET");
    vars.remove("STT_QUEUE");
    vars.remove("TRANSPORT");
    vars.remove("SUMMARY_MODEL");
    vars.remove("MDM_MODEL");
    vars.remove("BEDROCK_SUMMARY_MODEL_ID");
    vars.remove("BEDROCK_MDM_MODEL_ID");
    assert!(MigrationConfig::from_values(&vars).is_ok());
    vars.insert(
        "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI".into(),
        "/v2/credentials/synthetic".into(),
    );
    vars.insert(
        "AWS_CONTAINER_AUTHORIZATION_TOKEN".into(),
        "synthetic-token".into(),
    );
    assert!(
        MigrationConfig::from_values(&vars).is_ok(),
        "ECS task-role variables must be preserved"
    );
}

/// S6: cloud composes its login from manifest placement and the `username` and
/// `password` fields of the process's own login secret. A URL is a development
/// convenience and never a second source beside those fields.
#[test]
fn cloud_database_login_comes_from_secret_fields_not_a_url() {
    let parts = [
        "DB_HOST",
        "DB_PORT",
        "DB_NAME",
        "DB_USERNAME",
        "DB_PASSWORD",
    ];
    let vars = cloud();
    for key in parts {
        let mut missing = vars.clone();
        missing.remove(key);
        assert!(
            MigrationConfig::from_values(&missing).is_err(),
            "cloud must set {key}"
        );
    }
    let mut url = vars;
    for key in parts {
        url.remove(key);
    }
    url.insert(
        "DATABASE_URL".into(),
        "postgres://er_api:synthetic@db.synthetic.internal/ernote".into(),
    );
    assert!(
        MigrationConfig::from_values(&url).is_err(),
        "cloud must not accept a URL-shaped credential"
    );

    for key in parts {
        for value in ["", " ", "localhost"] {
            let mut dev = values();
            dev.insert(key.into(), value.into());
            assert!(
                MigrationConfig::from_values(&dev).is_err(),
                "DATABASE_URL conflicts with {key}, including blank values"
            );
        }
    }
    let mut dev = values();
    dev.remove("DATABASE_URL");
    for (key, value) in [
        ("DB_HOST", "localhost"),
        ("DB_PORT", "5433"),
        ("DB_NAME", "ernote"),
        ("DB_USERNAME", "postgres"),
        ("DB_PASSWORD", "dev"),
    ] {
        dev.insert(key.into(), value.into());
    }
    assert!(MigrationConfig::from_values(&dev).is_ok());
    let mut unknown = values();
    unknown.insert(
        "DATABASE_URL".into(),
        "postgres://synthetic:secret@localhost/test?unknown=1".into(),
    );
    assert!(MigrationConfig::from_values(&unknown).is_err());
}

/// S6: the bootstrap connects with the master credential in `DB_*` and receives
/// each login's staged value under its own names. Unsafe logins never start it.
#[test]
fn bootstrap_requires_four_distinct_generated_logins() {
    const STAGED: &str = "synthetic-secret-0123456789-abcdefgh";
    let mut vars = cloud();
    for (prefix, name) in [
        ("OWNER", "er_schema_owner"),
        ("API", "er_api_login"),
        ("WORKER", "er_worker_login"),
        ("ONBOARDING", "er_onboarding_login"),
    ] {
        vars.insert(format!("{prefix}_DB_USERNAME"), name.into());
        vars.insert(format!("{prefix}_DB_PASSWORD"), STAGED.into());
    }
    assert!(BootstrapConfig::from_values(&vars).is_ok());
    for (key, value) in [
        ("OWNER_DB_PASSWORD", ""),
        ("OWNER_DB_PASSWORD", "synthetic-secret"),
        ("API_DB_PASSWORD", "synthetic-secret 0123456789 abcdefghij"),
        ("WORKER_DB_USERNAME", "er_api_login"),
        ("API_DB_USERNAME", "er_api_runtime"),
        ("OWNER_DB_USERNAME", "Er_Schema_Owner"),
        ("WORKER_DB_USERNAME", "pg_synthetic"),
    ] {
        let mut unsafe_vars = vars.clone();
        unsafe_vars.insert(key.into(), value.into());
        let error = BootstrapConfig::from_values(&unsafe_vars)
            .err()
            .unwrap_or_else(|| panic!("accepted {key}"));
        assert!(
            !format!("{error:#}").contains("synthetic-secret"),
            "leaked {key}"
        );
    }
    for key in ["OWNER_DB_PASSWORD", "WORKER_DB_USERNAME"] {
        let mut missing = vars.clone();
        missing.remove(key);
        assert!(
            BootstrapConfig::from_values(&missing).is_err(),
            "must set {key}"
        );
    }
}

#[test]
fn endpoint_pair_and_approved_model_family_are_enforced() {
    let mut vars = values();
    vars.insert("S3_ENDPOINT_URL".into(), "http://localhost:9000".into());
    assert!(Config::from_values(&vars).is_err());
    vars.insert("SQS_ENDPOINT_URL".into(), "http://localhost:9324".into());
    assert!(
        Config::from_values(&vars)
            .unwrap()
            .uses_aws_endpoint_overrides()
    );
    let mut vars = cloud();
    vars.insert("DEEPGRAM_API_KEY".into(), "synthetic".into());
    for key in ["BEDROCK_SUMMARY_MODEL_ID", "BEDROCK_MDM_MODEL_ID"] {
        vars.insert(key.into(), "us.anthropic.claude-sonnet-4-6".into());
    }
    assert!(WorkerConfig::from_values(&vars).is_ok());
    for key in ["BEDROCK_SUMMARY_MODEL_ID", "BEDROCK_MDM_MODEL_ID"] {
        for id in [
            "future-sonnet-4-6",
            "anthropic.claude-haiku-4-5",
            "global.anthropic.claude-sonnet-4-6",
            "eu.anthropic.claude-sonnet-4-6",
            "us.anthropic.claude-haiku-4-5",
            "arn:aws:bedrock:profile",
        ] {
            let mut invalid = vars.clone();
            invalid.insert(key.into(), id.into());
            assert!(
                WorkerConfig::from_values(&invalid).is_err(),
                "{key} accepted {id}"
            );
        }
        // The in-Region ID and the US geographic profile are the two reviewed routes.
        for id in [
            "anthropic.claude-sonnet-4-6",
            "us.anthropic.claude-sonnet-4-6",
        ] {
            let mut valid = vars.clone();
            valid.insert(key.into(), id.into());
            let summarizer = WorkerConfig::from_values(&valid).unwrap().summarizer;
            let chosen = if key == "BEDROCK_SUMMARY_MODEL_ID" {
                summarizer.summary_model
            } else {
                summarizer.mdm_model
            };
            assert_eq!(chosen.bedrock, id, "{key} must route to exactly {id}");
        }
    }
    vars.insert("DB_MAX_CONNECTIONS".into(), "1".into());
    assert!(
        WorkerConfig::from_values(&vars).is_err(),
        "worker lock and repository need separate connections"
    );
}

#[test]
fn missing_app_env_exits_before_opening_a_developer_dotenv_or_network() {
    for binary in [
        env!("CARGO_BIN_EXE_er-server"),
        env!("CARGO_BIN_EXE_er-worker"),
    ] {
        let output = std::process::Command::new(binary)
            .env_clear()
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("APP_ENV is required"));
    }
}

/// S5 provisions the cloud queue pair from these two facts, and Terraform has no way
/// to read them: `infra/modules/data/queues.tf` creates `<name>` and `<name>-dlq`, and
/// sets the queue's visibility timeout to the worker's lease. If either constant moves
/// here, the deployed queues stop matching what the processes look up at startup —
/// `resolve_queue_urls` fails on a name that does not exist, or SQS redelivers a job the
/// worker is still running. Pin them so that rename fails here instead of in staging.
#[test]
fn deployed_queue_names_and_lease_match_the_provisioned_pair() {
    let mut vars = values();
    vars.insert("STT_QUEUE".into(), "ernote-staging-jobs".into());

    assert_eq!(
        Config::from_values(&vars).unwrap().stt_dlq(),
        "ernote-staging-jobs-dlq",
        "infra/modules/data/queues.tf names the dead-letter queue <queue>-dlq"
    );
    assert_eq!(
        WorkerConfig::from_values(&vars).unwrap().stt_dlq(),
        "ernote-staging-jobs-dlq",
        "the worker must resolve the same dead-letter queue the API does"
    );
    assert_eq!(
        er_server::config::WorkerLimits::default().visibility_seconds,
        300,
        "infra/modules/data/queues.tf sets visibility_timeout_seconds to this lease"
    );
}
