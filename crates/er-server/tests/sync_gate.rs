//! S4 — the write half of the sync protocol: `POST /v1/patients/{id}/events`.
//!
//! These are HTTP-level protocol tests. The storage-level guarantees (monotonic
//! sequence_ids, dedupe, org isolation) are the contract suite's job in
//! `repository_contract.rs`; what's tested here is the *endpoint's* behavior —
//! batch validation, the path/payload match check, the S3 ownership guard, and
//! end-to-end idempotency over the wire.

use std::sync::Arc;

use sqlx::PgPool;
use uuid::Uuid;

use er_domain::{CaptureMode, Disposition, NoteEvent, NotePhase};
use er_server::repository::PostgresRepository;
use er_server::routes;
use er_store::repository::Repository;

mod common;
use common::{
    ORG_A, ORG_B, event_row_count, get, get_json, mint, post_json, seed_org_and_clinician,
    seed_registered_patient, test_state,
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

    let (first_status, first_body) =
        post_json(&app, &events_url(patient), &token, batch.clone()).await;
    let (second_status, second_body) = post_json(&app, &events_url(patient), &token, batch).await;

    assert_eq!(first_status, 200);
    assert_eq!(second_status, 200, "a replay is success, not a conflict");

    let first_seqs = sequence_ids(&first_body);
    let second_seqs = sequence_ids(&second_body);
    assert_eq!(first_seqs.len(), 3);
    assert_eq!(
        first_seqs, second_seqs,
        "replay returns the ORIGINAL sequence_ids"
    );
    assert!(
        first_seqs.windows(2).all(|w| w[0] < w[1]),
        "monotonic within the batch"
    );
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
    assert_eq!(
        event_row_count(&pool, patient).await,
        0,
        "the valid event must not land either"
    );
    assert_eq!(
        event_row_count(&pool, other).await,
        0,
        "and certainly not the smuggled one"
    );
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
    assert_eq!(
        event_row_count(&pool, patient_b).await,
        0,
        "org A must not write into org B"
    );

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
    assert_eq!(
        event_row_count(&pool, patient).await,
        0,
        "rejected before any insert"
    );
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
        seqs.push(
            h.await
                .expect("task panicked")
                .expect("append under contention"),
        );
    }

    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(
        seqs.len(),
        WRITERS,
        "every writer got a distinct sequence_id"
    );
    assert_eq!(
        event_row_count(&pool, patient).await,
        WRITERS as i64,
        "no lost writes"
    );

    // The stream reads back in sequence order with no duplicates — the ordering
    // guarantee downsync's cursor (S5) is about to depend on.
    let stored = PostgresRepository::for_org(pool.clone(), ORG_A)
        .get_events(patient)
        .await
        .unwrap();
    let read_seqs: Vec<u64> = stored.iter().map(|e| e.sequence_id).collect();
    assert!(
        read_seqs.windows(2).all(|w| w[0] < w[1]),
        "strictly increasing"
    );
    assert_eq!(
        read_seqs, seqs,
        "reader sees exactly what the writers were promised"
    );
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
    assert_eq!(
        event_row_count(&pool, patient_b).await,
        0,
        "nothing stored for org B"
    );
}

// ===========================================================================
// S5 — Event downsync: GET /v1/patients/{id}/events?since=N
// ===========================================================================

/// A realistic five-event encounter, written straight through the repository.
/// Returns the events in order alongside their assigned sequence_ids, so tests
/// can assert on both the domain payloads and the cursor values.
async fn seed_five_events(pool: &PgPool, org: Uuid, patient: Uuid) -> (Vec<NoteEvent>, Vec<u64>) {
    let repo = PostgresRepository::for_org(pool.clone(), org);
    let encounter = Uuid::now_v7();
    let events = vec![
        NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient),
        NoteEvent::transcript_ready(
            patient,
            encounter,
            "substernal chest pressure, radiating to the left arm".to_string(),
            chrono::Utc::now(),
        ),
        NoteEvent::audio_stopped(patient, encounter),
        NoteEvent::trigger_mdm(patient),
        NoteEvent::review_ready(patient),
    ];

    let mut seqs = Vec::with_capacity(events.len());
    for e in &events {
        seqs.push(
            repo.append_event(patient, Uuid::now_v7(), e.clone())
                .await
                .expect("seed event"),
        );
    }
    (events, seqs)
}

