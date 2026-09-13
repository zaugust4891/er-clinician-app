//! `er-worker` — the transcription worker process (Step 6 S7).
//!
//! Boot and a loop. Every rule lives in [`er_server::worker`], which is a library
//! module so the tests can call `poll_once` directly; this file exists to build a
//! [`WorkerCtx`] out of the same `Config` and AWS constructors the API server
//! uses, and then hand it to `worker::run`.
//!
//! It ships in the same image as the API and migration binaries. The ECS worker
//! task uses `/usr/local/bin/er-worker` as its explicit entryPoint.

use er_ai::claude::ClaudeSummarizer;
use er_ai::summarizer::{AppSummarizer, MockSummarizer};
use er_ai::transport::ClaudeTransport;
use er_pipeline::speech_to_text::{AppSpeechToText, MockSpeechToText};
use er_server::{aws, config, worker};
use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt, util::SubscriberInitExt};

/// The API server long-polls nothing; the worker does. 20 s is SQS's maximum and
/// the right default: one held connection replaces a busy loop of empty receives,
/// which costs less and delivers a job sooner than any polling interval would
/// (§C2).
const LONG_POLL_SECONDS: i32 = 20;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let config = config::WorkerConfig::from_env()?;
    tracing::info!(environment = ?config.app_env, process = "worker", "configuration validated");

    // Same pool, same clients, same bootstrap as `AppState::build` — the worker is
    // a second consumer of the identical infrastructure, not a parallel
    // implementation of it.
    let pool = config.database.connect().await?;
    let clients = aws::configured_clients(&config.runtime).await;

    let dlq = config.stt_dlq();
    // In dev the queue may not exist yet if the worker wins the race to boot, so
    // it creates-if-missing exactly as the API does. In production the endpoint
    // overrides are absent and Terraform owns the infrastructure, so the worker
    // only resolves URLs and never attempts to mutate anything (§C6).
    let queues = if config.uses_aws_endpoint_overrides() {
        aws::ensure_audio_infra(
            &clients.s3,
            &clients.sqs,
            &config.audio_bucket,
            &config.stt_queue,
            &dlq,
        )
        .await
        .map_err(|_| anyhow::anyhow!("worker_audio_initialization_failed"))?
    } else {
        aws::resolve_queue_urls(&clients.sqs, &config.stt_queue, &dlq)
            .await
            .map_err(|_| anyhow::anyhow!("worker_queue_resolution_failed"))?
    };

    let stt = build_transcriber(&config)?;
    let summarizer = build_summarizer(&config).await?;

    let ctx = worker::WorkerCtx {
        pool,
        s3: clients.s3,
        sqs: clients.sqs,
        bucket: config.audio_bucket.clone(),
        queue_url: queues.stt.clone(),
        stt,
        summarizer,
        wait_time_seconds: LONG_POLL_SECONDS,
        limits: config.limits,
    };

    tracing::info!(
        bucket = %ctx.bucket,
        queue_url = %ctx.queue_url,
        dlq_url = %queues.stt_dlq,
        long_poll_seconds = LONG_POLL_SECONDS,
        "transcription worker ready"
    );

    worker::run(&ctx, Box::pin(shutdown_signal())).await;
    Ok(())
}

