//! S9 — the client, under test: `ServerApi` + `RemoteRepository`.
//!
//! `repository_contract.rs` proves the remote backend keeps the *storage* promise.
//! This file covers what only the client can get wrong: the two failure classes it
//! must keep apart, the paging loop it hides from callers, the `hydrate()` it must
//! not forget, and the non-storage endpoints (`shifts`, `export`) that live on
//! `ServerApi` because they were never `Repository` operations.
//!
//! Every test here talks to a real socket (`common::spawn_server`) — the oneshot
//! style used elsewhere bypasses HTTP, and HTTP is the thing under test.

use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use er_api::client::{RemoteRepository, ServerApi};
use er_domain::replay::replay;
use er_domain::{CaptureMode, NoteEvent, NotePhase};
use er_server::routes;
use er_store::{Repository, StoreError};

mod common;

const DOC: &str = "remote-client-doc";

/// Boot a server for this test's database and hand back a client pair pointed at
/// it: one `ServerApi` for the non-storage endpoints, one `RemoteRepository` for
/// the trait. Both carry the same valid token for org A's clinician.
async fn client_pair(pool: &PgPool) -> (ServerApi, RemoteRepository, String) {
    common::seed_org_and_clinician(pool, common::ORG_A, DOC).await;
    let base = common::spawn_server(routes::router(common::test_state(pool.clone()).await)).await;
    let token = common::mint(DOC);
    (
        ServerApi::new(base.as_str(), token.as_str()),
        RemoteRepository::new(base.as_str(), token.as_str()),
        base,
    )
}

/// Register a client-stamped patient through the API, the way a device does.
async fn register(api: &ServerApi, chief_complaint: &str) -> Uuid {
    let patient_id = Uuid::now_v7();
    api.register_patient(&er_api::RegisterPatientRequest {
        patient_id,
        chief_complaint: chief_complaint.to_string(),
        display_label: None,
    })
    .await
    .expect("register patient");
    patient_id
}

// ---------------------------------------------------------------------------
// Presigned upload boundary (Step 6 S8)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn presigned_put_sends_only_the_signed_content_type_and_scrubs_failures() {
    let upload = axum::Router::new().route(
        "/upload",
        axum::routing::put(
            |headers: axum::http::HeaderMap, body: axum::body::Bytes| async move {
                assert!(
                    headers.get(axum::http::header::AUTHORIZATION).is_none(),
                    "the clinician token must not cross into object storage"
                );
                assert_eq!(
                    headers
                        .get(axum::http::header::CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok()),
                    Some("audio/wav")
                );
                assert_eq!(body.as_ref(), b"RIFF synthetic audio");
                axum::http::StatusCode::NO_CONTENT
            },
        ),
    );
    let upload_base = common::spawn_server(upload).await;
    let api = ServerApi::new("http://api.invalid", "clinician-secret");
    api.put_presigned(
        &format!("{upload_base}/upload?X-Amz-Signature=upload-secret"),
        b"RIFF synthetic audio".to_vec(),
        "audio/wav",
    )
    .await
    .expect("the exact signed request succeeds");

    let rejected = axum::Router::new().route(
        "/upload",
        axum::routing::put(|| async {
            (
                axum::http::StatusCode::FORBIDDEN,
                "secret object-store diagnostic",
            )
        }),
    );
    let rejected_base = common::spawn_server(rejected).await;
    let rejection = api
        .put_presigned(
            &format!("{rejected_base}/upload?X-Amz-Signature=upload-secret"),
            vec![1],
            "audio/wav",
        )
        .await
        .expect_err("a rejected upload must fail");
    let rendered = rejection.to_string();
    assert!(matches!(
        rejection,
        StoreError::Remote { status: 403, .. }
    ));
    assert!(!rendered.contains("secret object-store diagnostic"));
    assert!(!rendered.contains("upload-secret"));

    let unreachable = common::unreachable_url().await;
    let transport = api
        .put_presigned(
            &format!("{unreachable}/upload?X-Amz-Signature=transport-secret"),
            vec![1],
            "audio/wav",
        )
        .await
        .expect_err("a disconnected object store must fail");
    let rendered = transport.to_string();
    assert!(matches!(transport, StoreError::Backend(_)));
    assert!(!rendered.contains("transport-secret"));
    assert!(!rendered.contains(&unreachable));
}

