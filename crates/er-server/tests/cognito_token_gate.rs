//! S7 — the access-token contract through the real router in Cognito mode.
//!
//! The router runs with `AuthConfig::Cognito` for a synthetic pool and client.
//! Tokens are signed in-test with the dev key standing in for the pool's key, so
//! token-negative cases vary one property of an accepted token. Authentication
//! rejections are bodiless 401s; tenant isolation uses indistinguishable 404s.

use sqlx::PgPool;
use tower::ServiceExt; // brings `.oneshot`
use uuid::Uuid;

use er_domain::{CaptureMode, NoteEvent, PatientNote};
use er_server::config::{AuthConfig, DEV_ISSUER};
use er_server::repository::PostgresRepository;
use er_server::routes;
use er_store::repository::Repository; // .append_event is a trait method

mod common;
use common::{
    ORG_A, ORG_B, get, get_json, post_empty, post_json, put_json, seed_org_and_clinician,
    seed_registered_patient, test_state,
};

const POOL_ID: &str = "us-east-1_Synthetic";
const CLIENT_ID: &str = "synthetic-smoke-client";
const KID: &str = "dev-key-1";

fn cognito() -> AuthConfig {
    AuthConfig::Cognito {
        region: "us-east-1".into(),
        pool_id: POOL_ID.into(),
        client_id: CLIENT_ID.into(),
    }
}

async fn cognito_router(pool: PgPool) -> axum::Router {
    // `test_state` trusts the dev JWKS; only the issuer/client contract changes.
    let mut state = test_state(pool).await;
    state.config.auth_mode = cognito();
    routes::router(state)
}

fn sign(claims: serde_json::Value, key: &jsonwebtoken::EncodingKey) -> String {
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some(KID.to_string());
    jsonwebtoken::encode(&header, &claims, key).expect("sign test jwt")
}

fn pool_key() -> jsonwebtoken::EncodingKey {
    let pem = std::fs::read("../../dev/keys/dev_private.pem").expect("dev private key");
    jsonwebtoken::EncodingKey::from_rsa_pem(&pem).expect("valid rsa pem")
}

/// The claims of an accepted Cognito access token for `sub`.
fn access_claims(sub: &str) -> serde_json::Value {
    serde_json::json!({
        "sub": sub,
        "iss": cognito().issuer(),
        "token_use": "access",
        "client_id": CLIENT_ID,
        "scope": "aws.cognito.signin.user.admin",
        "exp": jsonwebtoken::get_current_timestamp() + 600,
    })
}

fn access_token(sub: &str) -> String {
    sign(access_claims(sub), &pool_key())
}

/// `access_claims` with one field replaced (or removed for `Null`).
fn variant(sub: &str, field: &str, value: serde_json::Value) -> String {
    let mut claims = access_claims(sub);
    if value.is_null() {
        claims.as_object_mut().unwrap().remove(field);
    } else {
        claims[field] = value;
    }
    sign(claims, &pool_key())
}

/// A different pool signs with its own key, here under a colliding kid.
fn other_pool_key() -> jsonwebtoken::EncodingKey {
    use rsa::pkcs1::EncodeRsaPrivateKey;
    let private = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let der = private.to_pkcs1_der().unwrap();
    jsonwebtoken::EncodingKey::from_rsa_der(der.as_bytes())
}

/// Append a short encounter for `patient` in org B and store its snapshot
/// through the API as the owner. Returns the note and its checkpoint.
async fn seed_note_and_snapshot(
    pool: &PgPool,
    app: &axum::Router,
    patient: Uuid,
    owner_token: &str,
) -> (PatientNote, u64) {
    let repo = PostgresRepository::for_org(pool.clone(), ORG_B);
    let encounter = Uuid::now_v7();
    let events = vec![
        NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient),
        NoteEvent::transcript_ready(
            patient,
            encounter,
            "synthetic chest pressure".to_string(),
            chrono::Utc::now(),
        ),
        NoteEvent::audio_stopped(patient, encounter),
    ];
    let mut checkpoint = 0;
    for event in &events {
        checkpoint = repo
            .append_event(patient, Uuid::now_v7(), event.clone())
            .await
            .expect("seed event");
    }
    let note = er_domain::replay::replay(patient, &events);
    let body = serde_json::json!({ "sequence_id": checkpoint, "snapshot": note });
    let path = format!("/v1/patients/{patient}/snapshot");
    let (status, _) = put_json(app, &path, owner_token, body).await;
    assert_eq!(status, 200, "owner stores the snapshot");
    (note, checkpoint)
}

