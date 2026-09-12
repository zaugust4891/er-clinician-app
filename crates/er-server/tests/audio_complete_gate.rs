//! Step 6 S4 — completion, enqueue, and the ledger read.
//!
//! Where `audio_upload_gate.rs` stops at the presigned URL, these tests cross the
//! data plane: they `PUT` real bytes through that URL into MinIO, then prove what
//! the completion endpoint owns — that the server believes object storage rather
//! than the client, that exactly one job reaches the queue, and that the status
//! read is a ledger projection that never carries clinical text.
//!
//! Requires the compose stack (`docker compose up -d`) and the dev AWS_*
//! variables, the same standing requirement Postgres already imposes. Each test
//! gets its own bucket and queue pair (`common::ensure_test_audio_infra`).

use sqlx::PgPool;
use uuid::Uuid;

use er_server::{repository::PostgresRepository, routes};

mod common;
use common::{
    AudioFixture, ORG_A, ORG_B, ensure_test_audio_infra, get_json, mint, post_empty, post_json,
    seed_org_and_clinician, seed_registered_patient, test_state_with_audio, tiny_wav,
};

fn intent_url(patient_id: Uuid, encounter_id: Uuid) -> String {
    format!("/v1/patients/{patient_id}/encounters/{encounter_id}/audio")
}

fn complete_url(chunk_id: Uuid) -> String {
    format!("/v1/audio/{chunk_id}/complete")
}

fn status_url(chunk_id: Uuid) -> String {
    format!("/v1/audio/{chunk_id}")
}

fn intent_body(chunk_id: Uuid, chunk_index: u32, is_final: bool) -> serde_json::Value {
    serde_json::json!({
        "chunk_id": chunk_id,
        "chunk_index": chunk_index,
        "is_final": is_final,
        "content_type": "audio/wav",
    })
}

async fn chunk_status_in_db(pool: &PgPool, chunk_id: Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM audio_chunks WHERE id = $1")
        .bind(chunk_id)
        .fetch_one(pool)
        .await
        .expect("the ledger row must exist")
}

/// Ask for an upload slot the way a device does, and hand back the valet key.
async fn request_intent(
    app: &axum::Router,
    token: &str,
    patient_id: Uuid,
    encounter_id: Uuid,
    chunk_id: Uuid,
    chunk_index: u32,
    is_final: bool,
) -> er_api::AudioUploadResponse {
    let (status, json) = post_json(
        app,
        &intent_url(patient_id, encounter_id),
        token,
        intent_body(chunk_id, chunk_index, is_final),
    )
    .await;
    assert_eq!(status, 200, "intent failed: {json}");
    serde_json::from_value(json).expect("an AudioUploadResponse")
}

/// The data plane, exercised exactly as a device would: no Authorization header
/// (the URL is the authorization) and a content-type that must match the one
/// signed into the URL byte-for-byte.
async fn put_bytes(upload_url: &str, body: Vec<u8>) {
    let response = reqwest::Client::new()
        .put(upload_url)
        .header(reqwest::header::CONTENT_TYPE, "audio/wav")
        .body(body)
        .send()
        .await
        .expect("PUT to object storage");
    assert!(
        response.status().is_success(),
        "presigned PUT rejected: {}",
        response.status()
    );
}

#[sqlx::test]
async fn complete_after_put_marks_uploaded_and_enqueues_one_job(pool: PgPool) {
    let audio: AudioFixture = ensure_test_audio_infra().await;
    seed_org_and_clinician(&pool, ORG_A, "doc-complete-a").await;
    let patient_id = seed_registered_patient(&pool, ORG_A).await;
    let encounter_id = Uuid::now_v7();
    let chunk_id = Uuid::now_v7();
    let token = mint("doc-complete-a");
    let app = routes::router(test_state_with_audio(pool.clone(), &audio).await);

    let intent = request_intent(&app, &token, patient_id, encounter_id, chunk_id, 0, true).await;
    put_bytes(
        &intent.upload_url,
        tiny_wav(b"substernal chest pressure since 0600"),
    )
    .await;

    let (status, body) = post_empty(&app, &complete_url(chunk_id), &token).await;

    assert_eq!(status, 200, "completion failed: {body}");
    let view: er_api::ChunkView = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(view.chunk_id, chunk_id);
    assert_eq!(view.encounter_id, encounter_id);
    assert_eq!(view.chunk_index, 0);
    assert!(view.is_final);
    assert_eq!(view.status, er_api::chunk_status::UPLOADED);
    assert!(view.duration_ms.is_none(), "no worker has run yet");
    assert!(view.error.is_none());

    // The projection drops three columns on purpose (§C1/§C5): tenancy, the raw
    // object key, and any per-chunk transcript.
    for server_only in ["org_id", "s3_key", "transcript"] {
        assert!(
            body.get(server_only).is_none(),
            "{server_only} must never reach the wire"
        );
    }

    assert_eq!(chunk_status_in_db(&pool, chunk_id).await, "uploaded");

    let jobs = audio.drain_jobs().await;
    assert_eq!(jobs.len(), 1, "exactly one transcription job");
    assert_eq!(
        jobs[0],
        serde_json::json!({ "kind": "TranscribeChunk", "chunk_id": chunk_id }),
        "the worker decodes this message, so its shape is a contract"
    );
}

