//! S1: every negative case fails synchronously, before DB/JWKS/AWS/provider I/O.
use er_server::config::{AppEnv, Config, MigrationConfig, WorkerConfig};
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
    for (key, value) in [
        ("APP_ENV", "staging"),
        ("AUTH_MODE", "cognito"),
        ("COGNITO_REGION", "us-east-1"),
        ("COGNITO_POOL_ID", "us-east-1_synthetic"),
        ("COGNITO_CLIENT_ID", "synthetic"),
        ("TRANSPORT", "bedrock"),
        ("SUMMARY_MODEL", "sonnet"),
        ("MDM_MODEL", "sonnet"),
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
            "postgres://user:synthetic-secret@host/db?sslmode=disable",
        ),
        (
            "DATABASE_URL",
            "postgres://user:synthetic-secret@host/db?ssl-mode=disable",
        ),
        (
            "DATABASE_URL",
            "postgres://user:synthetic-secret@host/db?host=/tmp",
        ),
        ("DATABASE_URL", "postgres://user:synthetic-secret@%2Ftmp/db"),
        (
            "DATABASE_URL",
            "postgres://user:synthetic-secret@host/db?unknown=synthetic-secret",
        ),
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
