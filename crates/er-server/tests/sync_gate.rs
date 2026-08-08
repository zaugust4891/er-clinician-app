//! S4 — the write half of the sync protocol: `POST /v1/patients/{id}/events`.
//!
//! These are HTTP-level protocol tests. The storage-level guarantees (monotonic
//! sequence_ids, dedupe, org isolation) are the contract suite's job in
//! `repository_contract.rs`; what's tested here is the *endpoint's* behavior —
//! batch validation, the path/payload match check, the S3 ownership guard, and
//! end-to-end idempotency over the wire.

use sqlx::PgPool;
use uuid::Uuid;

use er_domain::{CaptureMode, NoteEvent};
use er_server::repository::PostgresRepository;
use er_server::routes;
use er_store::repository::Repository;

mod common;
use common::{
    event_row_count, mint, post_json, seed_org_and_clinician, seed_registered_patient, test_state,
    ORG_A, ORG_B,
};

/// One `EventUp` as JSON. Built by serializing a real `NoteEvent` rather than
/// hand-writing the tagged form, so these tests exercise the same serde shape a
/// device would send and can't drift from the enum's representation.
fn up(event_id: Uuid, event: &NoteEvent) -> serde_json::Value {
    serde_json::json!({ "event_id": event_id, "event": event })
}

fn events_url(patient_id: Uuid) -> String {
    format!("/v1/patients/{patient_id}/events")
}

/// A three-event encounter for `patient`, with caller-chosen ids so a retry can
/// send the byte-identical batch.
fn encounter_batch(patient: Uuid, ids: [Uuid; 3]) -> serde_json::Value {
    let encounter = Uuid::now_v7();
    serde_json::json!({
        "events": [
            up(ids[0], &NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient)),
            up(ids[1], &NoteEvent::transcript_ready(
                patient, encounter, "chest pain, worse on exertion".to_string(), chrono::Utc::now(),
            )),
            up(ids[2], &NoteEvent::audio_stopped(patient, encounter)),
        ]
    })
}

