//! One test suite, two implementations (deep-dive §6).
//!
//! `suite` holds the pure Repository-contract assertions and takes
//! `Arc<dyn Repository>`, so it neither knows nor cares which backend it got.
//! `sqlite_repository_satisfies_contract` runs it against an in-memory SQLite;
//! `postgres_repository_satisfies_contract` runs it against a pristine,
//! per-test Postgres database that `#[sqlx::test]` creates from ./migrations.
//! If a behavior passes on one backend and fails on the other, the trait's
//! promise of interchangeability is broken — that divergence is what this
//! file exists to catch.
//!
//! Backend-specific tests (checksum tampering, org isolation) live below the
//! shared suite; they need raw access to the underlying database.

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use er_domain::replay::replay;
use er_domain::{CaptureMode, NoteEvent, NotePhase};
use er_server::repository::PostgresRepository;
use er_store::repository::{Repository, SqliteRepository};

/// Pure storage-contract assertions every `Repository` must satisfy.
async fn suite(repo: Arc<dyn Repository>) {
    let patient = Uuid::now_v7();
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
    assert_eq!(events[0].event_id, id1, "envelope carries the client-stamped id");
    assert_eq!(events[0].event.event_type(), "AudioStarted");
    match &events[1].event {
        NoteEvent::TranscriptReady { text, .. } => {
            assert_eq!(text, "chest pain, worse on exertion", "payload round-trips intact")
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
    let first = repo.append_event(patient, dup_id, dup_ev.clone()).await.unwrap();
    let second = repo.append_event(patient, dup_id, dup_ev.clone()).await.unwrap();
    assert_eq!(first, second, "a retried append returns the original sequence_id");
    assert_eq!(
        repo.event_count().await.unwrap(),
        before + 1,
        "a retried append must not store a second row"
    );

    // --- unknown patient reads are empty, not errors ---
    assert!(repo.get_events(Uuid::now_v7()).await.unwrap().is_empty());
    assert!(repo.get_latest_snapshot(Uuid::now_v7()).await.unwrap().is_none());

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

#[tokio::test]
async fn sqlite_repository_satisfies_contract() {
    let conn = er_store::db::open_memory_db().unwrap();
    suite(Arc::new(SqliteRepository::new(conn))).await;
}

#[sqlx::test(migrations = "./migrations")]
async fn postgres_repository_satisfies_contract(pool: PgPool) {
    suite(Arc::new(PostgresRepository::for_org(pool, Uuid::now_v7()))).await;
}

/// A tampered snapshot must be treated as absent so load_patient_note falls
/// back to full replay — same behavior the SQLite impl already guarantees.
#[sqlx::test(migrations = "./migrations")]
async fn postgres_snapshot_checksum_mismatch_falls_back(pool: PgPool) {
    let repo = PostgresRepository::for_org(pool.clone(), Uuid::now_v7());
    let patient = Uuid::now_v7();
    let encounter = Uuid::now_v7();

    let ev = NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient);
    let seq = repo.append_event(patient, Uuid::now_v7(), ev.clone()).await.unwrap();
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
    let org_a = PostgresRepository::for_org(pool.clone(), Uuid::now_v7());
    let org_b = PostgresRepository::for_org(pool, Uuid::now_v7());
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
    let repo: Arc<dyn Repository> =
        Arc::new(PostgresRepository::for_org(pool, Uuid::now_v7()));
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
    bus.publish(NoteEvent::trigger_mdm(patient), &store).await.unwrap();
    bus.publish(NoteEvent::review_ready(patient), &store).await.unwrap();
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