#[sqlx::test]
async fn cognito_contract_access_tokens_reach_me_with_distinct_orgs(pool: PgPool) {
    let (a, b) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    seed_org_and_clinician(&pool, ORG_A, &a).await;
    seed_org_and_clinician(&pool, ORG_B, &b).await;
    let app = cognito_router(pool).await;

    let (status, me_a) = get_json(&app, "/v1/me", &access_token(&a)).await;
    assert_eq!(status, 200);
    let (status, me_b) = get_json(&app, "/v1/me", &access_token(&b)).await;
    assert_eq!(status, 200);
    assert_eq!(me_a["org_id"], ORG_A.to_string());
    assert_eq!(me_b["org_id"], ORG_B.to_string());
    assert_ne!(me_a["clinician_id"], me_b["clinician_id"]);
}

#[sqlx::test]
async fn every_negative_token_case_is_401(pool: PgPool) {
    let sub = Uuid::new_v4().to_string();
    seed_org_and_clinician(&pool, ORG_A, &sub).await;
    let app = cognito_router(pool).await;
    let now = jsonwebtoken::get_current_timestamp();
    let other_pool_issuer = AuthConfig::Cognito {
        region: "us-east-1".into(),
        pool_id: "us-east-1_OtherPool".into(),
        client_id: CLIENT_ID.into(),
    }
    .issuer();

    // The positive control: only the varied property separates these from 200.
    assert_eq!(get(&app, "/v1/me", Some(&access_token(&sub))).await, 200);

    let expired = variant(&sub, "exp", (now - 3600).into());
    let wrong_issuer = variant(&sub, "iss", other_pool_issuer.into());
    let wrong_key = sign(access_claims(&sub), &other_pool_key());
    let wrong_client = variant(&sub, "client_id", "another-client".into());
    let no_client = variant(&sub, "client_id", serde_json::Value::Null);
    let id_claims = serde_json::json!({
        "sub": sub,
        "iss": cognito().issuer(),
        "token_use": "id",
        "aud": CLIENT_ID,
        "exp": now + 600,
    });
    let id_token = sign(id_claims, &pool_key());
    let no_token_use = variant(&sub, "token_use", serde_json::Value::Null);
    let dev_token = variant(&sub, "iss", DEV_ISSUER.into());
    let unknown_sub = access_token(&Uuid::new_v4().to_string());
    let cases = [
        ("expired", expired),
        ("wrong pool issuer", wrong_issuer),
        ("wrong pool key", wrong_key),
        ("wrong client", wrong_client),
        ("no client", no_client),
        ("id token", id_token),
        ("no token_use", no_token_use),
        ("dev token", dev_token),
        ("unknown subject", unknown_sub),
        ("garbage", "not-a-jwt".to_string()),
    ];
    for (case, token) in &cases {
        assert_eq!(get(&app, "/v1/me", Some(token)).await, 401, "{case}");
    }

    // Missing or malformed bearer header.
    assert_eq!(get(&app, "/v1/me", None).await, 401, "missing bearer");
    let basic = axum::http::Request::builder()
        .uri("/v1/me")
        .header("authorization", format!("Basic {}", access_token(&sub)))
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.clone().oneshot(basic).await.unwrap();
    assert_eq!(response.status(), 401, "non-bearer scheme");
}