fn sequence_ids(body: &serde_json::Value) -> Vec<u64> {
    body["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| r["sequence_id"].as_u64().expect("sequence_id"))
        .collect()
}

// ---------------------------------------------------------------------------
// The headline guarantee (§C2): at-least-once delivery + idempotent storage
// ---------------------------------------------------------------------------

/// A device that never learns whether its POST landed must be able to resend
/// blindly. The second batch must return the SAME sequence_ids — not new ones,
/// not an error — and must not double-write the record.
#[sqlx::test]
async fn replayed_batch_is_idempotent(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let app = routes::router(test_state(pool.clone()).await);
    let token = mint("doc-joey-a");

    let ids = [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()];
    let batch = encounter_batch(patient, ids);

    let (first_status, first_body) = post_json(&app, &events_url(patient), &token, batch.clone()).await;
    let (second_status, second_body) = post_json(&app, &events_url(patient), &token, batch).await;

    assert_eq!(first_status, 200);
    assert_eq!(second_status, 200, "a replay is success, not a conflict");

    let first_seqs = sequence_ids(&first_body);
    let second_seqs = sequence_ids(&second_body);
    assert_eq!(first_seqs.len(), 3);
    assert_eq!(first_seqs, second_seqs, "replay returns the ORIGINAL sequence_ids");
    assert!(first_seqs.windows(2).all(|w| w[0] < w[1]), "monotonic within the batch");
    assert_eq!(event_row_count(&pool, patient).await, 3, "3 rows, not 6");

    // The acks must also come back keyed to the ids the client sent, in order —
    // otherwise a client can't match an ack to the event it retired.
    let acked: Vec<Uuid> = first_body["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| serde_json::from_value(r["event_id"].clone()).unwrap())
        .collect();
    assert_eq!(acked, ids.to_vec());
}

// ---------------------------------------------------------------------------
// Cross-patient smuggling
// ---------------------------------------------------------------------------

/// The path says patient A; one event in the body claims patient B. Without this
/// check the ownership guard would have verified A while the write landed on B.
/// The rejection must also be total: nothing from the batch may be stored, including
/// the events *before* the bad one in the array.
#[sqlx::test]
async fn event_patient_mismatch_is_400_and_writes_nothing(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let other = seed_registered_patient(&pool, ORG_A).await; // same org — still forbidden
    let app = routes::router(test_state(pool.clone()).await);
    let encounter = Uuid::now_v7();

    let body = serde_json::json!({
        "events": [
            // A legitimate event FIRST, so passing this test requires validating the
            // whole batch up front rather than inserting as you go.
            up(Uuid::now_v7(), &NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient)),
            up(Uuid::now_v7(), &NoteEvent::audio_started(other, encounter, CaptureMode::Ambient)),
        ]
    });

    let (status, body) = post_json(&app, &events_url(patient), &mint("doc-joey-a"), body).await;

    assert_eq!(status, 400);
    assert_eq!(body["error"], "event_patient_mismatch");
    assert_eq!(event_row_count(&pool, patient).await, 0, "the valid event must not land either");
    assert_eq!(event_row_count(&pool, other).await, 0, "and certainly not the smuggled one");
}

// ---------------------------------------------------------------------------
// The S3 ownership guard, applied to writes
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn appending_to_an_unregistered_patient_is_404(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let app = routes::router(test_state(pool.clone()).await);
    let ghost = Uuid::now_v7(); // never registered

    let batch = encounter_batch(ghost, [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()]);
    let (status, _) = post_json(&app, &events_url(ghost), &mint("doc-joey-a"), batch).await;

    assert_eq!(status, 404);
    assert_eq!(event_row_count(&pool, ghost).await, 0);
}

/// Registration is org-scoped, so org A writing to org B's patient is the same
/// 404 as writing to a patient that never existed — no existence oracle on the
/// write path either.
#[sqlx::test]
async fn appending_to_another_orgs_patient_is_404(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    seed_org_and_clinician(&pool, ORG_B, "doc-peppas-b").await;
    let patient_b = seed_registered_patient(&pool, ORG_B).await;
    let app = routes::router(test_state(pool.clone()).await);

    let ids = [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()];
    let (status, _) = post_json(
        &app,
        &events_url(patient_b),
        &mint("doc-joey-a"), // org A's token
        encounter_batch(patient_b, ids),
    )
    .await;

    assert_eq!(status, 404);
    assert_eq!(event_row_count(&pool, patient_b).await, 0, "org A must not write into org B");

    // Sanity: the rightful owner can write the very same batch.
    let (owner_status, _) = post_json(
        &app,
        &events_url(patient_b),
        &mint("doc-peppas-b"),
        encounter_batch(patient_b, ids),
    )
    .await;
    assert_eq!(owner_status, 200);
    assert_eq!(event_row_count(&pool, patient_b).await, 3);
}

// ---------------------------------------------------------------------------
// Batch-shape validation
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn empty_batch_is_400(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let app = routes::router(test_state(pool).await);

    let (status, body) = post_json(
        &app,
        &events_url(patient),
        &mint("doc-joey-a"),
        serde_json::json!({ "events": [] }),
    )
    .await;

    assert_eq!(status, 400);
    assert_eq!(body["error"], "empty_batch");
}

#[sqlx::test]
async fn oversized_batch_is_400(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let app = routes::router(test_state(pool.clone()).await);
    let encounter = Uuid::now_v7();

    // MAX_EVENT_BATCH is 500; 501 small events stay well under the 2 MiB body cap,
    // so this exercises the count guard rather than DefaultBodyLimit's 413.
    let events: Vec<_> = (0..501)
        .map(|_| {
            up(
                Uuid::now_v7(),
                &NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient),
            )
        })
        .collect();

    let (status, body) = post_json(
        &app,
        &events_url(patient),
        &mint("doc-joey-a"),
        serde_json::json!({ "events": events }),
    )
    .await;

    assert_eq!(status, 400);
    assert_eq!(body["error"], "batch_too_large");
    assert_eq!(event_row_count(&pool, patient).await, 0, "rejected before any insert");
}

