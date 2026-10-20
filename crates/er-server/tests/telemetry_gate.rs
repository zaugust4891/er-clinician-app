//! S9 log-leakage gate: fabricated canaries in query strings, headers, tokens,
//! token subjects/kids, bodies and invalid enum/id values are sent through the
//! real router under the cloud log filter and JSON format. The captured
//! application log must contain none of them, and every request line must carry
//! only the allowlisted fields.
use std::sync::{Arc, Mutex};

use axum::{body::Body, http::Request};
use sqlx::PgPool;
use tower::ServiceExt;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt};
use uuid::Uuid;

use er_server::{routes, telemetry};

mod common;
use common::{ORG_A, mint, seed_org_and_clinician, test_state};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Capture {
    fn lines(&self) -> Vec<serde_json::Value> {
        let text = String::from_utf8(self.0.lock().unwrap().clone()).unwrap();
        text.lines()
            .map(|line| serde_json::from_str(line).expect("every line is JSON"))
            .collect()
    }
}

/// Exactly the cloud subscriber: staging filter, flattened JSON.
fn cloud_subscriber(capture: &Capture) -> impl tracing::Subscriber + Send + Sync {
    let settings = telemetry::TelemetrySettings::from_values("api", Some("staging"), None, None);
    assert!(settings.json && settings.cloud);
    let writer = capture.clone();
    tracing_subscriber::registry()
        .with(EnvFilter::try_new(&settings.filter).unwrap())
        .with(telemetry::json_layer(move || writer.clone()))
}

const CANARIES: &[&str] = &[
    "CANARYDB",
    "CANARYPROVIDER",
    "CANARYCONFIG",
    "CANARYSDK",
    "CANARYSQL",
    "CANARYQUERY",
    "CANARYAUTH",
    "CANARYCOOKIE",
    "CANARYAGENT",
    "CANARYHEADER",
    "CANARYREQUEST",
    "CANARYPATH",
    "CANARYBODY",
    "CANARYENUM",
    "CANARYKID",
    "CANARYSUB",
    "CANARYCOMPLAINT",
];

fn token_with_kid(kid: &str) -> String {
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(kid.to_string());
    let pem = std::fs::read("../../dev/keys/dev_private.pem").unwrap();
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(&pem).unwrap();
    let claims = serde_json::json!({
        "sub": "telemetry-doc",
        "iss": er_server::config::DEV_ISSUER,
        "token_use": "access",
        "exp": jsonwebtoken::get_current_timestamp() + 3600,
    });
    jsonwebtoken::encode(&header, &claims, &key).unwrap()
}

async fn send(app: &axum::Router, request: Request<Body>) -> (u16, Option<String>) {
    let response = app.clone().oneshot(request).await.unwrap();
    let id = response
        .headers()
        .get(telemetry::REQUEST_ID_HEADER)
        .map(|value| value.to_str().unwrap().to_owned());
    (response.status().as_u16(), id)
}