/// Cross-org reads through Cognito tokens are the same 404 as a never-existed
/// id, for both the patient-scoped routes and globally keyed job/audio lookups.
#[sqlx::test]
async fn cross_org_access_is_indistinguishable_from_absence(pool: PgPool) {
    let (a, b) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    seed_org_and_clinician(&pool, ORG_A, &a).await;
    seed_org_and_clinician(&pool, ORG_B, &b).await;
    let patient_b = seed_registered_patient(&pool, ORG_B).await;
    let app = cognito_router(pool.clone()).await;
    let (token_a, token_b) = (access_token(&a), access_token(&b));

    // Org B's patient has real events and a stored snapshot, so every read
    // below has data to leak, and its owner gets 200 from each.
    let (note_b, checkpoint) = seed_note_and_snapshot(&pool, &app, patient_b, &token_b).await;
    let paths = [
        "/v1/patients/{id}/note",
        "/v1/patients/{id}/events?since=0",
        "/v1/patients/{id}/snapshot",
        "/v1/patients/{id}/note/export",
    ];
    for path in paths {
        let their_path = path.replace("{id}", &patient_b.to_string());
        assert_eq!(get(&app, &their_path, Some(&token_b)).await, 200, "{path}");
    }

    let never = Uuid::now_v7();
    for path in paths {
        let (their_path, absent_path) = (
            path.replace("{id}", &patient_b.to_string()),
            path.replace("{id}", &never.to_string()),
        );
        let theirs = get(&app, &their_path, Some(&token_a)).await;
        let absent = get(&app, &absent_path, Some(&token_a)).await;
        assert_eq!(theirs, 404, "{path} for another org's patient");
        assert_eq!(theirs, absent, "{path} must not reveal existence");
    }

    // A cross-org write is the same 404 and leaves org B's snapshot untouched.
    let forged = serde_json::json!({ "sequence_id": checkpoint + 100, "snapshot": note_b });
    let snapshot = |id: Uuid| format!("/v1/patients/{id}/snapshot");
    let (theirs, _) = put_json(&app, &snapshot(patient_b), &token_a, forged.clone()).await;
    let (absent, _) = put_json(&app, &snapshot(never), &token_a, forged).await;
    assert_eq!((theirs, absent), (404, 404), "cross-org snapshot write");
    let (status, body) = get_json(&app, &snapshot(patient_b), &token_b).await;
    assert_eq!(status, 200);
    assert_eq!(body["sequence_id"].as_u64(), Some(checkpoint));

    let job_b = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO jobs (id, org_id, clinician_id, patient_id, kind)
         SELECT $1, org_id, id, $2, 'mdm' FROM clinicians WHERE cognito_sub = $3",
    )
    .bind(job_b)
    .bind(patient_b)
    .bind(&b)
    .execute(&pool)
    .await
    .expect("seed org B job");
    let job = |id: Uuid| format!("/v1/jobs/{id}");
    assert_eq!(get(&app, &job(job_b), Some(&token_b)).await, 200);
    assert_eq!(get(&app, &job(job_b), Some(&token_a)).await, 404);
    assert_eq!(get(&app, &job(never), Some(&token_a)).await, 404);
    let audio = format!("/v1/audio/{never}");
    assert_eq!(get(&app, &audio, Some(&token_a)).await, 404);
}