// ---------------------------------------------------------------------------
// Failure classification (§C8, flow D)
// ---------------------------------------------------------------------------

/// The same status code means different things per endpoint, so the client must
/// interpret it per endpoint rather than globally. A missing snapshot is the
/// normal "no checkpoint, replay from zero" answer; a 404 on the write path is a
/// real error the caller has to see.
#[sqlx::test]
async fn a_snapshot_404_is_absence_but_a_write_404_is_an_error(pool: PgPool) {
    let (api, repo, _) = client_pair(&pool).await;
    let patient = register(&api, "chest pain").await;

    assert!(
        repo.get_latest_snapshot(patient).await.unwrap().is_none(),
        "a registered patient with no checkpoint must read as None, not as an error"
    );

    let stranger = Uuid::now_v7(); // never registered
    let err = repo
        .append_event(
            stranger,
            Uuid::now_v7(),
            NoteEvent::data_reviewed(stranger, "prior ECG".to_string()),
        )
        .await
        .expect_err("appending for an unregistered patient must fail");
    match err {
        StoreError::Remote { status: 404, .. } => {}
        other => panic!("expected Remote 404, got {other:?}"),
    }
}

/// A rejected token is a `Remote` error, never silent success and never a
/// transport error — the server answered, it just said no.
#[sqlx::test]
async fn an_unusable_token_surfaces_as_remote_401(pool: PgPool) {
    common::seed_org_and_clinician(&pool, common::ORG_A, DOC).await;
    let base = common::spawn_server(routes::router(common::test_state(pool).await)).await;
    let repo = RemoteRepository::new(base.as_str(), "not-a-jwt");

    match repo.event_count().await.expect_err("401 must not read as success") {
        // 401 has an empty body by design (the caller learns nothing), so the
        // message is empty here — the status is the part clients branch on.
        StoreError::Remote { status: 401, .. } => {}
        other => panic!("expected Remote 401, got {other:?}"),
    }
}

/// State conflicts arrive as `Remote` with their status intact, which is how S10
/// knows to fall back to `GET /v1/shifts/active` instead of retrying the start.
#[sqlx::test]
async fn a_second_start_shift_surfaces_as_remote_409(pool: PgPool) {
    let (api, _, _) = client_pair(&pool).await;
    api.start_shift().await.expect("first shift starts");

    match api.start_shift().await.expect_err("second start must be rejected") {
        StoreError::Remote { status: 409, message } => {
            assert_eq!(message, "shift_already_active", "the error code survives decoding")
        }
        other => panic!("expected Remote 409, got {other:?}"),
    }
}

