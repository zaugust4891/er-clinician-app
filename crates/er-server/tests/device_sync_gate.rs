//! Two independent SQLite files use the real authenticated API sequentially.
mod common;
use common::*;
use er_device::{AccessTokenProvider, DeviceConfig, DeviceHandle, SyncConfig};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

struct Token(String);
impl AccessTokenProvider for Token {
    fn access_token(&self, _: bool) -> er_device::Result<String> {
        Ok(self.0.clone())
    }
}
struct Phone {
    handle: Arc<DeviceHandle>,
    path: std::path::PathBuf,
}
impl Phone {
    fn open() -> Self {
        let path = std::env::temp_dir().join(format!("er-sync-{}.sqlite", Uuid::now_v7()));
        Self {
            handle: Arc::new(
                DeviceHandle::open(DeviceConfig {
                    database_path: path.to_str().unwrap().into(),
                    account_scope: "synthetic-clinician".into(),
                })
                .unwrap(),
            ),
            path,
        }
    }
}
impl Drop for Phone {
    fn drop(&mut self) {
        let _ = self.handle.close();
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", self.path.display(), suffix));
        }
    }
}
async fn round(phone: &Phone, config: &SyncConfig, token: &str) -> er_device::SyncView {
    let handle = phone.handle.clone();
    let config = config.clone();
    let token = Arc::new(Token(token.into()));
    tokio::task::spawn_blocking(move || handle.synchronize(config, token))
        .await
        .unwrap()
        .unwrap()
}
async fn settle(phone: &Phone, config: &SyncConfig, token: &str) {
    for _ in 0..8 {
        let state = round(phone, config, token).await;
        if state.status == "up_to_date" {
            return;
        }
        assert_eq!(state.status, "pending", "unexpected synchronization state");
    }
    panic!("sync did not finish within the bounded test rounds");
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[sqlx::test(migrations = "./migrations")]
async fn sequential_phones_converge_with_consent_and_keep_local_preferences_local(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "sync-physician").await;
    let origin = spawn_server(er_server::routes::router(test_state(pool.clone()).await)).await;
    let clinician: Uuid = sqlx::query_scalar("SELECT id FROM clinicians WHERE org_id=$1")
        .bind(ORG_A)
        .fetch_one(&pool)
        .await
        .unwrap();
    let config = SyncConfig {
        origin,
        organization: ORG_A,
        clinician,
    };
    let token = mint("sync-physician");
    let first = Phone::open();
    let local_shift = first
        .handle
        .execute(true, |c| c.start_shift(now()))
        .unwrap();
    let patient = first
        .handle
        .execute(true, |c| {
            let id = er_device::core::id(&c.add_patient(
                "Synthetic complaint".into(),
                Some("Room 4".into()),
                now(),
            )?)?;
            c.set_consent(
                id,
                er_device::ConsentStatus::Declined,
                "synthetic-policy".into(),
                0,
                now(),
            )?;
            c.record_data_reviewed(id, "ECG reviewed offline".into(), now())?;
            Ok(id)
        })
        .unwrap();
    settle(&first, &config, &token).await;
    let remote: Uuid = sqlx::query_scalar("SELECT id FROM shifts WHERE clinician_id=$1")
        .bind(clinician)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_ne!(local_shift, remote.to_string());
    struct CountToken {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl AccessTokenProvider for CountToken {
        fn access_token(&self, _: bool) -> er_device::Result<String> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok("unused".into())
        }
    }
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let h = first.handle.clone();
    let settings = config.clone();
    let counter = calls.clone();
    tokio::task::spawn_blocking(move || {
        h.poll_synchronization(settings, Arc::new(CountToken { calls: counter }))
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    first.handle.close().unwrap();
    let second = Phone::open();
    settle(&second, &config, &token).await;
    assert_eq!(
        second
            .handle
            .execute(false, move |c| Ok(c.note_view(patient)?.data_reviewed))
            .unwrap(),
        ["ECG reviewed offline"]
    );
    assert_eq!(
        second
            .handle
            .execute(false, |c| Ok(c.view().patients[0].consent.status.clone()))
            .unwrap(),
        er_device::ConsentStatus::Declined
    );
    second
        .handle
        .execute(true, move |c| {
            c.record_data_reviewed(patient, "Second phone review".into(), now())
        })
        .unwrap();
    settle(&second, &config, &token).await;
    second.handle.close().unwrap();
    let reopened = Phone {
        handle: Arc::new(
            DeviceHandle::open(DeviceConfig {
                database_path: first.path.to_str().unwrap().into(),
                account_scope: "synthetic-clinician".into(),
            })
            .unwrap(),
        ),
        path: first.path.clone(),
    };
    settle(&reopened, &config, &token).await;
    assert_eq!(
        reopened
            .handle
            .execute(false, move |c| Ok(c
                .note_view(patient)?
                .data_reviewed
                .len()))
            .unwrap(),
        2
    );
    let status = reopened.handle.sync_status().unwrap();
    assert!(status.local_only_preferences && status.local_only_context);
    assert_eq!(status.queued_events, 0);
    assert_eq!(status.awaiting_download, 0);
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE patient_id=$1")
        .bind(patient)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
}

#[sqlx::test(migrations = "./migrations")]
async fn wrong_identity_is_rejected_before_shift_or_patient_upload(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "sync-scope").await;
    let origin = spawn_server(er_server::routes::router(test_state(pool.clone()).await)).await;
    let phone = Phone::open();
    phone
        .handle
        .execute(true, |c| c.start_shift(now()))
        .unwrap();
    let config = SyncConfig {
        origin,
        organization: ORG_B,
        clinician: Uuid::now_v7(),
    };
    let handle = phone.handle.clone();
    let token = Arc::new(Token(mint("sync-scope")));
    let result = tokio::task::spawn_blocking(move || handle.synchronize(config, token))
        .await
        .unwrap();
    assert!(matches!(result, Err(er_device::DeviceError::Scope)));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM shifts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test(migrations = "./migrations")]
async fn partial_server_commit_then_lost_reply_converges_without_duplicate_ids(pool: PgPool) {
    use axum::{
        body::{Body, to_bytes},
        extract::State,
        http::{Request, StatusCode},
        middleware::{self, Next},
        response::{IntoResponse, Response},
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    async fn lose_first(
        State(first): State<Arc<AtomicBool>>,
        mut request: Request<Body>,
        next: Next,
    ) -> Response {
        if request.uri().path().ends_with("/events")
            && request.method() == "POST"
            && first.swap(false, Ordering::SeqCst)
        {
            let body = std::mem::replace(request.body_mut(), Body::empty());
            let bytes = to_bytes(body, 2_000_000).await.unwrap();
            let mut json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            json["events"].as_array_mut().unwrap().truncate(1);
            *request.body_mut() = Body::from(serde_json::to_vec(&json).unwrap());
            request.headers_mut().remove("content-length");
            let response = next.run(request).await;
            assert_eq!(response.status(), StatusCode::OK);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error":"synthetic_lost_reply"})),
            )
                .into_response();
        }
        next.run(request).await
    }
    seed_org_and_clinician(&pool, ORG_A, "partial-sync").await;
    let app = er_server::routes::router(test_state(pool.clone()).await).layer(
        middleware::from_fn_with_state(Arc::new(AtomicBool::new(true)), lose_first),
    );
    let origin = spawn_server(app).await;
    let clinician: Uuid = sqlx::query_scalar("SELECT id FROM clinicians WHERE org_id=$1")
        .bind(ORG_A)
        .fetch_one(&pool)
        .await
        .unwrap();
    let config = SyncConfig {
        origin,
        organization: ORG_A,
        clinician,
    };
    let token = mint("partial-sync");
    let phone = Phone::open();
    let patient = phone
        .handle
        .execute(true, |c| {
            c.start_shift(now())?;
            let p = er_device::core::id(&c.add_patient("Synthetic".into(), None, now())?)?;
            for text in ["first", "second", "third"] {
                c.record_data_reviewed(p, text.into(), now())?;
            }
            Ok(p)
        })
        .unwrap();
    let state = round(&phone, &config, &token).await;
    assert_eq!(state.status, "offline");
    assert_eq!(state.queued_events, 3);
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    settle(&phone, &config, &token).await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM events WHERE patient_id=$1")
        .bind(patient)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 3);
    assert_eq!(
        phone
            .handle
            .execute(false, move |c| Ok(c
                .note_view(patient)?
                .data_reviewed
                .len()))
            .unwrap(),
        3
    );
}

