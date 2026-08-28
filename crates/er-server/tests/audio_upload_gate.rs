//! Step 6 S3 — authenticated upload-intent minting.
//!
//! These tests stop before object upload (S4 owns `HeadObject` + enqueue). They
//! prove the part S3 owns: authorization attaches a tenant to a PHI-minimal key,
//! the pending ledger row is durable, retries are idempotent, and the returned
//! URL is a narrowly-scoped 15-minute PUT credential.

use sqlx::PgPool;
use uuid::Uuid;

use er_server::routes;

mod common;
use common::{
    ORG_A, ORG_B, mint, post_json, seed_org_and_clinician, seed_registered_patient, test_state,
};

fn intent_url(patient_id: Uuid, encounter_id: Uuid) -> String {
    format!("/v1/patients/{patient_id}/encounters/{encounter_id}/audio")
}

fn intent_body(
    chunk_id: Uuid,
    chunk_index: u32,
    is_final: bool,
    content_type: &str,
) -> serde_json::Value {
    serde_json::json!({
        "chunk_id": chunk_id,
        "chunk_index": chunk_index,
        "is_final": is_final,
        "content_type": content_type,
    })
}

async fn chunk_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audio_chunks")
        .fetch_one(pool)
        .await
        .expect("count audio chunks")
}

#[sqlx::test]
async fn valid_intent_persists_pending_row_and_returns_scoped_put_url(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-audio-a").await;
    let patient_id = seed_registered_patient(&pool, ORG_A).await;
    let encounter_id = Uuid::now_v7();
    let chunk_id = Uuid::now_v7();
    let app = routes::router(test_state(pool.clone()).await);

    let (status, json) = post_json(
        &app,
        &intent_url(patient_id, encounter_id),
        &mint("doc-audio-a"),
        intent_body(chunk_id, 0, true, "audio/wav"),
    )
    .await;

    assert_eq!(status, 200);
    let response: er_api::AudioUploadResponse = serde_json::from_value(json).unwrap();
    assert_eq!(response.chunk_id, chunk_id);
    assert_eq!(response.expires_in_secs, 900);

    let url = reqwest::Url::parse(&response.upload_url).expect("valid presigned URL");
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.host_str(), Some("localhost"));
    assert_eq!(url.port(), Some(9000));
    assert_eq!(
        url.path(),
        format!("/er-audio-test/{ORG_A}/{patient_id}/{encounter_id}/0-{chunk_id}")
    );

    let query = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };
    assert_eq!(query("X-Amz-Expires").as_deref(), Some("900"));
    assert!(
        query("X-Amz-SignedHeaders")
            .is_some_and(|headers| headers.split(';').any(|header| header == "content-type")),
        "the declared MIME type must be part of the signature"
    );

    let stored: (Uuid, Uuid, Uuid, i32, bool, String, String, String) = sqlx::query_as(
        "SELECT org_id, patient_id, encounter_id, chunk_index, is_final, content_type, s3_key, status \
           FROM audio_chunks WHERE id = $1",
    )
    .bind(chunk_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored.0, ORG_A);
    assert_eq!(stored.1, patient_id);
    assert_eq!(stored.2, encounter_id);
    assert_eq!(stored.3, 0);
    assert!(stored.4);
    assert_eq!(stored.5, "audio/wav");
    assert_eq!(
        stored.6,
        format!("{ORG_A}/{patient_id}/{encounter_id}/0-{chunk_id}")
    );
    assert_eq!(stored.7, er_api::chunk_status::PENDING);
}