/// Select the AI transport once at boot. Direct mode uses an API key and keeps
/// the offline mock fallback; Bedrock uses the normal AWS credential chain and
/// therefore has no application secret to distribute.
async fn build_summarizer(config: &config::WorkerConfig) -> anyhow::Result<AppSummarizer> {
    anyhow::ensure!(
        config.app_env == config::AppEnv::Dev
            || config.ai_transport == config::AiTransportMode::Bedrock,
        "cloud AI requires Bedrock"
    );
    let transport = match config.ai_transport {
        config::AiTransportMode::Direct => match config.anthropic_api_key.as_deref() {
            Some(key) => ClaudeTransport::direct(key)?,
            None => {
                tracing::warn!(
                    ai = "mock",
                    "TRANSPORT=direct but ANTHROPIC_KEY is not set — generated note prose \
                     will be CANNED. This is correct for CI and offline development and is \
                     NEVER correct for real patient workflows."
                );
                return Ok(AppSummarizer::Mock(MockSummarizer::new()));
            }
        },
        config::AiTransportMode::Bedrock => {
            let shared = aws::shared_config(&config.runtime).await;
            let bedrock = aws_sdk_bedrockruntime::config::Builder::from(&shared)
                .timeout_config(aws::operation_timeouts(config.limits.provider_timeout))
                .build();
            ClaudeTransport::bedrock(aws_sdk_bedrockruntime::Client::from_conf(bedrock))
        }
    };

    tracing::info!(
        transport = ?config.ai_transport,
        summary_model = %config.summarizer.summary_model.direct,
        mdm_model = %config.summarizer.mdm_model.direct,
        "AI generation provider configured"
    );
    Ok(AppSummarizer::Claude(ClaudeSummarizer::from_transport(
        transport,
        config.summarizer.clone(),
    )))
}

/// Choose the transcriber once, at boot — the `AppSummarizer` pattern (D6.2/D6.12).
///
/// The mock branch warns rather than informs on purpose. A worker silently
/// writing `[mock transcript: 44 year old male…]` into real charts is the single
/// most expensive misconfiguration this process can have, so the log line has to
/// be impossible to skim past.
fn build_transcriber(config: &config::WorkerConfig) -> anyhow::Result<AppSpeechToText> {
    anyhow::ensure!(
        config.app_env == config::AppEnv::Dev || config.deepgram_api_key.is_some(),
        "cloud STT requires Deepgram"
    );
    match config.deepgram_api_key.as_deref() {
        Some(key) => {
            let transcriber = er_pipeline::deepgram::DeepgramTranscriber::new(key)?;
            tracing::info!(stt = "deepgram", "speech-to-text provider configured");
            Ok(AppSpeechToText::Deepgram(transcriber))
        }
        None => {
            tracing::warn!(
                stt = "mock",
                "DEEPGRAM_API_KEY is not set — every chunk will receive a CANNED transcript. \
                 This is correct for CI and offline development and is NEVER correct for \
                 real patient audio."
            );
            Ok(AppSpeechToText::Mock(MockSpeechToText::new()))
        }
    }
}

fn init_tracing() {
    let fmt_layer = if std::env::var("RUST_LOG").is_ok() {
        tracing_subscriber::fmt::layer().json().boxed()
    } else {
        tracing_subscriber::fmt::layer().pretty().boxed()
    };

    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(fmt_layer)
        .init();
}

/// SIGTERM (what ECS sends before it kills a task) and SIGINT (what Ctrl-C sends).
/// Matching `main.rs` rather than `tokio::signal::ctrl_c` so a rolling deploy
/// drains cleanly instead of abandoning an in-flight transcription.
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");

    tokio::select! {
        _ = sigterm.recv() => {},
        _ = sigint.recv() => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use er_server::config::WorkerConfig;

    fn direct_config(key: Option<&str>) -> WorkerConfig {
        WorkerConfig::from_values(
            &[
                ("APP_ENV".into(), "dev".into()),
                ("AWS_REGION".into(), "us-east-1".into()),
                (
                    "DATABASE_URL".into(),
                    "postgres://test:test@localhost/test".into(),
                ),
                ("AUDIO_BUCKET".into(), "unused".into()),
                ("STT_QUEUE".into(), "unused".into()),
                ("ANTHROPIC_KEY".into(), key.unwrap_or_default().into()),
            ]
            .into(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn direct_transport_is_activated_when_a_key_is_configured() {
        let configured = build_summarizer(&direct_config(Some("synthetic-key")))
            .await
            .unwrap();
        assert!(matches!(configured, AppSummarizer::Claude(_)));
    }

    #[tokio::test]
    async fn direct_transport_without_a_key_uses_the_explicit_offline_mock() {
        let configured = build_summarizer(&direct_config(None)).await.unwrap();
        assert!(matches!(configured, AppSummarizer::Mock(_)));
    }
}