// ---------------------------------------------------------------------------
// §C5 — the per-patient advisory lock
// ---------------------------------------------------------------------------

/// `append_event` now takes `pg_advisory_xact_lock(hashtextextended(patient_id, 0))`
/// so appends for one patient are strictly serial and sequence order equals commit
/// order (§C5). That commit-order property can't be observed from outside the
/// database without contriving a pause inside a live transaction, so this test
/// covers what IS observable: under real contention on the same lock, every append
/// still lands exactly once, none deadlock, and the stream stays totally ordered.
///
/// A lock taken on the wrong value, held across the wrong scope, or deadlocking
/// against the pool would surface here as a timeout, an error, or a lost row.
#[sqlx::test]
async fn concurrent_appends_to_one_patient_all_land(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;

    const WRITERS: usize = 16;
    let mut handles = Vec::with_capacity(WRITERS);
    for _ in 0..WRITERS {
        let repo = PostgresRepository::for_org(pool.clone(), ORG_A);
        handles.push(tokio::spawn(async move {
            let encounter = Uuid::now_v7();
            repo.append_event(
                patient,
                Uuid::now_v7(),
                NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient),
            )
            .await
        }));
    }

    let mut seqs = Vec::with_capacity(WRITERS);
    for h in handles {
        seqs.push(h.await.expect("task panicked").expect("append under contention"));
    }

    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(seqs.len(), WRITERS, "every writer got a distinct sequence_id");
    assert_eq!(event_row_count(&pool, patient).await, WRITERS as i64, "no lost writes");

    // The stream reads back in sequence order with no duplicates — the ordering
    // guarantee downsync's cursor (S5) is about to depend on.
    let stored = PostgresRepository::for_org(pool.clone(), ORG_A)
        .get_events(patient)
        .await
        .unwrap();
    let read_seqs: Vec<u64> = stored.iter().map(|e| e.sequence_id).collect();
    assert!(read_seqs.windows(2).all(|w| w[0] < w[1]), "strictly increasing");
    assert_eq!(read_seqs, seqs, "reader sees exactly what the writers were promised");
}

// ---------------------------------------------------------------------------
// §C6 audit item #4 — the scoped duplicate lookup
// ---------------------------------------------------------------------------

/// `events.event_id` is globally UNIQUE, so an id already used by another org
/// makes the INSERT conflict and fall through to `sequence_for`. Before S4 that
/// lookup was unscoped: it would have found the OTHER org's row and cheerfully
/// returned its sequence_id — handing org B a valid-looking cursor into org A's
/// stream, and silently dropping org B's event. Scoped to (org, patient), the
/// same collision fails closed instead.
#[sqlx::test]
async fn event_id_colliding_across_orgs_fails_closed(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    seed_org_and_clinician(&pool, ORG_B, "doc-peppas-b").await;
    let patient_a = seed_registered_patient(&pool, ORG_A).await;
    let patient_b = seed_registered_patient(&pool, ORG_B).await;

    let shared_event_id = Uuid::now_v7(); // the collision
    let encounter = Uuid::now_v7();

    let seq_a = PostgresRepository::for_org(pool.clone(), ORG_A)
        .append_event(
            patient_a,
            shared_event_id,
            NoteEvent::audio_started(patient_a, encounter, CaptureMode::Ambient),
        )
        .await
        .expect("org A's own append succeeds");

    let result = PostgresRepository::for_org(pool.clone(), ORG_B)
        .append_event(
            patient_b,
            shared_event_id,
            NoteEvent::audio_started(patient_b, encounter, CaptureMode::Ambient),
        )
        .await;

    assert!(
        result.is_err(),
        "org B must not receive a sequence_id for a row it does not own (got {result:?})"
    );
    if let Ok(leaked) = result {
        assert_ne!(leaked, seq_a, "and certainly not org A's sequence_id");
    }
    assert_eq!(event_row_count(&pool, patient_b).await, 0, "nothing stored for org B");
}