#[sqlx::test]
async fn retry_is_idempotent_and_chunk_contract_conflicts_are_409(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-audio-a").await;
    let patient_id = seed_registered_patient(&pool, ORG_A).await;
    let encounter_id = Uuid::now_v7();
    let token = mint("doc-audio-a");
    let app = routes::router(test_state(pool.clone()).await);
    let first_chunk = Uuid::now_v7();
    let url = intent_url(patient_id, encounter_id);
    let first_body = intent_body(first_chunk, 0, false, "audio/wav");

    let (first_status, first_response) = post_json(&app, &url, &token, first_body.clone()).await;
    let (retry_status, retry_response) = post_json(&app, &url, &token, first_body).await;
    assert_eq!(first_status, 200);
    assert_eq!(retry_status, 200);
    assert_eq!(first_response["chunk_id"], retry_response["chunk_id"]);
    assert_eq!(chunk_count(&pool).await, 1, "a retry must not add a row");

    let (_, index_conflict) = post_json(
        &app,
        &url,
        &token,
        intent_body(Uuid::now_v7(), 0, false, "audio/wav"),
    )
    .await;
    assert_eq!(index_conflict["error"], "chunk_index_taken");

    let final_chunk = Uuid::now_v7();
    let (final_status, _) = post_json(
        &app,
        &url,
        &token,
        intent_body(final_chunk, 1, true, "audio/wav"),
    )
    .await;
    assert_eq!(final_status, 200);

    let (second_final_status, second_final) = post_json(
        &app,
        &url,
        &token,
        intent_body(Uuid::now_v7(), 2, true, "audio/wav"),
    )
    .await;
    assert_eq!(second_final_status, 409);
    assert_eq!(second_final["error"], "final_chunk_already_set");

    let other_encounter = Uuid::now_v7();
    let (mismatch_status, mismatch) = post_json(
        &app,
        &intent_url(patient_id, other_encounter),
        &token,
        intent_body(first_chunk, 0, false, "audio/wav"),
    )
    .await;
    assert_eq!(mismatch_status, 409);
    assert_eq!(mismatch["error"], "chunk_id_mismatch");
    assert_eq!(chunk_count(&pool).await, 2);
}

#[sqlx::test]
async fn mime_and_integer_boundaries_are_rejected_before_insert(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-audio-a").await;
    let patient_id = seed_registered_patient(&pool, ORG_A).await;
    let token = mint("doc-audio-a");
    let app = routes::router(test_state(pool.clone()).await);

    for content_type in ["audio/wav", "audio/ogg", "audio/mp4", "audio/mpeg"] {
        let (status, _) = post_json(
            &app,
            &intent_url(patient_id, Uuid::now_v7()),
            &token,
            intent_body(Uuid::now_v7(), 0, true, content_type),
        )
        .await;
        assert_eq!(status, 200, "{content_type} must stay supported");
    }

    for content_type in [
        "application/octet-stream",
        "audio/wav; charset=binary",
        "Audio/wav",
        " audio/wav",
    ] {
        let (status, body) = post_json(
            &app,
            &intent_url(patient_id, Uuid::now_v7()),
            &token,
            intent_body(Uuid::now_v7(), 0, true, content_type),
        )
        .await;
        assert_eq!(status, 400, "{content_type:?} must be rejected");
        assert_eq!(body["error"], "content_type_unsupported");
    }

    let (status, body) = post_json(
        &app,
        &intent_url(patient_id, Uuid::now_v7()),
        &token,
        intent_body(Uuid::now_v7(), u32::MAX, true, "audio/wav"),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(body["error"], "chunk_index_too_large");
    assert_eq!(
        chunk_count(&pool).await,
        4,
        "only the four valid MIME types land"
    );
}

#[sqlx::test]
async fn ownership_guard_hides_missing_and_other_org_patients(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-audio-a").await;
    seed_org_and_clinician(&pool, ORG_B, "doc-audio-b").await;
    let patient_b = seed_registered_patient(&pool, ORG_B).await;
    let encounter_id = Uuid::now_v7();
    let app = routes::router(test_state(pool.clone()).await);

    let (foreign_status, foreign_body) = post_json(
        &app,
        &intent_url(patient_b, encounter_id),
        &mint("doc-audio-a"),
        intent_body(Uuid::now_v7(), 0, true, "audio/wav"),
    )
    .await;
    let (missing_status, missing_body) = post_json(
        &app,
        &intent_url(Uuid::now_v7(), encounter_id),
        &mint("doc-audio-a"),
        intent_body(Uuid::now_v7(), 0, true, "audio/wav"),
    )
    .await;

    assert_eq!(foreign_status, 404);
    assert_eq!(foreign_body["error"], "not_found");
    assert_eq!(missing_status, 404);
    assert_eq!(missing_body["error"], "not_found");
    assert_eq!(
        chunk_count(&pool).await,
        0,
        "the guard runs before ledger insertion"
    );

    let (owner_status, _) = post_json(
        &app,
        &intent_url(patient_b, encounter_id),
        &mint("doc-audio-b"),
        intent_body(Uuid::now_v7(), 0, true, "audio/wav"),
    )
    .await;
    assert_eq!(owner_status, 200);
    assert_eq!(chunk_count(&pool).await, 1);
}