#[sqlx::test]
async fn canaries_never_reach_application_logs(pool: PgPool) {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(cloud_subscriber(&capture));

    seed_org_and_clinician(&pool, ORG_A, "telemetry-doc").await;
    let app = routes::router(test_state(pool.clone()).await);
    let token = mint("telemetry-doc");
    let patient = Uuid::now_v7();

    // Fake bearer token, cookie, raw user agent, arbitrary header, query string.
    let (status, request_id) = send(
        &app,
        Request::get(format!("/v1/patients/{patient}/note?secret=CANARYQUERY"))
            .header("authorization", "Bearer CANARYAUTH.CANARYAUTH.CANARYAUTH")
            .header("cookie", "session=CANARYCOOKIE")
            .header("user-agent", "CANARYAGENT/1.0")
            .header("x-clinical-note", "CANARYHEADER")
            .header("x-request-id", "CANARYREQUEST")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, 401);
    let request_id = request_id.expect("server-generated request id");

    // Unknown kid and an unknown (but validly signed) subject.
    let (status, _) = send(
        &app,
        Request::get("/v1/me")
            .header(
                "authorization",
                format!("Bearer {}", token_with_kid("CANARYKID")),
            )
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, 401);
    let (status, _) = send(
        &app,
        Request::get("/v1/me")
            .header("authorization", format!("Bearer {}", mint("CANARYSUB")))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, 401);

    // Unmatched path.
    let (status, _) = send(
        &app,
        Request::get("/v1/CANARYPATH/x?q=CANARYQUERY")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, 404);

    // Malformed id and an invalid enum value: serde echoes both in its error.
    let (status, _) = send(
        &app,
        Request::post("/v1/patients")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"patient_id":"CANARYBODY","chief_complaint":"CANARYCOMPLAINT"}"#,
            ))
            .unwrap(),
    )
    .await;
    assert!((400..500).contains(&status), "{status}");
    let (status, _) = send(
        &app,
        Request::post(format!("/v1/patients/{patient}/events"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"events":[{"event_id":"00000000-0000-0000-0000-000000000001","event":{"CANARYENUM":{}}}]}"#,
            ))
            .unwrap(),
    )
    .await;
    assert!((400..500).contains(&status), "{status}");

    // A successful clinical write: the chief complaint is content, not telemetry.
    let (status, _) = send(
        &app,
        Request::post("/v1/patients")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "patient_id": patient,
                    "chief_complaint": "CANARYCOMPLAINT chest pain",
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, 200);

    // A real PostgreSQL cast error carries the invalid bind in its message.
    let db_error = sqlx::query("SELECT $1::text::integer")
        .bind("CANARYDB")
        .execute(&pool)
        .await
        .unwrap_err();
    use axum::response::IntoResponse;
    let _ = er_server::error::ApiError::Internal(db_error.into()).into_response();
    let _ = er_server::error::ApiError::Internal(
        er_ai::error::AiError::Transport("CANARYPROVIDER".into()).into(),
    )
    .into_response();
    er_server::telemetry::StartupFailure::config(anyhow::anyhow!("CANARYCONFIG")).report();
    tracing::error!(target: "aws_smithy_runtime", error = "CANARYSDK", "raw upstream error");
    tracing::error!(target: "sqlx::query", error = "CANARYSQL", "raw query error");
    let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    for canary in CANARIES {
        assert!(!text.contains(canary), "{canary} leaked into logs");
    }
    assert!(!text.contains(&patient.to_string()), "patient id leaked");

    let lines = capture.lines();
    let requests: Vec<_> = lines
        .iter()
        .filter(|line| line["message"] == "request completed")
        .collect();
    assert_eq!(requests.len(), 7);
    let allowed = [
        "timestamp",
        "level",
        "target",
        "message",
        "span",
        "method",
        "route",
        "status",
        "duration_ms",
        "service",
        "release",
    ];
    for line in &requests {
        for key in line.as_object().unwrap().keys() {
            assert!(allowed.contains(&key.as_str()), "unexpected field {key}");
        }
        assert!(line["span"]["request_id"].is_string());
    }
    let routes: Vec<_> = requests
        .iter()
        .map(|line| line["route"].as_str().unwrap())
        .collect();
    assert!(routes.contains(&"/v1/patients/{id}/note"));
    assert!(routes.contains(&telemetry::UNMATCHED_ROUTE));
    assert_eq!(requests[0]["span"]["request_id"], request_id.as_str());

    // Auth rejections are present, as codes.
    let reasons: Vec<_> = lines
        .iter()
        .filter(|line| line["message"] == "auth rejected")
        .map(|line| line["reason"].as_str().unwrap().to_owned())
        .collect();
    assert!(reasons.contains(&"jwt_invalid".to_owned()), "{reasons:?}");
    assert!(reasons.contains(&"unknown_kid".to_owned()), "{reasons:?}");
    assert!(
        reasons.contains(&"unknown_clinician".to_owned()),
        "{reasons:?}"
    );
}

#[sqlx::test]
async fn successful_probes_are_not_logged(pool: PgPool) {
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(cloud_subscriber(&capture));
    let app = routes::router(test_state(pool).await);
    let (status, _) = send(&app, Request::get("/healthz").body(Body::empty()).unwrap()).await;
    assert_eq!(status, 200);
    assert!(
        capture
            .lines()
            .iter()
            .all(|line| line["message"] != "request completed")
    );
}

