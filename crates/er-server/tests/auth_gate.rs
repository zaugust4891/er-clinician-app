use std::assert_eq;
use tracing_subscriber::fmt::format::json;
use uuid::Uuid;

use er_server::{routes, state::AppState, auth::keys::KeySet};
use std::{sync::Arc, path::Path};
use sqlx::{PgPool, pool::maybe::MaybePoolConnection::PoolConnection};

const ORG_A: Uuid = Uuid::from_u128(0x0A);
const ORG_B: Uuid = Uuid::from_u128(0x0B);

///Insert an org and one clinician(cognito = `sub`) in it. Order matters:
/// clinicians.org_id REFERENCES orgs(id), so the org row must exist first.
async fn seed_org_and_clinician(pool: &PgPool, org_id: Uuid, sub: &str ) {
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

async fn test_state(pool: PgPool) -> AppState {
    // cwd for integration tests is the crate dir (crates/er-server), so the repo-root
    // dev keys are two levels up. `concat!(env!("CARGO_MANIFEST_DIR"), ...)` would be
    // cwd-independent if you prefer.
    let keys = Arc::new(
        KeySet::from_file(Path::new("../../dev/keys/jwks.json")).expect("dev jwks"),
    );
    AppState::for_tests(pool, keys)
}

use tower::ServiceExt;                    // brings `.oneshot`
use axum::{body::Body, http::{Request, header}};

async fn get(app: &axum::Router, path: &str, token: Option<&str>) -> u16 {
    let mut req = Request::builder().method("GET").uri(path);
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let response = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    response.status().as_u16()
}

use er_server::config::DEV_ISSUER;

/// Sign a valid dev access token for `sub`, using the same private key and kid
/// the dev JWKS trusts. Mirrors the S3 unit-test signer.
fn mint(sub: &str) -> String {
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some("dev-key-1".to_string()); // must match jwks.json's kid

    let pem = std::fs::read("../../dev/keys/dev_private.pem").expect("dev private key");
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(&pem).expect("valid rsa pem");

    let claims = serde_json::json!({
        "sub": sub,
        "iss": DEV_ISSUER, // must match dev-mode issuer()
        "token_use": "access",
        "exp": &jsonwebtoken::get_current_timestamp() + 3600, // 1h in future
    });

    jsonwebtoken::encode(&header, &claims, &key).expect("sign dev jwt")
}

use er_server::repository::PostgresRepository;
use er_store::repository::Repository; // to use the .append_event() trait method
use er_domain::{NoteEvent, CaptureMode};


/*
NOTES on the following helper function seed_patient_events(): 
- Writes a minimal event stream for a brand-new patient in `org_id`, returning the patient id. 
  Uses the org-scoped repo so the events carry `org_id` — which is what the cross-tenant 404 relies on.

- A patient's event stream scoped to one org, and hand back that patient's id so the test can then request 
  /v1/patients/{id}/note as different clinicians and check tenant isolation.

- The key is that it writes through PostgresRepository::for_org(pool, org_id) — the same org-scoped path production uses. 
  That's what stamps org_id onto the events rows, which is exactly what makes the cross-tenant query come back empty later.

- Signature append_event(patient_id, event_id, event) (repository.rs:46). event_id is the row's identity 
  (dedup key for the ON CONFLICT (event_id) DO NOTHING), distinct from patient_id. 
  A fresh Uuid::now_v7() per call is right.

- Repository must be imported as a trait, even though we never name it — .append_event is a trait method,
  so the Repository trait has to be in scope for the call to resolve(use::er_server::repository::Repository).

- Why audio_started specifically: I picked the one event that both constructs trivially and replays cleanly from a fresh Registered note.
  replay.rs:30, AudioStarted drives the state machine to Encountering and pushes an encounter
  no dependency on prior events. 
  Most other events (MedicationRecorded, DataReviewed, DispositionSet, …) are guarded by can_ingest_results() 
  and would be silently dropped from a bare note, leaving the stream effectively empty.
  The test only asserts the status code, so a single audio_started giving a valid non-empty note is all you need. (The handler will return hpi: null, which is fine.)
  
- Ordering / FK requirement: the tenancy migration back-fills events.org_id → orgs(id) (migration line 20). 
  So the org row must exist before you append events into it. In test 3 that's already satisfied — 
  seed_org_and_clinician(&pool, ORG_B, "doc-b") runs before seed_patient_events(&pool, ORG_B). 
  If you ever call seed_patient_events for an org you didn't seed first, the insert fails the foreign key.
*/

async fn seed_patient_events(pool: &PgPool, org_id: Uuid) -> Uuid {
    let patient_id = Uuid::now_v7();
    let encounter_id = Uuid::now_v7();
    let repo = PostgresRepository::for_org(pool.clone(), org_id);

    // One event is enough: it makes `get_events` non-empty so the handler
    // replays a note and returns 200 (rather than the empty->404 path).
    repo.append_event(
        patient_id,
        Uuid::now_v7(), // event_id -- the row's unique key (ON CONFLICT DO NOTHING)
        NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient),
    )
    .await
    .expect("seed patient events");

    patient_id
}


#[sqlx::test]
async fn no_token_is_401_and_healthz_is_open(pool: PgPool) {
    let app = routes::router(test_state(pool).await);
    assert_eq!(get(&app, "/v1/me", None).await, 401);
    assert_eq!(get(&app, "/healthz", None).await, 200);
}

#[sqlx::test]
async fn dev_token_reaches_me(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "test-doc-cusamano").await;
    let app = routes::router(test_state(pool).await);
    assert_eq!(get(&app, "v1/me", Some(&mint("test-doc-cusamano"))).await, 200);
}

/// Key here: "belongs to another org" and "doesn't exist" are indistinguishable to the caller.
#[sqlx::test]
async fn cross_tenant_fails_closed_with_404(pool: PgPool) {
    // Seed TWO orgs, a clinician in each, and one patient's events in org B/
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    seed_org_and_clinician(&pool, ORG_B, "doc-peppas-b").await;
    let patient_b = seed_patient_events(&pool, ORG_B).await; // via for_org(pool, ORG_B)

    let app = routes::router(test_state(pool).await);
    // Org-A token asks for org-B's patient: 404, NOT 403 - existence is NOT confirmed.
    assert_eq!(get(&app, &format!("/v1/patients/{patient_b}/note"), Some(&mint("doc-joey-a"))).await, 404);
    // Sanity: the rightful owner sees it.
    assert_eq!(get(&app, &format!("/v1/patients/{patient_b}/note"), Some(&mint("doc-peppas-b"))).await, 200);
    // And a random never-existed id is the SAME 404 for doc-peppas-b - indistinguishability.
    assert_eq!(get(&app, &format!("/v1/patients/{}/note", Uuid::now_v7()), Some(&mint("doc-peppas-b"))).await, 404);
}