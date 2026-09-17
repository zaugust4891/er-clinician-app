//! One test suite, three implementations (deep-dive §6; S9/S11).
//!
//! `suite` holds the pure Repository-contract assertions and takes
//! `Arc<dyn Repository>`, so it neither knows nor cares which backend it got.
//! `sqlite_repository_satisfies_contract` runs it against an in-memory SQLite;
//! `postgres_repository_satisfies_contract` runs it against a pristine,
//! per-test Postgres database that `#[sqlx::test]` creates from ./migrations;
//! `remote_repository_satisfies_contract` runs it against a real HTTP server
//! through `er_api::client::RemoteRepository`. If a behavior passes on one
//! backend and fails on another, the trait's promise of interchangeability is
//! broken — that divergence is what this file exists to catch.
//!
//! Backend-specific tests (checksum tampering, org isolation) live below the
//! shared suite; they need raw access to the underlying database.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use er_domain::replay::replay;
use er_domain::{CaptureMode, NoteEvent, NotePhase};
use er_server::repository::PostgresRepository;
use er_server::routes;
use er_store::repository::{Repository, SqliteRepository};

mod common;

/// "How a patient becomes known to this backend."
///
/// The remote implementation adds a precondition the raw engines don't have: the
/// server only accepts writes for a *registered* patient (§C6). The contract was
/// always implicitly "append for a known patient" — SQLite and Postgres just
/// treat every UUID as known. Injecting that step is the honest way to state it
/// without weakening the assertions.
///
/// The boxed-pinned future is the standard way to store an async closure in a
/// value (`futures::future::BoxFuture` is this same type with a shorter name).
type PatientFactory = Box<dyn Fn() -> Pin<Box<dyn Future<Output = Uuid> + Send>> + Send + Sync>;

/// SQLite and Postgres: any UUID is already a patient.
fn bare_patient_factory() -> PatientFactory {
    Box::new(|| Box::pin(async { Uuid::now_v7() }))
}

/// Pure storage-contract assertions every `Repository` must satisfy.
async fn suite(repo: Arc<dyn Repository>, new_patient: &PatientFactory) {
    let patient = new_patient().await;
    let encounter = Uuid::now_v7();

    // --- append → get_events round-trip, ordering, envelope fields ---
    let e1 = NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient);
    let e2 = NoteEvent::transcript_ready(
        patient,
        encounter,
        "chest pain, worse on exertion".to_string(),
        Utc::now(),
    );
    let e3 = NoteEvent::audio_stopped(patient, encounter);

    let id1 = Uuid::now_v7();
    let seq1 = repo.append_event(patient, id1, e1.clone()).await.unwrap();
    let seq2 = repo
        .append_event(patient, Uuid::now_v7(), e2.clone())
        .await
        .unwrap();
    let seq3 = repo
        .append_event(patient, Uuid::now_v7(), e3.clone())
        .await
        .unwrap();
    assert!(seq1 < seq2 && seq2 < seq3, "sequence ids are monotonic");

    let events = repo.get_events(patient).await.unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].sequence_id, seq1);
    assert_eq!(
        events[0].event_id, id1,
        "envelope carries the client-stamped id"
    );
    assert_eq!(events[0].event.event_type(), "AudioStarted");
    match &events[1].event {
        NoteEvent::TranscriptReady { text, .. } => {
            assert_eq!(
                text, "chest pain, worse on exertion",
                "payload round-trips intact"
            )
        }
        other => panic!("expected TranscriptReady, got {other:?}"),
    }

    // occurred_at is the *clinical* time on both backends. Compare with 1ms
    // tolerance: Postgres TIMESTAMPTZ stores microseconds, chrono nanoseconds.
    let drift = events[1].occurred_at - e2.clinical_occurred_at();
    assert!(
        drift.abs() < chrono::Duration::milliseconds(1),
        "occurred_at must carry the event's clinical time, drifted {drift}"
    );

    // --- get_events_since returns only the delta ---
    let delta = repo.get_events_since(patient, seq1).await.unwrap();
    assert_eq!(delta.len(), 2);
    assert_eq!(delta[0].sequence_id, seq2);

    // --- idempotency: at-least-once delivery → exactly-once storage (D10) ---
    let before = repo.event_count().await.unwrap();
    let dup_id = Uuid::now_v7();
    let dup_ev = NoteEvent::data_reviewed(patient, "prior ECG reviewed".to_string());
    let first = repo
        .append_event(patient, dup_id, dup_ev.clone())
        .await
        .unwrap();
    let second = repo
        .append_event(patient, dup_id, dup_ev.clone())
        .await
        .unwrap();
    assert_eq!(
        first, second,
        "a retried append returns the original sequence_id"
    );
    assert_eq!(
        repo.event_count().await.unwrap(),
        before + 1,
        "a retried append must not store a second row"
    );

    // --- a patient nothing has happened to reads empty, not as an error ---
    let untouched = new_patient().await;
    assert!(repo.get_events(untouched).await.unwrap().is_empty());
    assert!(repo.get_latest_snapshot(untouched).await.unwrap().is_none());

    // --- snapshot save → load round-trip ---
    let all = repo.get_events(patient).await.unwrap();
    let domain_events: Vec<NoteEvent> = all.iter().map(|s| s.event.clone()).collect();
    let note = replay(patient, &domain_events);
    let last_seq = all.last().unwrap().sequence_id;

    let snaps_before = repo.snapshot_count().await.unwrap();
    repo.save_snapshot(patient, last_seq, &note).await.unwrap();
    let (snap_seq, loaded) = repo
        .get_latest_snapshot(patient)
        .await
        .unwrap()
        .expect("snapshot was just saved");
    assert_eq!(snap_seq, last_seq);
    assert_eq!(loaded.phase, note.phase);
    assert_eq!(loaded.encounter_count, note.encounter_count);
    assert_eq!(loaded.encounters.len(), note.encounters.len());
    assert_eq!(repo.snapshot_count().await.unwrap(), snaps_before + 1);

    // Saving again for the same patient upserts — replaces, never duplicates.
    repo.save_snapshot(patient, last_seq, &note).await.unwrap();
    assert_eq!(repo.snapshot_count().await.unwrap(), snaps_before + 1);
}

