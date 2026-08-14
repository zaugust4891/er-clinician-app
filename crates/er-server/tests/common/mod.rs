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
use chrono::Utc;
use sqlx::PgPool;
use tower::ServiceExt; // brings `.oneshot`
use uuid::Uuid;

use er_domain::{
    CaptureMode, ClinicalResult, Diagnosis, Disposition, LabResult, Medication, MedicationKind,
    NoteEvent, NoteSections, ResultSource, ResultType,
};

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
// A live server (S9 — the client needs a real socket)
// ---------------------------------------------------------------------------

/// Serve `app` on a real TCP port for the rest of the test and return its base
/// URL. Every other helper here uses `.oneshot`, which calls the router directly
/// and never opens a socket; `RemoteRepository` speaks actual HTTP, so it needs
/// somewhere to connect.
///
/// `127.0.0.1:0` means "any free port" — the OS picks and `local_addr()` reports
/// which, so parallel tests never collide. `axum::serve` implements `IntoFuture`,
/// so it can be spawned as a background task; it dies with the test's runtime.
pub async fn spawn_server(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral port");
    let addr = listener.local_addr().expect("listener has a local address");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// A URL that is syntactically fine and answers nothing: bind a port, learn its
/// number, then release it. Connecting there is refused, which is how a test
/// distinguishes "no response at all" (`StoreError::Backend`) from "the server
/// said no" (`StoreError::Remote`).
pub async fn unreachable_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind an ephemeral port");
    let addr = listener.local_addr().expect("listener has a local address");
    drop(listener);
    format!("http://{addr}")
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

/// GET returning status, the `content-type` header, and the raw body. Needed for
/// endpoints whose body is not JSON — `note/export` serves flat text for a clipboard,
/// and the content-type is part of that contract, not incidental.
pub async fn get_text(app: &axum::Router, path: &str, token: &str) -> (u16, String, String) {
    let req = Request::builder()
        .method("GET")
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    let response = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, content_type, String::from_utf8_lossy(&bytes).into_owned())
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

/// POST with no request body — for endpoints whose every input comes from the
/// token and the path (`/v1/shifts`, `/v1/shifts/{id}/end`). Deliberately sends no
/// content-type: these handlers must not require one, since there is nothing to type.
pub async fn post_empty(app: &axum::Router, path: &str, token: &str) -> (u16, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

/// PUT a JSON body. Same content-type reasoning as `post_json`; the response body
/// is returned raw because some PUT handlers answer with a bare status and no JSON.
pub async fn put_json(
    app: &axum::Router,
    path: &str,
    token: &str,
    body: serde_json::Value,
) -> (u16, String) {
    let req = Request::builder()
        .method("PUT")
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

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

// ---------------------------------------------------------------------------
// A complete clinical visit
// ---------------------------------------------------------------------------

/// One ED visit driven all the way to `Signed`, with every section and every
/// objective-data list populated — the event stream, not the writing of it, so
/// callers can push it through whichever path they are testing (a repository
/// directly, or `EventBus` over HTTP).
///
/// Ordering follows the real pipeline: the MDM content is persisted while still
/// `Idle`, *then* TriggerMDM advances the phase (CLAUDE.md's "persist content
/// before advancing state").
pub fn full_visit_events(patient: Uuid, encounter: Uuid) -> Vec<NoteEvent> {
    vec![
        NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient),
        NoteEvent::transcript_ready(
            patient,
            encounter,
            "substernal chest pressure since 0600, radiating to the left arm".to_string(),
            Utc::now(),
        ),
        NoteEvent::audio_stopped(patient, encounter),
        NoteEvent::encounter_summarized(
            patient,
            encounter,
            NoteSections {
                hpi: Some("55yo with substernal chest pressure since 0600.".to_string()),
                ros: Some("Positive for chest pain and dyspnea. Negative for fever.".to_string()),
                exam: Some("Alert, diaphoretic. Lungs clear. Regular rate and rhythm.".to_string()),
                mdm: None,
                disposition_text: None,
            },
            vec![encounter],
        ),
        NoteEvent::result_received(
            patient,
            ClinicalResult {
                id: Uuid::now_v7(),
                patient_id: patient,
                source: ResultSource::Manual,
                timestamp: Utc::now(),
                result_type: ResultType::Lab(LabResult {
                    fhir_id: None,
                    loinc_code: Some("6598-7".to_string()),
                    display_name: "Troponin I".to_string(),
                    value: 0.42,
                    unit: "ng/mL".to_string(),
                    reference_range: Some((0.0, 0.04)),
                    is_abnormal: true,
                }),
            },
            Utc::now(),
        ),
        NoteEvent::medication_recorded(
            patient,
            Medication {
                id: Uuid::now_v7(),
                patient_id: patient,
                name: "Aspirin".to_string(),
                dose: "324 mg".to_string(),
                route: Some("PO".to_string()),
                rxnorm_code: None,
                kind: MedicationKind::AdministeredInEd,
                administered_at: Utc::now(),
                frequency: "once".to_string(),
            },
        ),
        NoteEvent::diagnosis_assigned(
            patient,
            Diagnosis {
                id: Uuid::now_v7(),
                patient_id: patient,
                text: "NSTEMI".to_string(),
                icd10_code: Some("I21.4".to_string()),
                is_primary: true,
            },
        ),
        NoteEvent::data_reviewed(patient, "Prior ECG from 2025 reviewed.".to_string()),
        NoteEvent::disposition_set(
            patient,
            Disposition::Admit {
                service: "Cardiology".to_string(),
                bed: Some("CCU 3".to_string()),
            },
        ),
        NoteEvent::mdm_generated(
            patient,
            "Elevated troponin with ischemic chest pain. NSTEMI. Admit to cardiology.".to_string(),
            vec![encounter],
        ),
        NoteEvent::trigger_mdm(patient),
        NoteEvent::review_ready(patient),
        NoteEvent::sign(patient),
    ]
}