#[tokio::test]
async fn token_refresh_and_retry_after_are_distinct_from_offline() {
    use axum::{
        Json,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::get,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    let org = Uuid::now_v7();
    let clinician = Uuid::now_v7();
    struct Refresh {
        calls: Arc<AtomicUsize>,
    }
    impl AccessTokenProvider for Refresh {
        fn access_token(&self, refresh: bool) -> er_device::Result<String> {
            if refresh {
                self.calls.fetch_add(1, Ordering::SeqCst);
            }
            Ok(if refresh { "fresh" } else { "expired" }.into())
        }
    }
    let app = axum::Router::new()
        .route(
            "/v1/me",
            get(move |headers: HeaderMap| async move {
                if headers.get("authorization").and_then(|h| h.to_str().ok())
                    != Some("Bearer fresh")
                {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                Json(serde_json::json!({"org_id":org,"clinician_id":clinician})).into_response()
            }),
        )
        .route(
            "/v1/shifts/active",
            get(|| async {
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    [("retry-after", "120")],
                    Json(serde_json::json!({"error":"rate_limited"})),
                )
            }),
        );
    let origin = spawn_server(app).await;
    let phone = Phone::open();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Arc::new(Refresh {
        calls: calls.clone(),
    });
    let config = SyncConfig {
        origin,
        organization: org,
        clinician,
    };
    let handle = phone.handle.clone();
    let state = tokio::task::spawn_blocking(move || handle.synchronize(config, provider))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(state.status, "rate_limited");
    assert!(state.retry_at_ms.unwrap() >= now() + 119_000);
}

#[tokio::test]
async fn slow_network_does_not_block_local_commands_or_close() {
    use axum::{Json, routing::get};
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let org = Uuid::now_v7();
    let clinician = Uuid::now_v7();
    let signal = started.clone();
    let wait = release.clone();
    let origin = spawn_server(axum::Router::new().route(
        "/v1/me",
        get(move || {
            let signal = signal.clone();
            let wait = wait.clone();
            async move {
                signal.notify_one();
                wait.notified().await;
                Json(serde_json::json!({"org_id":org,"clinician_id":clinician}))
            }
        }),
    ))
    .await;
    let phone = Phone::open();
    let h = phone.handle.clone();
    let task = tokio::task::spawn_blocking(move || {
        h.synchronize(
            SyncConfig {
                origin,
                organization: org,
                clinician,
            },
            Arc::new(Token("synthetic".into())),
        )
    });
    started.notified().await;
    phone
        .handle
        .execute(true, |c| c.start_shift(now()))
        .unwrap();
    phone.handle.close().unwrap();
    release.notify_one();
    assert!(matches!(
        task.await.unwrap(),
        Err(er_device::DeviceError::Closed)
    ));
}

#[sqlx::test(migrations = "./migrations")]
async fn lost_shift_start_reply_recovers_the_existing_server_shift(pool: PgPool) {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        middleware::{self, Next},
        response::IntoResponse,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    let first = Arc::new(AtomicBool::new(true));
    let app = er_server::routes::router(test_state(pool.clone()).await).layer(middleware::from_fn(
        move |request: Request<Body>, next: Next| {
            let first = first.clone();
            async move {
                let lose = request.uri().path() == "/v1/shifts"
                    && request.method() == "POST"
                    && first.swap(false, Ordering::SeqCst);
                let response = next.run(request).await;
                if lose {
                    assert_eq!(response.status(), StatusCode::OK);
                    StatusCode::SERVICE_UNAVAILABLE.into_response()
                } else {
                    response
                }
            }
        },
    ));
    seed_org_and_clinician(&pool, ORG_A, "lost-start").await;
    let origin = spawn_server(app).await;
    let clinician: Uuid = sqlx::query_scalar("SELECT id FROM clinicians WHERE org_id=$1")
        .bind(ORG_A)
        .fetch_one(&pool)
        .await
        .unwrap();
    let config = SyncConfig {
        origin,
        organization: ORG_A,
        clinician,
    };
    let token = mint("lost-start");
    let phone = Phone::open();
    phone
        .handle
        .execute(true, |c| c.start_shift(now()))
        .unwrap();
    assert_eq!(round(&phone, &config, &token).await.status, "offline");
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    settle(&phone, &config, &token).await;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM shifts WHERE clinician_id=$1")
        .bind(clinician)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn offline_capture_audit_follows_events_and_survives_second_phone_import(pool: PgPool) {
    struct SyntheticCapture;
    impl er_device::CaptureAdapter for SyntheticCapture {
        fn available(&self) -> bool {
            true
        }
        fn start(&mut self, _: Uuid, _: Uuid, _: bool) -> er_device::Result<()> {
            Ok(())
        }
        fn stop(&mut self) -> er_device::Result<()> {
            Ok(())
        }
    }
    seed_org_and_clinician(&pool, ORG_A, "capture-sync").await;
    let origin = spawn_server(er_server::routes::router(test_state(pool.clone()).await)).await;
    let clinician: Uuid = sqlx::query_scalar("SELECT id FROM clinicians WHERE org_id=$1")
        .bind(ORG_A)
        .fetch_one(&pool)
        .await
        .unwrap();
    let config = SyncConfig {
        origin,
        organization: ORG_A,
        clinician,
    };
    let token = mint("capture-sync");
    let phone = Phone::open();
    phone.handle.close().unwrap();
    let device_config = DeviceConfig {
        database_path: phone.path.to_str().unwrap().into(),
        account_scope: "synthetic-clinician".into(),
    };
    let setup_config = device_config.clone();
    let patient = tokio::task::spawn_blocking(move || {
        let mut c =
            er_device::DeviceCore::open_with_capture(setup_config, Box::new(SyntheticCapture))
                .unwrap();
        c.start_shift(now()).unwrap();
        let patient = er_device::core::id(
            &c.add_patient("Synthetic capture".into(), Some("4".into()), now())
                .unwrap(),
        )
        .unwrap();
        c.select_patient(Some(patient), now()).unwrap();
        c.set_consent(
            patient,
            er_device::ConsentStatus::Granted,
            "synthetic-v1".into(),
            0,
            now(),
        )
        .unwrap();
        c.start_capture(patient, now()).unwrap();
        c.pause(patient, now(), "waiting".into()).unwrap();
        c.start_capture(patient, now()).unwrap();
        c.pause(patient, now(), "done".into()).unwrap();
        drop(c);

        patient
    })
    .await
    .unwrap();
    let source = Phone {
        handle: Arc::new(DeviceHandle::open(device_config).unwrap()),
        path: phone.path.clone(),
    };
    settle(&source, &config, &token).await;
    source.handle.close().unwrap();
    let destination = Phone::open();
    settle(&destination, &config, &token).await;
    let note = destination
        .handle
        .execute(false, move |c| c.note_view(patient))
        .unwrap();
    assert_eq!(note.encounter_count, 2);
    assert!(note.hpi.is_none());
    assert_eq!(
        destination
            .handle
            .execute(false, |c| Ok(c.view().patients[0].consent.status.clone()))
            .unwrap(),
        er_device::ConsentStatus::Granted
    );
    assert_eq!(
        destination
            .handle
            .execute(false, |c| Ok(c.view().patients[0].recording.status.clone()))
            .unwrap(),
        er_device::RecorderStatus::Paused
    );
    let api = er_api::client::ServerApi::new(&config.origin, &token);
    let audit = api.capture_audit(patient).await.unwrap();
    assert_eq!(audit.audit.segments.len(), 2);
    let mut changed = audit.audit.clone();
    changed.consents[0].policy_version = "rewrite".into();
    assert!(matches!(
        api.save_capture_audit(
            patient,
            &er_api::CaptureAuditWrite {
                expected_version: audit.version,
                audit: changed
            }
        )
        .await,
        Err(er_store::StoreError::Remote { status: 409, .. })
    ));
}
