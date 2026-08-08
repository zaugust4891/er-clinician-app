use std::assert_eq;
use uuid::Uuid;

use er_server::{routes, state::AppState, auth::keys::KeySet};
use std::{sync::Arc, path::Path};
use sqlx::PgPool;

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

/// Like `get`, but also hands back the parsed JSON body — needed when the status
/// code alone can't distinguish two outcomes (200-with-a-note vs 200-with-nothing).
async fn get_json(app: &axum::Router, path: &str, token: &str) -> (u16, serde_json::Value) {
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
async fn post_json(
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

/// How many `patients` rows carry this id, across every org. Deliberately NOT
/// org-scoped: the idempotency claim is about the table, and an org-scoped count
/// would hide a second row written under a different tenant.
async fn patient_row_count(pool: &PgPool, patient_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM patients WHERE id = $1")
        .bind(patient_id)
        .fetch_one(pool)
        .await
        .expect("count patients")
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
        "exp": jsonwebtoken::get_current_timestamp() + 3600, // 1h in future
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

/// Register a patient in `org_id` through the same org-scoped path production uses,
/// and hand back its id. Since S3 every per-patient route is gated on `patients`
/// having a row — seeding an event stream alone now yields a 404, correctly.
///
/// `created_by` is read back from `clinicians` rather than invented: the column is
/// `REFERENCES clinicians(id)`, so `seed_org_and_clinician` must have run for this
/// org first (the same ordering requirement the notes above describe for events).
async fn seed_registered_patient(pool: &PgPool, org_id: Uuid) -> Uuid {
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
async fn seed_active_shift(pool: &PgPool, org_id: Uuid) -> Uuid {
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

async fn seed_patient_events(pool: &PgPool, org_id: Uuid) -> Uuid {
    let patient_id = seed_registered_patient(pool, org_id).await;
    let encounter_id = Uuid::now_v7();
    let repo = PostgresRepository::for_org(pool.clone(), org_id);

    // One event is enough to prove the handler replays a real stream rather than
    // handing back the fresh `Registered` note an event-less patient produces.
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
    assert_eq!(get(&app, "/v1/me", Some(&mint("test-doc-cusamano"))).await, 200);
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

/// S3's deliberate behavior change: "registered, but nothing has happened yet" is a
/// different truth from "no such patient", and only the second one hides behind 404.
/// Before the ownership guard existed, both collapsed into the empty-events 404.
#[sqlx::test]
async fn registered_patient_with_no_events_is_200_not_404(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await; // registered, zero events

    let app = routes::router(test_state(pool).await);
    let (status, body) = get_json(&app, &format!("/v1/patients/{patient}/note"), &mint("doc-joey-a")).await;

    assert_eq!(status, 200);
    assert_eq!(body["phase"], "Registered");
}

/// §C2's idempotency doctrine at the HTTP layer: the client stamps the id, so a
/// retry after a lost response MUST be a no-op rather than a duplicate or a 409.
/// The device cannot tell "my request never arrived" from "the reply never came
/// back", so the server has to make both safe to retry.
#[sqlx::test]
async fn registering_twice_is_idempotent(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let app = routes::router(test_state(pool.clone()).await);
    let token = mint("doc-joey-a");
    let patient_id = Uuid::now_v7();
    let body = serde_json::json!({
        "patient_id": patient_id,
        "chief_complaint": "chest pain",
        "display_label": "Room 4",
    });

    let (first, first_body) = post_json(&app, "/v1/patients", &token, body.clone()).await;
    let (second, second_body) = post_json(&app, "/v1/patients", &token, body).await;

    assert_eq!(first, 200, "first registration");
    assert_eq!(second, 200, "replay of the same registration");
    assert_eq!(first_body["patient_id"], second_body["patient_id"]);
    assert_eq!(patient_row_count(&pool, patient_id).await, 1, "exactly one row");
}

#[sqlx::test]
async fn empty_chief_complaint_is_400(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let app = routes::router(test_state(pool.clone()).await);
    let patient_id = Uuid::now_v7();

    // Whitespace, not "": the handler trims before checking, so this also proves
    // the guard isn't a naive is_empty() on the raw string.
    let (status, _) = post_json(
        &app,
        "/v1/patients",
        &mint("doc-joey-a"),
        serde_json::json!({ "patient_id": patient_id, "chief_complaint": "   " }),
    )
    .await;

    assert_eq!(status, 400);
    assert_eq!(patient_row_count(&pool, patient_id).await, 0, "rejected before insert");
}

/// The other half of the guard: registration is org-scoped, so a patient announced
/// by org B is not merely unreadable by org A — it is indistinguishable from a
/// patient that never existed. Same 404, no existence oracle.
#[sqlx::test]
async fn patient_registered_by_another_org_is_404(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    seed_org_and_clinician(&pool, ORG_B, "doc-peppas-b").await;
    let app = routes::router(test_state(pool).await);
    let patient_id = Uuid::now_v7();

    let (registered, _) = post_json(
        &app,
        "/v1/patients",
        &mint("doc-peppas-b"),
        serde_json::json!({ "patient_id": patient_id, "chief_complaint": "chest pain" }),
    )
    .await;
    assert_eq!(registered, 200);

    let note = format!("/v1/patients/{patient_id}/note");
    assert_eq!(get(&app, &note, Some(&mint("doc-joey-a"))).await, 404, "org A must not see it");
    assert_eq!(get(&app, &note, Some(&mint("doc-peppas-b"))).await, 200, "org B owns it");
}

/// The second half of `register_patient`: a patient announced while a shift is
/// open lands on that shift's list, and positions advance 0, 1, … — the durable
/// form of ShiftManager's stable-ordering invariant.
#[sqlx::test]
async fn registration_attaches_to_the_active_shift(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let shift_id = seed_active_shift(&pool, ORG_A).await;
    let app = routes::router(test_state(pool.clone()).await);
    let token = mint("doc-joey-a");

    let mut ids = Vec::new();
    for complaint in ["chest pain", "ankle injury"] {
        let patient_id = Uuid::now_v7();
        let (status, _) = post_json(
            &app,
            "/v1/patients",
            &token,
            serde_json::json!({ "patient_id": patient_id, "chief_complaint": complaint }),
        )
        .await;
        assert_eq!(status, 200);
        ids.push(patient_id);
    }

    let rows: Vec<(Uuid, i32)> = sqlx::query_as(
        "SELECT patient_id, position FROM shift_patients WHERE shift_id = $1 ORDER BY position",
    )
    .bind(shift_id)
    .fetch_all(&pool)
    .await
    .expect("read shift roster");

    assert_eq!(rows, vec![(ids[0], 0), (ids[1], 1)]);
}

/// Registration must still succeed with no shift open — the attach is a
/// best-effort SELECT that matches zero rows, not a precondition. A clinician
/// who hasn't started a shift can still create a patient.
#[sqlx::test]
async fn registration_without_an_active_shift_still_succeeds(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let app = routes::router(test_state(pool.clone()).await);
    let patient_id = Uuid::now_v7();

    let (status, _) = post_json(
        &app,
        "/v1/patients",
        &mint("doc-joey-a"),
        serde_json::json!({ "patient_id": patient_id, "chief_complaint": "chest pain" }),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(patient_row_count(&pool, patient_id).await, 1);
}