fn page_seqs(body: &serde_json::Value) -> Vec<u64> {
    body["events"]
        .as_array()
        .expect("events array")
        .iter()
        .map(|e| e["sequence_id"].as_u64().expect("sequence_id"))
        .collect()
}

fn events_query(patient: Uuid, query: &str) -> String {
    format!("/v1/patients/{patient}/events{query}")
}

/// The paging contract itself: a page smaller than the remaining stream must say so.
#[sqlx::test]
async fn page_limit_truncates_and_sets_has_more(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let (_, seqs) = seed_five_events(&pool, ORG_A, patient).await;
    let app = routes::router(test_state(pool).await);

    let (status, body) = get_json(
        &app,
        &events_query(patient, "?since=0&limit=2"),
        &mint("doc-joey-a"),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(page_seqs(&body), seqs[0..2].to_vec(), "first two, in order");
    assert_eq!(body["has_more"], true, "three events remain");
}

/// Absent query parameters mean "from the beginning, default page" — a bare
/// `GET …/events` must be a valid full-stream fetch, not a 400.
#[sqlx::test]
async fn missing_query_params_default_to_the_whole_stream(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let (_, seqs) = seed_five_events(&pool, ORG_A, patient).await;
    let app = routes::router(test_state(pool).await);

    let (status, body) = get_json(&app, &events_query(patient, ""), &mint("doc-joey-a")).await;

    assert_eq!(status, 200);
    assert_eq!(page_seqs(&body), seqs);
    assert_eq!(body["has_more"], false, "the default page holds all five");
}

/// §C3's cursor loop, run for real: start at 0, advance to the last sequence_id
/// of each page, stop when has_more is false. Every event exactly once, in order,
/// with no gaps and no repeats. This is the loop every client will implement.
#[sqlx::test]
async fn cursor_loop_visits_every_event_exactly_once(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let (_, seqs) = seed_five_events(&pool, ORG_A, patient).await;
    let app = routes::router(test_state(pool).await);
    let token = mint("doc-joey-a");

    let mut cursor = 0u64;
    let mut seen = Vec::new();
    for _ in 0..10 {
        // bounded: a correct loop needs 3 passes at limit=2; 10 catches a non-terminating one
        let (status, body) = get_json(
            &app,
            &events_query(patient, &format!("?since={cursor}&limit=2")),
            &token,
        )
        .await;
        assert_eq!(status, 200);

        let page = page_seqs(&body);
        seen.extend(page.iter().copied());
        if let Some(&last) = page.last() {
            cursor = last;
        }
        if body["has_more"] == false {
            break;
        }
    }

    assert_eq!(
        seen, seqs,
        "the loop reconstructs the stream in order, once each"
    );
}

/// A caller already at the head of the stream gets an empty page, not an error
/// and not a repeat of the last event — proof `get_events_since` is exclusive.
#[sqlx::test]
async fn cursor_at_head_returns_empty_page(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let (_, seqs) = seed_five_events(&pool, ORG_A, patient).await;
    let app = routes::router(test_state(pool).await);

    let last = seqs.last().copied().unwrap();
    let (status, body) = get_json(
        &app,
        &events_query(patient, &format!("?since={last}")),
        &mint("doc-joey-a"),
    )
    .await;

    assert_eq!(status, 200);
    assert!(
        body["events"].as_array().unwrap().is_empty(),
        "nothing after the head"
    );
    assert_eq!(body["has_more"], false);
}

/// The dropped-phone story, proven end to end: a fresh device that downloads the
/// stream from `since=0` and replays it locally must arrive at the SAME note the
/// server-side stream produces. Compared as whole serialized documents rather than
/// spot-checked fields, minus only the two values that are non-deterministic by
/// construction (`id` and `created_at` come from `PatientNote::new`, not the events).
#[sqlx::test]
async fn wire_replay_matches_local_replay(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let (local_events, _) = seed_five_events(&pool, ORG_A, patient).await;
    let app = routes::router(test_state(pool).await);

    let (status, body) = get_json(
        &app,
        &events_query(patient, "?since=0"),
        &mint("doc-joey-a"),
    )
    .await;
    assert_eq!(status, 200);

    // Rebuild the domain events from the wire representation alone.
    let wire_events: Vec<NoteEvent> = body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| serde_json::from_value(e["event"].clone()).expect("event deserializes"))
        .collect();
    assert_eq!(wire_events.len(), local_events.len());

    let from_wire = er_domain::replay::replay(patient, &wire_events);
    let from_local = er_domain::replay::replay(patient, &local_events);

    // Three values are non-deterministic by construction rather than by event data:
    //   - `id` / `created_at`  — assigned by PatientNote::new, not derived from events
    //   - `audit_log[*].id`    — AuditEntry::with_timestamp stamps a fresh Uuid::now_v7()
    //     (audit.rs:50). Its *timestamp* was deliberately made deterministic from
    //     clinical_occurred_at; the id was not. See the note in this test's sibling
    //     assertion below — audit CONTENT is still compared in full.
    let normalize = |note: &er_domain::PatientNote| {
        let mut v = serde_json::to_value(note).expect("note serializes");
        let obj = v.as_object_mut().unwrap();
        obj.remove("id");
        obj.remove("created_at");
        for entry in obj["audit_log"].as_array_mut().unwrap() {
            entry.as_object_mut().unwrap().remove("id");
        }
        v
    };

    assert_eq!(
        normalize(&from_wire),
        normalize(&from_local),
        "a note replayed from downsynced events must equal the locally replayed note"
    );

    // Guard the exception: only audit ENTRY IDS may differ. If a future change makes
    // some other field non-deterministic, the assert above catches it; this one keeps
    // the audit trail itself — actions, order, and clinical timestamps — under test.
    let audit_of = |note: &er_domain::PatientNote| {
        note.audit_log
            .iter()
            .map(|e| (format!("{:?}", e.action), e.timestamp))
            .collect::<Vec<_>>()
    };
    assert_eq!(audit_of(&from_wire), audit_of(&from_local));
    assert!(
        !from_wire.audit_log.is_empty(),
        "the fixture must actually produce audit entries"
    );
}