#[sqlx::test]
async fn stalled_handler_and_failed_receive_cannot_emit_progress(pool: PgPool) {
    use er_ai::summarizer::{AppSummarizer, MockSummarizer};
    use er_pipeline::speech_to_text::{AppSpeechToText, MockSpeechToText};
    use er_server::{
        repository::{NewChunk, PostgresRepository},
        worker::{self, WorkerCtx},
    };
    use std::time::Duration;

    seed_org_and_clinician(&pool, ORG_A, "worker-telemetry-doc").await;
    let audio = common::ensure_test_audio_infra().await;
    let repo = PostgresRepository::for_org(pool.clone(), ORG_A);
    let chunk_id = Uuid::now_v7();
    repo.insert_chunk(NewChunk {
        id: chunk_id,
        patient_id: Uuid::now_v7(),
        encounter_id: Uuid::now_v7(),
        chunk_index: 0,
        is_final: true,
        content_type: "audio/wav",
        s3_key: "CANARYOBJECTKEY",
    })
    .await
    .unwrap();
    repo.mark_chunk_uploaded(chunk_id).await.unwrap();
    er_server::queue::send_job(
        &audio.sqs,
        &audio.config.stt_queue_url,
        &er_server::queue::Job::TranscribeChunk { chunk_id },
    )
    .await
    .unwrap();
    let mut ctx = WorkerCtx {
        pool: pool.clone(),
        s3: audio.s3,
        sqs: audio.sqs,
        bucket: audio.config.bucket.clone(),
        queue_url: audio.config.stt_queue_url.clone(),
        stt: AppSpeechToText::Mock(MockSpeechToText::new()),
        summarizer: AppSummarizer::Mock(MockSummarizer::new()),
        wait_time_seconds: 0,
        limits: Default::default(),
    };
    // Hold the real row lock that mark_chunk_transcribing needs. The lease can
    // remain healthy, but a detached heartbeat would incorrectly report progress.
    let mut lock = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM audio_chunks WHERE id = $1 FOR UPDATE")
        .bind(chunk_id)
        .fetch_one(&mut *lock)
        .await
        .unwrap();
    let capture = Capture::default();
    // Even the quietest cloud setting must retain delivery correlation and progress.
    let writer = capture.clone();
    let settings =
        telemetry::TelemetrySettings::from_values("worker", Some("staging"), Some("error"), None);
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::registry()
            .with(EnvFilter::new(settings.filter))
            .with(telemetry::json_layer(move || writer.clone())),
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(250), worker::poll_once(&ctx))
            .await
            .is_err()
    );
    assert!(
        capture
            .lines()
            .iter()
            .all(|line| line["metric_name"] != "worker_progress")
    );
    lock.rollback().await.unwrap();

    // A completed idle poll really does advance progress (the canceled delivery
    // is still leased). A malformed body is classified without logging the text.
    ctx.sqs
        .send_message()
        .queue_url(&ctx.queue_url)
        .message_body("{\"kind\":\"CANARYQUEUEBODY\"}")
        .send()
        .await
        .unwrap();
    assert_eq!(worker::poll_once(&ctx).await.unwrap(), 1);
    assert!(
        capture
            .lines()
            .iter()
            .any(|line| line["metric_name"] == "worker_progress")
    );
    let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    for forbidden in ["CANARYQUEUEBODY", "CANARYOBJECTKEY", &chunk_id.to_string()] {
        assert!(!text.contains(forbidden));
    }
    assert!(
        capture
            .lines()
            .iter()
            .any(|line| line["error_kind"] == "invalid_job_message"
                && line["span"]["delivery_id"].is_string())
    );

    capture.0.lock().unwrap().clear();
    ctx.queue_url.push_str("-missing");
    assert!(worker::poll_once(&ctx).await.is_err());
    assert!(
        capture
            .lines()
            .iter()
            .all(|line| line["metric_name"] != "worker_progress")
    );
}