#[sqlx::test]
async fn completion_without_bytes_is_409_and_enqueues_nothing(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    seed_org_and_clinician(&pool, ORG_A, "doc-complete-a").await;
    let patient_id = seed_registered_patient(&pool, ORG_A).await;
    let token = mint("doc-complete-a");
    let app = routes::router(test_state_with_audio(pool.clone(), &audio).await);

    // Claimed, never uploaded — the bug this endpoint exists to catch.
    let missing = Uuid::now_v7();
    request_intent(&app, &token, patient_id, Uuid::now_v7(), missing, 0, true).await;
    let (missing_status, missing_body) = post_empty(&app, &complete_url(missing), &token).await;

    // Uploaded, but the connection died before any bytes arrived.
    let empty = Uuid::now_v7();
    let empty_intent =
        request_intent(&app, &token, patient_id, Uuid::now_v7(), empty, 0, true).await;
    put_bytes(&empty_intent.upload_url, Vec::new()).await;
    let (empty_status, empty_body) = post_empty(&app, &complete_url(empty), &token).await;

    assert_eq!(missing_status, 409);
    assert_eq!(missing_body["error"], "chunk_not_uploaded");
    assert_eq!(empty_status, 409);
    assert_eq!(empty_body["error"], "chunk_empty");

    assert_eq!(chunk_status_in_db(&pool, missing).await, "pending");
    assert_eq!(chunk_status_in_db(&pool, empty).await, "pending");
    assert!(
        audio.drain_jobs().await.is_empty(),
        "a rejected completion must not put work on the queue"
    );
}

#[sqlx::test]
async fn repeated_completion_is_idempotent_and_finished_chunks_are_not_requeued(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    seed_org_and_clinician(&pool, ORG_A, "doc-complete-a").await;
    let patient_id = seed_registered_patient(&pool, ORG_A).await;
    let chunk_id = Uuid::now_v7();
    let token = mint("doc-complete-a");
    let app = routes::router(test_state_with_audio(pool.clone(), &audio).await);

    let intent = request_intent(&app, &token, patient_id, Uuid::now_v7(), chunk_id, 0, true).await;
    put_bytes(&intent.upload_url, tiny_wav(b"repeat delivery")).await;

    let (first, _) = post_empty(&app, &complete_url(chunk_id), &token).await;
    let (second, second_body) = post_empty(&app, &complete_url(chunk_id), &token).await;

    assert_eq!(first, 200);
    assert_eq!(second, 200, "a retried completion is not an error");
    assert_eq!(second_body["status"], er_api::chunk_status::UPLOADED);
    assert_eq!(
        audio.drain_jobs().await.len(),
        2,
        "at-least-once: a retry may re-send, and §C2 makes the worker shrug"
    );

    // Now let the worker's half of the ledger run its course.
    let repo = PostgresRepository::for_org(pool.clone(), ORG_A);
    repo.mark_chunk_transcribing(chunk_id).await.unwrap();
    repo.store_chunk_transcript(chunk_id, "repeat delivery", 4_200)
        .await
        .unwrap();

    let (late, late_body) = post_empty(&app, &complete_url(chunk_id), &token).await;

    assert_eq!(late, 200);
    assert_eq!(
        late_body["status"],
        er_api::chunk_status::DONE,
        "a late completion must not drag a finished chunk backwards"
    );
    assert_eq!(late_body["duration_ms"], 4_200);
    assert!(
        audio.drain_jobs().await.is_empty(),
        "work that is already done is not re-queued"
    );
}