/// Downsync is behind the same ownership guard as every other per-patient route.
#[sqlx::test]
async fn downsync_of_another_orgs_patient_is_404(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    seed_org_and_clinician(&pool, ORG_B, "doc-peppas-b").await;
    let patient_b = seed_registered_patient(&pool, ORG_B).await;
    seed_five_events(&pool, ORG_B, patient_b).await;
    let app = routes::router(test_state(pool).await);

    let url = events_query(patient_b, "?since=0");
    assert_eq!(
        get(&app, &url, Some(&mint("doc-joey-a"))).await,
        404,
        "org A is refused"
    );
    assert_eq!(
        get(&app, &url, Some(&mint("doc-peppas-b"))).await,
        200,
        "org B owns it"
    );
}

/// The paging off-by-one: a page that exactly consumes the remaining stream must
/// report `has_more: false`. `has_more` is `len > limit`, and every other test here
/// has len either well above or well below limit — only len == limit distinguishes
/// `>` from `>=`, and `>=` would send every client on one pointless extra round trip.
#[sqlx::test]
async fn exactly_full_page_does_not_claim_more(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let (_, seqs) = seed_five_events(&pool, ORG_A, patient).await;
    let app = routes::router(test_state(pool).await);

    let (status, body) = get_json(
        &app,
        &events_query(patient, "?since=0&limit=5"), // exactly the whole stream
        &mint("doc-joey-a"),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(page_seqs(&body), seqs, "all five fit");
    assert_eq!(
        body["has_more"], false,
        "the stream is exhausted, not one-short"
    );
}

// ---------------------------------------------------------------------------
// S11 — the Step 5 gate, as a test
// ---------------------------------------------------------------------------

/// The whole step in one test: a shift and a patient created through the API, a
/// complete ED visit published by the *unmodified* `EventBus` over HTTP, the
/// device thrown away, and the note recovered on a second device that has
/// nothing but a token — then read back in both dialects (structured note and
/// clipboard text) and the shift closed.
///
/// This is `E5`'s two-terminal demo with the terminals replaced by two clients,
/// and it is the acceptance criterion "CLI-as-client drives full lifecycle
/// remotely" in executable form. Nothing here is server-test scaffolding: every
/// call is one a device makes.
#[sqlx::test]
async fn full_remote_lifecycle(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let base = common::spawn_server(routes::router(test_state(pool).await)).await;
    let token = mint("doc-joey-a");

    // --- Device one: boot (S10's startup path) ---
    let api = er_api::client::ServerApi::new(base.as_str(), token.as_str());
    let repo: Arc<dyn Repository> = Arc::new(er_api::client::RemoteRepository::new(
        base.as_str(),
        token.as_str(),
    ));

    assert!(
        api.active_shift().await.unwrap().is_none(),
        "first boot of the day"
    );
    let shift = api.start_shift().await.unwrap();

    let patient = Uuid::now_v7(); // client-stamped (D5.3)
    api.register_patient(&er_api::RegisterPatientRequest {
        patient_id: patient,
        chief_complaint: "chest pain".to_string(),
        display_label: Some("Room 4".to_string()),
    })
    .await
    .expect("register the patient before any event mentions it (§C6)");

    // --- The visit, through the same EventBus the local CLI uses ---
    let store = er_app::load::new_store();
    let bus = er_app::EventBus::new(1024, Some(Arc::clone(&repo)));
    let events = common::full_visit_events(patient, Uuid::now_v7());
    for event in &events {
        bus.publish(event.clone(), &store)
            .await
            .expect("publish over HTTP");
    }
    assert_eq!(store.get(&patient).unwrap().phase, NotePhase::Signed);

    // --- The phone goes in the sharps bin. No local database, no shift.json. ---
    drop(store);
    drop(bus);

    // --- Device two: same token, empty everything ---
    let loaner = er_api::client::ServerApi::new(base.as_str(), token.as_str());
    let loaner_repo: Arc<dyn Repository> = Arc::new(er_api::client::RemoteRepository::new(
        base.as_str(),
        token.as_str(),
    ));

    let resumed = loaner
        .active_shift()
        .await
        .unwrap()
        .expect("the shift is still open on the server — that is the resume story");
    assert_eq!(resumed.shift_id, shift.shift_id);
    assert_eq!(
        resumed
            .patients
            .iter()
            .map(|p| p.patient_id)
            .collect::<Vec<_>>(),
        vec![patient],
        "the roster comes back without any local state"
    );
    assert_eq!(resumed.patients[0].chief_complaint, "chest pain");
    assert_eq!(resumed.patients[0].display_label.as_deref(), Some("Room 4"));

    // The recovery algorithm is untouched application code: snapshot fast-path
    // plus delta replay, answered over HTTP.
    let reloaded = er_app::load::load_patient_note(loaner_repo.as_ref(), patient)
        .await
        .unwrap()
        .expect("the note reloads from the server alone");
    assert_eq!(reloaded.phase, NotePhase::Signed);
    assert_eq!(
        reloaded.note_sections.hpi.as_deref(),
        Some("55yo with substernal chest pressure since 0600.")
    );
    assert_eq!(reloaded.encounter_count, 1);

    // --- The server's own read model agrees with the client's replay ---
    let note = loaner.note(patient).await.unwrap();
    assert_eq!(note.phase, "Signed");
    assert_eq!(note.hpi, reloaded.note_sections.hpi);
    assert_eq!(note.mdm, reloaded.note_sections.mdm);
    assert_eq!(note.results.len(), 1);
    assert_eq!(note.medications.len(), 1);
    assert_eq!(note.diagnoses.len(), 1);
    assert_eq!(note.data_reviewed.len(), 1);
    assert!(
        matches!(note.disposition, Some(Disposition::Admit { .. })),
        "disposition survived the round trip: {:?}",
        note.disposition
    );

    // --- The clipboard dialect: what actually gets pasted into Epic ---
    let text = loaner.export(patient).await.unwrap();
    assert!(text.contains("Chief Complaint: chest pain"), "got:\n{text}");
    assert!(
        text.contains("55yo with substernal chest pressure"),
        "got:\n{text}"
    );
    assert!(text.contains("NSTEMI"), "got:\n{text}");
    assert!(text.contains("Troponin I"), "got:\n{text}");

    // --- Every event landed exactly once, no matter which client asked ---
    assert_eq!(
        loaner_repo.get_events(patient).await.unwrap().len(),
        events.len(),
        "the stream holds one row per published event"
    );

    // --- End of shift, from the loaner ---
    let ended = loaner.end_shift(shift.shift_id).await.unwrap();
    assert_eq!(ended.status, "ended");
    assert!(loaner.active_shift().await.unwrap().is_none());
}
