//! Execute the actual k6 script against disposable real storage and mock providers.
//! Explicit opt-in because k6 is an operator/CI tool, not a Rust dependency.
use er_ai::summarizer::{AppSummarizer, MockSummarizer};
use er_pipeline::speech_to_text::{AppSpeechToText, MockSpeechToText};
use er_server::{
    repository::PostgresRepository,
    worker::{self, WorkerCtx},
};
use sqlx::PgPool;
mod common;
#[sqlx::test]
#[ignore = "requires K6_BIN; run with isolated ci-services fixtures"]
async fn k6_profile_smoke(pool: PgPool) {
    let exe = std::env::var("K6_BIN").expect("K6_BIN required");
    common::seed_org_and_clinician(&pool, common::ORG_A, "load-doc").await;
    let audio = common::ensure_test_audio_infra().await;
    let base = common::spawn_server(er_server::routes::router(
        common::test_state_with_audio(pool.clone(), &audio).await,
    ))
    .await;
    let ctx = WorkerCtx {
        pool: pool.clone(),
        s3: audio.s3.clone(),
        sqs: audio.sqs.clone(),
        bucket: audio.config.bucket.clone(),
        queue_url: audio.config.stt_queue_url.clone(),
        stt: AppSpeechToText::Mock(MockSpeechToText::new()),
        summarizer: AppSummarizer::Mock(MockSummarizer::new()),
        wait_time_seconds: 0,
        limits: Default::default(),
    };
    let consumer = tokio::spawn(async move {
        loop {
            worker::poll_once(&ctx).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    });
    let dir = std::env::temp_dir().join(format!("ernote-load-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let creds = dir.join("credentials.json");
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&creds)
        .unwrap();
    file.write_all(
        serde_json::to_string(&serde_json::json!([{"token":common::mint("load-doc")}]))
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    let wav = dir.join("synthetic.wav");
    std::fs::write(&wav, common::tiny_wav(b"synthetic load transcript")).unwrap();
    let output = tokio::process::Command::new(exe)
        .args(["run", "../../load/shift.js"])
        .env("BASE_URL", base)
        .env("ACK_SYNTHETIC_LOAD", "yes")
        .env("PROVIDERS", "mock")
        .env("CLINICIANS", "1")
        .env("PATIENTS", "1")
        .env("ENCOUNTERS", "1")
        .env("SHIFT_SECONDS", "1")
        .env("POLL_SECONDS", "1")
        .env("CREDENTIALS_FILE", &creds)
        .env("AUDIO_FILE", &wav)
        .env("SUMMARY_FILE", dir.join("summary.json"))
        .output()
        .await
        .unwrap();
    consumer.abort();
    std::fs::remove_dir_all(dir).unwrap();
    assert!(
        output.status.success(),
        "k6 failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM usage_log WHERE kind='stt'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
    assert_eq!(audio.drain_jobs().await.len(), 0);
    let repo = PostgresRepository::for_org(pool.clone(), common::ORG_A);
    let p: uuid::Uuid = sqlx::query_scalar("SELECT id FROM patients LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    repo.record_stt_usage(uuid::Uuid::nil(), p, 60_000, "reviewed-stt", Some(9_000))
        .await
        .unwrap();
    repo.record_stt_usage(uuid::Uuid::nil(), p, 60_000, "reviewed-stt", Some(9_000))
        .await
        .unwrap();
    let cost: i64 =
        sqlx::query_scalar("SELECT cost_micros FROM usage_log WHERE model='reviewed-stt'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(cost, 9_000);
}
