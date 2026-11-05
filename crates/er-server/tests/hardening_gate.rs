use axum::{body::Body, http::Request};
use er_server::{repository::PostgresRepository, routes};
use er_store::Repository;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;
mod common;
use common::*;

#[sqlx::test]
async fn every_route_rejects_missing_token_and_disabled_or_wrong_role(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "hardening-doc").await;
    let app = routes::router(test_state(pool.clone()).await);
    let token = mint("hardening-doc");
    assert_eq!(get(&app, "/v1/me", Some(&token)).await, 200);
    let id = Uuid::now_v7();
    let paths = [
        ("GET", "/v1/me".into()),
        ("GET", "/v1/stats".into()),
        ("POST", "/v1/patients".into()),
        ("POST", "/v1/shifts".into()),
        ("GET", "/v1/shifts/active".into()),
        ("GET", format!("/v1/shifts/{id}")),
        ("POST", format!("/v1/shifts/{id}/end")),
        ("GET", format!("/v1/jobs/{id}")),
        ("GET", format!("/v1/audio/{id}")),
        ("POST", format!("/v1/audio/{id}/complete")),
        ("GET", format!("/v1/patients/{id}/note")),
        ("GET", format!("/v1/patients/{id}/note/export")),
        ("POST", format!("/v1/patients/{id}/mdm")),
        ("GET", format!("/v1/patients/{id}/events")),
        ("POST", format!("/v1/patients/{id}/events")),
        ("GET", format!("/v1/patients/{id}/snapshot")),
        ("PUT", format!("/v1/patients/{id}/snapshot")),
        ("POST", format!("/v1/patients/{id}/encounters/{id}/audio")),
    ];
    for state in [
        "disabled_at = now()",
        "disabled_at = NULL, role = 'observer'",
    ] {
        sqlx::query(&format!("UPDATE clinicians SET {state}"))
            .execute(&pool)
            .await
            .unwrap();
        for (method, path) in &paths {
            for bearer in [None, Some(&token)] {
                let mut request = Request::builder().method(*method).uri(path);
                if let Some(token) = bearer {
                    request = request.header("authorization", format!("Bearer {token}"));
                }
                let response = app
                    .clone()
                    .oneshot(request.body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), 401, "{method} {path}");
            }
        }
    }
}

#[sqlx::test]
async fn http_limits_include_retry_after_and_reads_are_audited(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "quota-doc").await;
    let app = routes::router(test_state(pool.clone()).await);
    let token = mint("quota-doc");
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let path = format!("/v1/patients/{patient}/mdm");
    for _ in 0..5 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&path)
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(response.status(), 429);
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&path)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    assert_eq!(response.headers()["retry-after"], "60");
    assert_eq!(
        get(&app, &format!("/v1/patients/{patient}/note"), Some(&token)).await,
        200
    );
    let audit: (String, Option<Uuid>) =
        sqlx::query_as("SELECT route, resource_id FROM access_audit")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audit, ("/v1/patients/{id}/note".into(), Some(patient)));
    // Losing the audit destination must prevent the next clinical read.
    sqlx::query("DROP TABLE access_audit")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(&app, &format!("/v1/patients/{patient}/note"), Some(&token)).await,
        503
    );
}

#[sqlx::test]
async fn replay_evidence_is_stable_and_detects_lost_events(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "restore-doc").await;
    let repo = PostgresRepository::for_org(pool, ORG_A);
    let patient = Uuid::now_v7();
    assert!(er_server::recovery::evidence(&repo, patient).await.is_err());
    let encounter = Uuid::now_v7();
    repo.append_event(
        patient,
        Uuid::now_v7(),
        er_domain::NoteEvent::audio_started(patient, encounter, er_domain::CaptureMode::Ambient),
    )
    .await
    .unwrap();
    let before = er_server::recovery::evidence(&repo, patient).await.unwrap();
    assert_eq!(
        before,
        er_server::recovery::evidence(&repo, patient).await.unwrap()
    );
    repo.append_event(
        patient,
        Uuid::now_v7(),
        er_domain::NoteEvent::audio_stopped(patient, encounter),
    )
    .await
    .unwrap();
    assert_ne!(
        before,
        er_server::recovery::evidence(&repo, patient).await.unwrap()
    );
}