/// No response at all is a different failure than a rejection: one may be worth
/// retrying later from an offline queue, the other never will be.
#[sqlx::test]
async fn an_unreachable_server_is_a_backend_error_not_a_remote_one(pool: PgPool) {
    common::seed_org_and_clinician(&pool, common::ORG_A, DOC).await;
    let repo = RemoteRepository::new(common::unreachable_url().await, common::mint(DOC));

    match repo.event_count().await.expect_err("nothing is listening there") {
        StoreError::Backend(_) => {}
        other => panic!("expected Backend (transport) error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The cursor loop (§C3, flow B)
// ---------------------------------------------------------------------------

/// `get_events_since` promises the *complete* delta, so the client has to follow
/// pages itself. Page size 2 over a 5-event stream forces three round trips
/// (2 + 2 + 1) through the same loop production runs at 500.
#[sqlx::test]
async fn the_cursor_loop_returns_every_event_across_pages(pool: PgPool) {
    let (api, _, base) = client_pair(&pool).await;
    let token = common::mint(DOC);
    let repo = RemoteRepository::with_page_size(base.as_str(), token.as_str(), 2);

    let patient = register(&api, "chest pain").await;
    let encounter = Uuid::now_v7();
    let events = [
        NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient),
        NoteEvent::transcript_ready(patient, encounter, "cp x2h".to_string(), Utc::now()),
        NoteEvent::audio_stopped(patient, encounter),
        NoteEvent::data_reviewed(patient, "prior ECG".to_string()),
        NoteEvent::trigger_mdm(patient),
    ];
    let mut seqs = Vec::new();
    for e in &events {
        seqs.push(repo.append_event(patient, Uuid::now_v7(), e.clone()).await.unwrap());
    }

    let all = repo.get_events(patient).await.unwrap();
    assert_eq!(all.len(), events.len(), "paging must not drop or duplicate events");
    assert_eq!(
        all.iter().map(|e| e.sequence_id).collect::<Vec<_>>(),
        seqs,
        "one ascending stream comes back, not one page"
    );
    assert_eq!(all[1].event.event_type(), "TranscriptReady");

    // Mid-stream cursor: strictly greater than `since`, still multi-page.
    let delta = repo.get_events_since(patient, seqs[1]).await.unwrap();
    assert_eq!(
        delta.iter().map(|e| e.sequence_id).collect::<Vec<_>>(),
        seqs[2..],
        "a cursor read resumes after the given sequence, exactly once each"
    );

    // A cursor at the head is an empty page, not a hang.
    assert!(repo.get_events_since(patient, *seqs.last().unwrap()).await.unwrap().is_empty());
}

/// The one server behavior the client cannot survive politely: a page that
/// promises more and delivers nothing leaves the cursor where it was. Looping
/// would hang the caller; returning what arrived would replay into a wrong note.
/// Failing is the only safe third option, so it is pinned here against a
/// deliberately lying server (no database and no auth needed — the lie is the
/// whole fixture).
#[tokio::test]
async fn an_empty_page_that_claims_more_fails_instead_of_looping() {
    let liar = axum::Router::new().route(
        "/v1/patients/{id}/events",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({ "events": [], "has_more": true }))
        }),
    );
    let repo = RemoteRepository::new(common::spawn_server(liar).await, "any-token");

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        repo.get_events(Uuid::now_v7()),
    )
    .await
    .expect("the cursor loop must terminate on a malformed page");

    match outcome.expect_err("a page that can't be paged past is not a success") {
        StoreError::Backend(e) => assert!(e.to_string().contains("has_more")),
        other => panic!("expected a Backend error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Snapshots over the wire (flow C)
// ---------------------------------------------------------------------------

/// `PatientNote::state` is `#[serde(skip)]`, so a note that crossed JSON has no
/// state object until `hydrate()` rebuilds it — and `apply_transition` on a
/// stateless note does nothing *quietly*. This asserts the observable
/// consequence: a downloaded note can still advance its state machine.
#[sqlx::test]
async fn a_downloaded_snapshot_arrives_hydrated(pool: PgPool) {
    let (api, repo, _) = client_pair(&pool).await;
    let patient = register(&api, "chest pain").await;
    let encounter = Uuid::now_v7();

    let events = [
        NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient),
        NoteEvent::audio_stopped(patient, encounter),
    ];
    let mut last_seq = 0;
    for e in &events {
        last_seq = repo.append_event(patient, Uuid::now_v7(), e.clone()).await.unwrap();
    }
    let note = replay(patient, &events);
    assert_eq!(note.phase, NotePhase::Idle);
    repo.save_snapshot(patient, last_seq, &note).await.unwrap();

    let (seq, mut loaded) = repo
        .get_latest_snapshot(patient)
        .await
        .unwrap()
        .expect("the snapshot just saved comes back");
    assert_eq!(seq, last_seq);
    assert_eq!(loaded.phase, NotePhase::Idle);
    assert!(loaded.state.is_some(), "hydrate() must run before the note is handed back");

    loaded.handle_trigger_mdm(Utc::now());
    assert_eq!(
        loaded.phase,
        NotePhase::Finalizing,
        "a note that crossed the wire must still transition; a missing state object \
         would leave it in Idle without erroring"
    );
}

// ---------------------------------------------------------------------------
// The non-Repository half of ServerApi
// ---------------------------------------------------------------------------

/// Shifts are why `ServerApi` exists separately: none of this is a storage
/// operation, and S10's CLI calls it directly.
#[sqlx::test]
async fn server_api_drives_the_whole_shift_lifecycle(pool: PgPool) {
    let (api, _, _) = client_pair(&pool).await;

    assert!(
        api.active_shift().await.unwrap().is_none(),
        "first boot has no shift: a 404 is the normal answer, not an error"
    );

    let started = api.start_shift().await.unwrap();
    assert_eq!(started.status, "active");
    assert!(started.patients.is_empty());

    // Registering while a shift is open attaches the patient to its roster.
    let patient = register(&api, "ankle injury").await;
    let resumed = api
        .active_shift()
        .await
        .unwrap()
        .expect("the open shift is discoverable without local state (D5.6)");
    assert_eq!(resumed.shift_id, started.shift_id);
    assert_eq!(
        resumed.patients.iter().map(|p| p.patient_id).collect::<Vec<_>>(),
        vec![patient],
    );
    assert_eq!(resumed.patients[0].chief_complaint, "ankle injury");

    let ended = api.end_shift(started.shift_id).await.unwrap();
    assert_eq!(ended.status, "ended");
    assert!(ended.ended_at.is_some());
    assert!(
        api.active_shift().await.unwrap().is_none(),
        "an ended shift is no longer the active one"
    );
}

/// The architectural claim, exercised end to end: `EventBus` and
/// `load_patient_note` are handed a `RemoteRepository` and behave exactly as they
/// do against SQLite — persist-first, snapshot on transitions, reload from the
/// backend alone. Neither one is aware of HTTP.
#[sqlx::test]
async fn the_app_layer_runs_unchanged_over_http(pool: PgPool) {
    let (api, _, base) = client_pair(&pool).await;
    let token = common::mint(DOC);
    let patient = register(&api, "chest pain").await;
    let encounter = Uuid::now_v7();

    let repo: Arc<dyn Repository> = Arc::new(RemoteRepository::new(base.as_str(), token.as_str()));
    let store = er_app::load::new_store();
    let bus = er_app::EventBus::new(32, Some(Arc::clone(&repo)));

    for event in [
        NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient),
        NoteEvent::transcript_ready(patient, encounter, "cp x2h".to_string(), Utc::now()),
        NoteEvent::audio_stopped(patient, encounter),
        NoteEvent::trigger_mdm(patient),
        NoteEvent::review_ready(patient),
        NoteEvent::sign(patient),
    ] {
        bus.publish(event, &store).await.expect("publish over HTTP");
    }
    assert_eq!(store.get(&patient).unwrap().phase, NotePhase::Signed);

    // Simulated device loss: the in-memory projection is gone and the local disk
    // holds nothing. The server is the only truth left.
    drop(store);

    let loaded = er_app::load::load_patient_note(repo.as_ref(), patient)
        .await
        .unwrap()
        .expect("the note reloads from the server alone");
    assert_eq!(loaded.phase, NotePhase::Signed);
    assert_eq!(loaded.encounter_count, 1);
    assert_eq!(loaded.encounters[0].transcript.as_deref(), Some("cp x2h"));

    // `export` is the one endpoint that answers text/plain, so it is the one that
    // would break if the client tried to JSON-decode every response.
    let text = api.export(patient).await.expect("export the note");
    assert!(text.starts_with("ED PROVIDER NOTE"), "got: {text}");
    assert!(text.contains("Chief Complaint: chest pain"), "got: {text}");
}
