//! S6 — snapshot sync: `PUT` / `GET /v1/patients/{id}/snapshot`.
//!
//! Split out from `sync_gate.rs` (which had already grown past 600 lines) because
//! the snapshot pair is a distinct endpoint with its own integrity story: a
//! server-computed checksum, a deliberate 404-means-absent contract, and a
//! self-healing fallback to full replay. Fixtures come from `common/`.

use sqlx::PgPool;
use uuid::Uuid;

use er_domain::{CaptureMode, NoteEvent, NotePhase, PatientNote};
use er_server::repository::PostgresRepository;
use er_server::routes;
use er_store::repository::Repository;

mod common;
use common::{
    get, get_json, mint, put_json, seed_org_and_clinician, seed_registered_patient, test_state,
    ORG_A, ORG_B,
};

fn snapshot_url(patient: Uuid) -> String {
    format!("/v1/patients/{patient}/snapshot")
}

/// Write a short encounter and return the note it replays to, plus the sequence_id
/// that note is a checkpoint *of* — the two halves of a `SnapshotUp`.
async fn seed_note_and_checkpoint(
    pool: &PgPool,
    org: Uuid,
    patient: Uuid,
) -> (PatientNote, u64) {
    let repo = PostgresRepository::for_org(pool.clone(), org);
    let encounter = Uuid::now_v7();
    let events = vec![
        NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient),
        NoteEvent::transcript_ready(
            patient,
            encounter,
            "substernal chest pressure since 0600".to_string(),
            chrono::Utc::now(),
        ),
        NoteEvent::audio_stopped(patient, encounter),
    ];

    let mut last = 0;
    for e in &events {
        last = repo
            .append_event(patient, Uuid::now_v7(), e.clone())
            .await
            .expect("seed event");
    }

    (er_domain::replay::replay(patient, &events), last)
}

fn snapshot_body(sequence_id: u64, note: &PatientNote) -> serde_json::Value {
    serde_json::json!({ "sequence_id": sequence_id, "snapshot": note })
}

// ---------------------------------------------------------------------------
// Round trip
// ---------------------------------------------------------------------------

/// The core contract. Unlike replay-derived comparisons (S5), a snapshot is a
/// transferred *blob* — nothing is re-derived — so the note must come back
/// byte-identical, `id` and `audit_log` ids included. Anything less means the
/// serialization boundary is lossy.
#[sqlx::test]
async fn snapshot_put_then_get_round_trips(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let (note, checkpoint) = seed_note_and_checkpoint(&pool, ORG_A, patient).await;
    let app = routes::router(test_state(pool).await);
    let token = mint("doc-joey-a");

    let (put_status, _) = put_json(&app, &snapshot_url(patient), &token, snapshot_body(checkpoint, &note)).await;
    assert_eq!(put_status, 200);

    let (get_status, body) = get_json(&app, &snapshot_url(patient), &token).await;
    assert_eq!(get_status, 200);
    assert_eq!(body["sequence_id"].as_u64().unwrap(), checkpoint);
    assert_eq!(
        body["snapshot"],
        serde_json::to_value(&note).unwrap(),
        "the snapshot is a blob: it must round-trip exactly, not merely equivalently"
    );

    // And it must be *usable* on arrival: the `state` trait object is #[serde(skip)],
    // so a client deserializes then hydrates — the sequence S9 will perform.
    let mut received: PatientNote = serde_json::from_value(body["snapshot"].clone()).unwrap();
    assert!(received.state.is_none(), "state does not travel over the wire");
    received.hydrate();
    assert!(received.state.is_some(), "hydrate rebuilds behavior from the phase enum");
    assert_eq!(received.phase, NotePhase::Idle);
    assert_eq!(
        received.encounters[0].transcript.as_deref(),
        Some("substernal chest pressure since 0600")
    );
}

/// `save_snapshot` upserts on patient_id, so a client checkpointing repeatedly
/// through a shift must replace its snapshot, never accumulate them.
#[sqlx::test]
async fn repeated_put_replaces_rather_than_accumulates(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let (note, checkpoint) = seed_note_and_checkpoint(&pool, ORG_A, patient).await;
    let app = routes::router(test_state(pool.clone()).await);
    let token = mint("doc-joey-a");

    put_json(&app, &snapshot_url(patient), &token, snapshot_body(checkpoint, &note)).await;

    // A later checkpoint of the same patient, one event further along.
    let repo = PostgresRepository::for_org(pool.clone(), ORG_A);
    let later_event = NoteEvent::trigger_mdm(patient);
    let later_seq = repo
        .append_event(patient, Uuid::now_v7(), later_event.clone())
        .await
        .unwrap();
    let mut later_note = note.clone();
    er_domain::replay::apply_event(&mut later_note, &later_event);

    let (status, _) = put_json(&app, &snapshot_url(patient), &token, snapshot_body(later_seq, &later_note)).await;
    assert_eq!(status, 200);

    assert_eq!(repo.snapshot_count().await.unwrap(), 1, "one row per patient");
    let (_, body) = get_json(&app, &snapshot_url(patient), &token).await;
    assert_eq!(body["sequence_id"].as_u64().unwrap(), later_seq, "the newer checkpoint wins");
    assert_eq!(body["snapshot"]["phase"], "Finalizing");
}