/// Existing audio and valid write bodies make these real isolation checks:
/// validation errors or nonexistent fixtures cannot stand in for authorization.
#[sqlx::test]
async fn cognito_cross_org_writes_and_audio_preserve_owner_data(pool: PgPool) {
    let (a, b) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
    seed_org_and_clinician(&pool, ORG_A, &a).await;
    seed_org_and_clinician(&pool, ORG_B, &b).await;
    let patient_b = seed_registered_patient(&pool, ORG_B).await;
    let app = cognito_router(pool.clone()).await;
    let (token_a, token_b) = (access_token(&a), access_token(&b));
    let encounter = Uuid::now_v7();
    let chunk = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO audio_chunks
         (id, org_id, patient_id, encounter_id, chunk_index, is_final, content_type, s3_key)
         VALUES ($1, $2, $3, $4, 0, true, 'audio/wav', 'synthetic-audio')",
    )
    .bind(chunk)
    .bind(ORG_B)
    .bind(patient_b)
    .bind(encounter)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        get(&app, &format!("/v1/audio/{chunk}"), Some(&token_b)).await,
        200
    );

    let absent = Uuid::now_v7();
    let owner_audio = get_json(&app, &format!("/v1/audio/{chunk}"), &token_a).await;
    let absent_audio = get_json(&app, &format!("/v1/audio/{absent}"), &token_a).await;
    assert_eq!(owner_audio.0, 404);
    assert_eq!(
        owner_audio, absent_audio,
        "audio status must not reveal existence"
    );
    let complete = post_empty(&app, &format!("/v1/audio/{chunk}/complete"), &token_a).await;
    let missing = post_empty(&app, &format!("/v1/audio/{absent}/complete"), &token_a).await;
    assert_eq!(complete.0, 404);
    assert_eq!(
        complete, missing,
        "completion must not reach object storage"
    );

    for operation in ["events", "audio", "mdm"] {
        let mut responses = Vec::new();
        for patient in [patient_b, absent] {
            let (path, body) = match operation {
                "events" => (
                    format!("/v1/patients/{patient}/events"),
                    serde_json::json!({"events": [{
                        "event_id": Uuid::now_v7(),
                        "event": NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient),
                    }]}),
                ),
                "audio" => (
                    format!("/v1/patients/{patient}/encounters/{encounter}/audio"),
                    serde_json::json!({
                        "chunk_id": Uuid::now_v7(), "chunk_index": 0,
                        "is_final": true, "content_type": "audio/wav",
                    }),
                ),
                _ => (
                    format!("/v1/patients/{patient}/mdm"),
                    serde_json::json!({"job_id": Uuid::now_v7()}),
                ),
            };
            responses.push(post_json(&app, &path, &token_a, body).await);
        }
        assert_eq!(responses[0].0, 404, "{operation}");
        assert_eq!(responses[0], responses[1], "{operation} existence oracle");
    }

    let (status, shift) = post_empty(&app, "/v1/shifts", &token_b).await;
    assert_eq!(status, 200);
    let shift_id = shift["shift_id"].as_str().unwrap();
    let foreign = get_json(&app, &format!("/v1/shifts/{shift_id}"), &token_a).await;
    let missing = get_json(&app, &format!("/v1/shifts/{absent}"), &token_a).await;
    assert_eq!(foreign.0, 404);
    assert_eq!(foreign, missing);
    let foreign = post_empty(&app, &format!("/v1/shifts/{shift_id}/end"), &token_a).await;
    let missing = post_empty(&app, &format!("/v1/shifts/{absent}/end"), &token_a).await;
    assert_eq!(foreign.0, 404);
    assert_eq!(foreign, missing);
    let (status, still_active) = get_json(&app, "/v1/shifts/active", &token_b).await;
    assert_eq!(status, 200);
    assert_eq!(still_active["shift_id"], shift_id);

    let (chunks, events, jobs, chunk_state): (i64, i64, i64, String) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM audio_chunks), (SELECT count(*) FROM events),
                (SELECT count(*) FROM jobs), (SELECT status FROM audio_chunks WHERE id = $1)",
    )
    .bind(chunk)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (chunks, events, jobs),
        (1, 0, 0),
        "denied writes changed no data"
    );
    assert_eq!(chunk_state, "pending");
}

#[sqlx::test]
async fn offboarding_denies_unexpired_token_after_api_replacement(pool: PgPool) {
    let sub = Uuid::new_v4().to_string();
    seed_org_and_clinician(&pool, ORG_A, &sub).await;
    let app = cognito_router(pool.clone()).await;
    let token = access_token(&sub);
    assert_eq!(get(&app, "/v1/me", Some(&token)).await, 200);
    er_server::onboarding::revoke(
        &pool,
        &[er_server::onboarding::Revocation {
            org_id: ORG_A,
            cognito_sub: sub,
        }],
    )
    .await
    .unwrap();

    // This is the documented limitation: a running task retains its cached grant.
    assert_eq!(get(&app, "/v1/me", Some(&token)).await, 200);
    let replacement = cognito_router(pool).await;
    assert_eq!(get(&replacement, "/v1/me", Some(&token)).await, 401);
}