/// The tenancy migration added FKs from events/snapshots to orgs, so every
/// org a test writes under must have a parent row first.
async fn seed_org(pool: &PgPool) -> Uuid {
    let org = Uuid::now_v7();
    sqlx::query("INSERT INTO orgs (id, name) VALUES ($1, 'test org')")
        .bind(org)
        .execute(pool)
        .await
        .unwrap();
    org
}

#[tokio::test]
async fn sqlite_repository_satisfies_contract() {
    let conn = er_store::db::open_memory_db().unwrap();
    suite(
        Arc::new(SqliteRepository::new(conn)),
        &bare_patient_factory(),
    )
    .await;
}

#[sqlx::test(migrations = "./migrations")]
async fn postgres_repository_satisfies_contract(pool: PgPool) {
    let org = seed_org(&pool).await;
    suite(
        Arc::new(PostgresRepository::for_org(pool, org)),
        &bare_patient_factory(),
    )
    .await;
}

/// The third run, and the point of S9: the same assertions, with a JSON wire, an
/// HTTP server, an auth middleware, and a tenancy guard between the caller and
/// Postgres. Nothing in `suite` changes — the swap is the proof the abstraction
/// held.
#[sqlx::test(migrations = "./migrations")]
async fn remote_repository_satisfies_contract(pool: PgPool) {
    common::seed_org_and_clinician(&pool, common::ORG_A, "remote-doc").await;
    let base = common::spawn_server(routes::router(common::test_state(pool).await)).await;
    let token = common::mint("remote-doc");

    // A second client for the factory's registration calls: `RemoteRepository`
    // deliberately exposes only the seven storage methods, and registration is
    // not one of them (§C6 — it is an API-layer precondition, not storage).
    let api = Arc::new(er_api::client::ServerApi::new(base.clone(), token.clone()));
    let factory: PatientFactory = Box::new(move || {
        let api = Arc::clone(&api);
        Box::pin(async move {
            let patient_id = Uuid::now_v7(); // client-stamped (D5.3)
            api.register_patient(&er_api::RegisterPatientRequest {
                patient_id,
                chief_complaint: "chest pain".to_string(),
                display_label: None,
            })
            .await
            .expect("register patient");
            patient_id
        })
    });

    let repo: Arc<dyn Repository> = Arc::new(er_api::client::RemoteRepository::new(base, token));
    suite(repo, &factory).await;
}

