use sqlx::PgPool;
use uuid::Uuid;

use er_server::repository::{InsertChunkOutcome, NewChunk, PostgresRepository};

async fn seed_org(pool: &PgPool) -> Uuid {
    let org_id = Uuid::now_v7();
    sqlx::query("INSERT INTO orgs (id, name) VALUES ($1, 'audio status test')")
        .bind(org_id)
        .execute(pool)
        .await
        .unwrap();
    org_id
}

async fn insert_pending_chunk(
    repo: &PostgresRepository,
    encounter_id: Uuid,
    chunk_index: i32,
) -> Uuid {
    let chunk_id = Uuid::now_v7();
    let outcome = repo
        .insert_chunk(NewChunk {
            id: chunk_id,
            patient_id: Uuid::now_v7(),
            encounter_id,
            chunk_index,
            is_final: false,
            content_type: "audio/wav",
            s3_key: "org/patient/encounter/chunk.wav",
        })
        .await
        .unwrap();

    assert!(matches!(outcome, InsertChunkOutcome::Created(_)));
    chunk_id
}

#[sqlx::test(migrations = "./migrations")]
async fn chunk_status_helpers_enforce_forward_only_transitions(pool: PgPool) {
    let org_id = seed_org(&pool).await;
    let other_org_id = seed_org(&pool).await;
    let repo = PostgresRepository::for_org(pool.clone(), org_id);
    let other_repo = PostgresRepository::for_org(pool, other_org_id);
    let chunk_id = insert_pending_chunk(&repo, Uuid::now_v7(), 0).await;

    let pending = repo
        .mark_chunk_transcribing(chunk_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        pending.status, "pending",
        "pending cannot skip the uploaded state"
    );

    let uploaded = repo.mark_chunk_uploaded(chunk_id).await.unwrap().unwrap();
    assert_eq!(uploaded.status, "uploaded");

    let uploaded_retry = repo.mark_chunk_uploaded(chunk_id).await.unwrap().unwrap();
    assert_eq!(uploaded_retry.status, "uploaded");

    let transcribing = repo
        .mark_chunk_transcribing(chunk_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(transcribing.status, "transcribing");

    let done = repo
        .store_chunk_transcript(chunk_id, "Patient reports chest pain.", 58_700)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.status, "done");
    assert_eq!(
        done.transcript.as_deref(),
        Some("Patient reports chest pain.")
    );
    assert_eq!(done.duration_ms, Some(58_700));
    assert!(done.error.is_none());

    let late_complete = repo.mark_chunk_uploaded(chunk_id).await.unwrap().unwrap();
    assert_eq!(late_complete.status, "done");
    assert_eq!(late_complete.transcript, done.transcript);

    let late_failure = repo
        .mark_chunk_failed(chunk_id, "late duplicate worker failure")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(late_failure.status, "done");
    assert!(late_failure.error.is_none());

    assert!(
        other_repo
            .mark_chunk_uploaded(chunk_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo.mark_chunk_uploaded(Uuid::now_v7())
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn failed_chunk_is_terminal(pool: PgPool) {
    let org_id = seed_org(&pool).await;
    let repo = PostgresRepository::for_org(pool, org_id);
    let chunk_id = insert_pending_chunk(&repo, Uuid::now_v7(), 0).await;

    repo.mark_chunk_uploaded(chunk_id).await.unwrap();
    repo.mark_chunk_transcribing(chunk_id).await.unwrap();

    let failed = repo
        .mark_chunk_failed(chunk_id, "decoder rejected synthetic WAV")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, "failed");
    assert_eq!(
        failed.error.as_deref(),
        Some("decoder rejected synthetic WAV")
    );

    let late_success = repo
        .store_chunk_transcript(chunk_id, "must not overwrite the failure", 1_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(late_success.status, "failed");
    assert!(late_success.transcript.is_none());
    assert_eq!(late_success.error, failed.error);
}

#[sqlx::test(migrations = "./migrations")]
async fn encounter_chunks_are_ordered_and_org_scoped(pool: PgPool) {
    let org_id = seed_org(&pool).await;
    let other_org_id = seed_org(&pool).await;
    let repo = PostgresRepository::for_org(pool.clone(), org_id);
    let other_repo = PostgresRepository::for_org(pool, other_org_id);
    let encounter_id = Uuid::now_v7();

    insert_pending_chunk(&repo, encounter_id, 2).await;
    insert_pending_chunk(&repo, encounter_id, 0).await;
    insert_pending_chunk(&repo, encounter_id, 1).await;
    insert_pending_chunk(&other_repo, encounter_id, 3).await;

    let rows = repo.encounter_chunks(encounter_id).await.unwrap();
    let indexes: Vec<i32> = rows.iter().map(|row| row.chunk_index).collect();
    assert_eq!(indexes, vec![0, 1, 2]);
    assert!(rows.iter().all(|row| row.org_id == org_id));

    let other_rows = other_repo.encounter_chunks(encounter_id).await.unwrap();
    assert_eq!(other_rows.len(), 1);
    assert_eq!(other_rows[0].chunk_index, 3);
    assert_eq!(other_rows[0].org_id, other_org_id);
}
