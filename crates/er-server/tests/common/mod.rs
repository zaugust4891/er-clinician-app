//! Shared fixtures for the HTTP-level integration tests (`auth_gate`, `sync_gate`).
//!
//! Extracted from `auth_gate.rs` once S4 needed the same mint/seed/request helpers.
//! Integration tests are separate crates, so this is a `mod common;` include rather
//! than a library — hence the `#![allow(dead_code)]`: each test binary compiles the
//! whole module but uses only part of it, and unused-warnings here are noise, not signal.
#![allow(dead_code)]

use std::{path::Path, sync::Arc};

use axum::{
    body::Body,
    http::{header, Request},
};
use sqlx::PgPool;
use tower::ServiceExt; // brings `.oneshot`
use uuid::Uuid;

use er_server::{auth::keys::KeySet, config::DEV_ISSUER, repository::PostgresRepository, state::AppState};

pub const ORG_A: Uuid = Uuid::from_u128(0x0A);
pub const ORG_B: Uuid = Uuid::from_u128(0x0B);

// ---------------------------------------------------------------------------
// State + tokens
// ---------------------------------------------------------------------------

pub async fn test_state(pool: PgPool) -> AppState {
    // cwd for integration tests is the crate dir (crates/er-server), so the repo-root
    // dev keys are two levels up.
    let keys = Arc::new(KeySet::from_file(Path::new("../../dev/keys/jwks.json")).expect("dev jwks"));
    AppState::for_tests(pool, keys)
}

/// Sign a valid dev access token for `sub`, using the same private key and kid
/// the dev JWKS trusts.
pub fn mint(sub: &str) -> String {
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some("dev-key-1".to_string()); // must match jwks.json's kid

    let pem = std::fs::read("../../dev/keys/dev_private.pem").expect("dev private key");
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(&pem).expect("valid rsa pem");

    let claims = serde_json::json!({
        "sub": sub,
        "iss": DEV_ISSUER, // must match dev-mode issuer()
        "token_use": "access",
        "exp": jsonwebtoken::get_current_timestamp() + 3600, // 1h in future
    });

    jsonwebtoken::encode(&header, &claims, &key).expect("sign dev jwt")
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

pub async fn get(app: &axum::Router, path: &str, token: Option<&str>) -> u16 {
    let mut req = Request::builder().method("GET").uri(path);
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let response = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    response.status().as_u16()
}

/// Like `get`, but also hands back the parsed JSON body — needed when the status
/// code alone can't distinguish two outcomes (200-with-a-note vs 200-with-nothing).
pub async fn get_json(app: &axum::Router, path: &str, token: &str) -> (u16, serde_json::Value) {
    let req = Request::builder()
        .method("GET")
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    let response = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// POST a JSON body and hand back status + parsed response. `Json<T>` rejects a
/// request without `content-type: application/json` with a 415 before the handler
/// ever runs, so the header is not optional decoration.
pub async fn post_json(
    app: &axum::Router,
    path: &str,
    token: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

// ---------------------------------------------------------------------------
// Seeding
// ---------------------------------------------------------------------------

/// Insert an org and one clinician (cognito_sub = `sub`) in it. Order matters:
/// clinicians.org_id REFERENCES orgs(id), so the org row must exist first.
pub async fn seed_org_and_clinician(pool: &PgPool, org_id: Uuid, sub: &str) {
    sqlx::query("INSERT INTO orgs (id, name) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(org_id)
        .bind("Test Org")
        .execute(pool)
        .await
        .expect("seed org");

    sqlx::query(
        "INSERT INTO clinicians (id, org_id, cognito_sub, display_name) \
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
    )
    .bind(Uuid::now_v7()) // clinician id - random per row
    .bind(org_id)
    .bind(sub)
    .bind("Dr. Cusamano")
    .execute(pool)
    .await
    .expect("seed clinician");
}

/// Register a patient in `org_id` through the same org-scoped path production uses,
/// and hand back its id. Since S3 every per-patient route is gated on `patients`
/// having a row — seeding an event stream alone now yields a 404, correctly.
///
/// `created_by` is read back from `clinicians` rather than invented: the column is
/// `REFERENCES clinicians(id)`, so `seed_org_and_clinician` must have run for this org first.
pub async fn seed_registered_patient(pool: &PgPool, org_id: Uuid) -> Uuid {
    let patient_id = Uuid::now_v7();
    let created_by: Uuid = sqlx::query_scalar("SELECT id FROM clinicians WHERE org_id = $1 LIMIT 1")
        .bind(org_id)
        .fetch_one(pool)
        .await
        .expect("seed_org_and_clinician must run before seeding a patient");

    PostgresRepository::for_org(pool.clone(), org_id)
        .register_patient(patient_id, created_by, "chest pain", None)
        .await
        .expect("register patient");

    patient_id
}

/// Open an active shift for the org's clinician by writing the row directly —
/// there is no shift endpoint until S8, but `register_patient`'s attach half is
/// live now and needs something to attach to.
pub async fn seed_active_shift(pool: &PgPool, org_id: Uuid) -> Uuid {
    let shift_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO shifts (id, org_id, clinician_id, status) \
         SELECT $1, $2, c.id, 'active' FROM clinicians c WHERE c.org_id = $2 LIMIT 1",
    )
    .bind(shift_id)
    .bind(org_id)
    .execute(pool)
    .await
    .expect("seed active shift");
    shift_id
}

// ---------------------------------------------------------------------------
// Assertions against raw tables
// ---------------------------------------------------------------------------

/// How many `patients` rows carry this id, across every org. Deliberately NOT
/// org-scoped: the idempotency claim is about the table, and an org-scoped count
/// would hide a second row written under a different tenant.
pub async fn patient_row_count(pool: &PgPool, patient_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM patients WHERE id = $1")
        .bind(patient_id)
        .fetch_one(pool)
        .await
        .expect("count patients")
}

/// Same reasoning as `patient_row_count`: count across all orgs so a row written
/// under the wrong tenant still shows up.
pub async fn event_row_count(pool: &PgPool, patient_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE patient_id = $1")
        .bind(patient_id)
        .fetch_one(pool)
        .await
        .expect("count events")
}