#[sqlx::test]
async fn oversized_or_mistyped_audio_is_not_marked_uploaded_or_enqueued(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    seed_org_and_clinician(&pool, ORG_A, "s1-audio").await;
    let patient_id = seed_registered_patient(&pool, ORG_A).await;
    let token = mint("s1-audio");
    let mut state = test_state_with_audio(pool.clone(), &audio).await;
    state.config.runtime.max_audio_bytes = 64;
    let app = routes::router(state);
    for (size, content_type, code) in [
        (65, "audio/wav", "chunk_too_large"),
        (44, "audio/ogg", "chunk_content_type_mismatch"),
    ] {
        let chunk_id = Uuid::now_v7();
        request_intent(&app, &token, patient_id, Uuid::now_v7(), chunk_id, 0, true).await;
        let key: String = sqlx::query_scalar("SELECT s3_key FROM audio_chunks WHERE id=$1")
            .bind(chunk_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        // SDK fixture mutation models stale/replaced object metadata; the client
        // cannot change the content type signed into its presigned PUT URL.
        audio
            .s3
            .put_object()
            .bucket(&audio.config.bucket)
            .key(key)
            .content_type(content_type)
            .body(vec![0_u8; size].into())
            .send()
            .await
            .unwrap();
        let (status, body) = post_empty(&app, &complete_url(chunk_id), &token).await;
        assert_eq!(status, 409);
        assert_eq!(body["error"], code);
        assert_eq!(chunk_status_in_db(&pool, chunk_id).await, "pending");
    }
    assert!(audio.drain_jobs().await.is_empty());
}

#[sqlx::test]
async fn status_read_tracks_the_ledger_without_exposing_transcripts(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    seed_org_and_clinician(&pool, ORG_A, "doc-complete-a").await;
    let patient_id = seed_registered_patient(&pool, ORG_A).await;
    let encounter_id = Uuid::now_v7();
    let happy = Uuid::now_v7();
    let poison = Uuid::now_v7();
    let token = mint("doc-complete-a");
    let app = routes::router(test_state_with_audio(pool.clone(), &audio).await);
    let repo = PostgresRepository::for_org(pool.clone(), ORG_A);

    let intent = request_intent(&app, &token, patient_id, encounter_id, happy, 0, false).await;
    let (pending_status, pending) = get_json(&app, &status_url(happy), &token).await;
    assert_eq!(pending_status, 200);
    assert_eq!(pending["status"], er_api::chunk_status::PENDING);
    assert!(pending["duration_ms"].is_null());
    assert!(pending["error"].is_null());

    put_bytes(
        &intent.upload_url,
        tiny_wav(b"the chest pain started at six"),
    )
    .await;
    post_empty(&app, &complete_url(happy), &token).await;
    let (_, uploaded) = get_json(&app, &status_url(happy), &token).await;
    assert_eq!(uploaded["status"], er_api::chunk_status::UPLOADED);

    repo.mark_chunk_transcribing(happy).await.unwrap();
    let (_, transcribing) = get_json(&app, &status_url(happy), &token).await;
    assert_eq!(transcribing["status"], er_api::chunk_status::TRANSCRIBING);
    assert!(
        !er_api::chunk_status::is_terminal(transcribing["status"].as_str().unwrap()),
        "the CLI's poll loop keeps going while a worker holds the lease"
    );

    repo.store_chunk_transcript(happy, "The chest pain started at six.", 42_680)
        .await
        .unwrap();
    let (_, done) = get_json(&app, &status_url(happy), &token).await;
    assert_eq!(done["status"], er_api::chunk_status::DONE);
    assert_eq!(done["duration_ms"], 42_680);
    assert!(done["error"].is_null());
    assert!(
        done.get("transcript").is_none(),
        "clinical text reaches the device as a TranscriptReady event, never through this read"
    );
    assert!(
        er_api::chunk_status::is_terminal(done["status"].as_str().unwrap()),
        "the poll loop stops here"
    );

    // The unhappy path is the same shape, which is the point.
    request_intent(&app, &token, patient_id, encounter_id, poison, 1, true).await;
    repo.mark_chunk_uploaded(poison).await.unwrap();
    repo.mark_chunk_failed(poison, "audio unavailable")
        .await
        .unwrap();
    let (_, failed) = get_json(&app, &status_url(poison), &token).await;
    assert_eq!(failed["status"], er_api::chunk_status::FAILED);
    assert_eq!(failed["error"], "audio unavailable");
    assert!(failed["duration_ms"].is_null());

    let (unknown_status, unknown_body) = get_json(&app, &status_url(Uuid::now_v7()), &token).await;
    assert_eq!(unknown_status, 404);
    assert_eq!(unknown_body["error"], "not_found");
}

#[sqlx::test]
async fn completion_and_status_are_org_scoped(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    seed_org_and_clinician(&pool, ORG_A, "doc-complete-a").await;
    seed_org_and_clinician(&pool, ORG_B, "doc-complete-b").await;
    let patient_id = seed_registered_patient(&pool, ORG_A).await;
    let chunk_id = Uuid::now_v7();
    let owner = mint("doc-complete-a");
    let stranger = mint("doc-complete-b");
    let app = routes::router(test_state_with_audio(pool.clone(), &audio).await);

    let intent = request_intent(&app, &owner, patient_id, Uuid::now_v7(), chunk_id, 0, true).await;
    put_bytes(&intent.upload_url, tiny_wav(b"another org's encounter")).await;

    let (foreign_complete, foreign_complete_body) =
        post_empty(&app, &complete_url(chunk_id), &stranger).await;
    let (foreign_status, foreign_status_body) =
        get_json(&app, &status_url(chunk_id), &stranger).await;

    // Registered-elsewhere and never-existed are indistinguishable: no oracle.
    assert_eq!(foreign_complete, 404);
    assert_eq!(foreign_complete_body["error"], "not_found");
    assert_eq!(foreign_status, 404);
    assert_eq!(foreign_status_body["error"], "not_found");
    assert_eq!(
        chunk_status_in_db(&pool, chunk_id).await,
        "pending",
        "a cross-tenant call changes nothing"
    );
    assert!(
        audio.drain_jobs().await.is_empty(),
        "a cross-tenant call enqueues nothing"
    );

    let (owner_status, _) = post_empty(&app, &complete_url(chunk_id), &owner).await;
    assert_eq!(owner_status, 200, "the owning org is unaffected");
    assert_eq!(audio.drain_jobs().await.len(), 1);
}