/// A tampered snapshot must be treated as absent so load_patient_note falls
/// back to full replay — same behavior the SQLite impl already guarantees.
#[sqlx::test(migrations = "./migrations")]
async fn postgres_snapshot_checksum_mismatch_falls_back(pool: PgPool) {
    let org = seed_org(&pool).await;
    let repo = PostgresRepository::for_org(pool.clone(), org);
    let patient = Uuid::now_v7();
    let encounter = Uuid::now_v7();

    let ev = NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient);
    let seq = repo
        .append_event(patient, Uuid::now_v7(), ev.clone())
        .await
        .unwrap();
    let note = replay(patient, &[ev]);
    repo.save_snapshot(patient, seq, &note).await.unwrap();
    assert!(repo.get_latest_snapshot(patient).await.unwrap().is_some());

    sqlx::query("UPDATE snapshots SET checksum = 'tampered' WHERE patient_id = $1")
        .bind(patient)
        .execute(&pool)
        .await
        .unwrap();

    assert!(
        repo.get_latest_snapshot(patient).await.unwrap().is_none(),
        "corrupted snapshot must read as None (full-replay fallback)"
    );
}

/// D9: org A must never see org B's data. Two repositories on the same pool,
/// scoped to different orgs, must be fully isolated from each other.
#[sqlx::test(migrations = "./migrations")]
async fn postgres_repositories_are_org_isolated(pool: PgPool) {
    let (id_a, id_b) = (seed_org(&pool).await, seed_org(&pool).await);
    let org_a = PostgresRepository::for_org(pool.clone(), id_a);
    let org_b = PostgresRepository::for_org(pool, id_b);
    let patient = Uuid::now_v7();

    org_a
        .append_event(
            patient,
            Uuid::now_v7(),
            NoteEvent::data_reviewed(patient, "old records".to_string()),
        )
        .await
        .unwrap();

    assert_eq!(org_a.event_count().await.unwrap(), 1);
    assert!(
        org_b.get_events(patient).await.unwrap().is_empty(),
        "org B must never see org A's events"
    );
    assert_eq!(org_b.event_count().await.unwrap(), 0);
}

/// Crash-recovery semantics against Postgres: publish a full note lifecycle
/// through the EventBus (persist-first, projection, snapshots on transitions),
/// drop the in-memory projection, and reload purely from the database.
/// This passing is the proof that replay/snapshot/checksum semantics survived
/// the port (deep-dive §6.3).
#[sqlx::test(migrations = "./migrations")]
async fn postgres_full_lifecycle_survives_reload(pool: PgPool) {
    let org = seed_org(&pool).await;
    let repo: Arc<dyn Repository> = Arc::new(PostgresRepository::for_org(pool, org));
    let store = er_app::load::new_store();
    let bus = er_app::EventBus::new(32, Some(repo.clone()));

    let patient = Uuid::now_v7();
    let encounter = Uuid::now_v7();

    bus.publish(
        NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient),
        &store,
    )
    .await
    .unwrap();
    bus.publish(
        NoteEvent::transcript_ready(patient, encounter, "cp".to_string(), Utc::now()),
        &store,
    )
    .await
    .unwrap();
    bus.publish(NoteEvent::audio_stopped(patient, encounter), &store)
        .await
        .unwrap();
    bus.publish(NoteEvent::trigger_mdm(patient), &store)
        .await
        .unwrap();
    bus.publish(NoteEvent::review_ready(patient), &store)
        .await
        .unwrap();
    bus.publish(NoteEvent::sign(patient), &store).await.unwrap();

    assert_eq!(store.get(&patient).unwrap().phase, NotePhase::Signed);

    // Simulated crash: the in-memory projection is gone; Postgres is truth.
    drop(store);

    let loaded = er_app::load::load_patient_note(repo.as_ref(), patient)
        .await
        .unwrap()
        .expect("note reloads from Postgres alone");
    assert_eq!(loaded.phase, NotePhase::Signed);
    assert_eq!(loaded.encounter_count, 1);
    assert_eq!(loaded.encounters[0].transcript.as_deref(), Some("cp"));
}