// ---------------------------------------------------------------------------
// Absence and corruption both mean "replay instead"
// ---------------------------------------------------------------------------

/// A registered patient who has never checkpointed. 404 here is a normal answer,
/// not an error — it mirrors the local `Ok(None)` contract, and the client's
/// response is to replay from `since=0` rather than to fail.
#[sqlx::test]
async fn missing_snapshot_is_404_and_replay_still_works(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let (expected, _) = seed_note_and_checkpoint(&pool, ORG_A, patient).await;
    let app = routes::router(test_state(pool.clone()).await);

    assert_eq!(get(&app, &snapshot_url(patient), Some(&mint("doc-joey-a"))).await, 404);

    // The fallback path is the point: no snapshot must never mean no note.
    let repo = PostgresRepository::for_org(pool, ORG_A);
    let loaded = er_app::load::load_patient_note(&repo, patient)
        .await
        .unwrap()
        .expect("full replay reconstructs the note without any snapshot");
    assert_eq!(loaded.phase, expected.phase);
    assert_eq!(loaded.encounters.len(), expected.encounters.len());
}

/// The integrity guarantee. A tampered row must read as ABSENT rather than be
/// served as truth — the server would otherwise hand a client a note that no
/// event stream supports. The 404 then routes the client into full replay, so
/// corruption is self-healing rather than fatal.
#[sqlx::test]
async fn corrupted_snapshot_reads_as_absent_and_self_heals(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let (note, checkpoint) = seed_note_and_checkpoint(&pool, ORG_A, patient).await;
    let app = routes::router(test_state(pool.clone()).await);
    let token = mint("doc-joey-a");

    put_json(&app, &snapshot_url(patient), &token, snapshot_body(checkpoint, &note)).await;
    assert_eq!(get(&app, &snapshot_url(patient), Some(&token)).await, 200, "healthy first");

    // Tamper with the stored note but leave the checksum column alone — exactly
    // what silent storage corruption looks like.
    let mut tampered: serde_json::Value = serde_json::to_value(&note).unwrap();
    tampered["phase"] = serde_json::json!("Signed");
    sqlx::query("UPDATE snapshots SET snapshot = $1 WHERE patient_id = $2")
        .bind(serde_json::to_string(&tampered).unwrap())
        .bind(patient)
        .execute(&pool)
        .await
        .expect("tamper");

    assert_eq!(
        get(&app, &snapshot_url(patient), Some(&token)).await,
        404,
        "a checksum mismatch must read as absent, never be served"
    );

    // Self-healing: the events are untouched, so the note is still recoverable
    // and reports the phase the STREAM says, not the one the tampered row claimed.
    let repo = PostgresRepository::for_org(pool, ORG_A);
    let loaded = er_app::load::load_patient_note(&repo, patient)
        .await
        .unwrap()
        .expect("replay recovers the note");
    assert_eq!(loaded.phase, NotePhase::Idle, "not the forged Signed");
}

// ---------------------------------------------------------------------------
// Tenancy
// ---------------------------------------------------------------------------

/// Both halves of the pair sit behind the S3 ownership guard. The write side
/// matters most: `snapshots.patient_id` is a bare PRIMARY KEY, so an unguarded
/// upsert from another org would overwrite the row *and* its org_id.
#[sqlx::test]
async fn snapshot_endpoints_are_org_scoped(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    seed_org_and_clinician(&pool, ORG_B, "doc-peppas-b").await;
    let patient_b = seed_registered_patient(&pool, ORG_B).await;
    let (note_b, checkpoint) = seed_note_and_checkpoint(&pool, ORG_B, patient_b).await;
    let app = routes::router(test_state(pool.clone()).await);

    // Org B checkpoints its own patient.
    let (owner_put, _) = put_json(
        &app,
        &snapshot_url(patient_b),
        &mint("doc-peppas-b"),
        snapshot_body(checkpoint, &note_b),
    )
    .await;
    assert_eq!(owner_put, 200);

    // Org A may neither read it nor overwrite it.
    assert_eq!(
        get(&app, &snapshot_url(patient_b), Some(&mint("doc-joey-a"))).await,
        404,
        "org A cannot read org B's checkpoint"
    );

    let mut forged = note_b.clone();
    forged.phase = NotePhase::Signed;
    let (intruder_put, _) = put_json(
        &app,
        &snapshot_url(patient_b),
        &mint("doc-joey-a"),
        snapshot_body(checkpoint, &forged),
    )
    .await;
    assert_eq!(intruder_put, 404, "org A cannot overwrite org B's checkpoint");

    // And org B's snapshot is untouched by the attempt.
    let (status, body) = get_json(&app, &snapshot_url(patient_b), &mint("doc-peppas-b")).await;
    assert_eq!(status, 200);
    assert_eq!(body["snapshot"]["phase"], "Idle", "still org B's note, not the forgery");
}
