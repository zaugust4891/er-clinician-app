//! S8 — shifts: start, end, fetch, resume.
//!
//! The shift is the last piece of state that used to live in `shift.json` and
//! therefore died with the device. These tests are about durability and retry
//! safety: exactly one active shift per clinician (refereed by the database, not
//! by a read-then-write), an idempotent end, and a resume route a fresh device can
//! call knowing nothing but its own token.

use sqlx::PgPool;
use uuid::Uuid;

use er_server::routes;

mod common;
use common::{
    ORG_A, ORG_B, get, get_json, mint, post_empty, post_json, seed_org_and_clinician, test_state,
};

const SHIFTS: &str = "/v1/shifts";
const ACTIVE: &str = "/v1/shifts/active";

fn shift_url(shift_id: &str) -> String {
    format!("/v1/shifts/{shift_id}")
}
fn end_url(shift_id: &str) -> String {
    format!("/v1/shifts/{shift_id}/end")
}

fn shift_id_of(body: &serde_json::Value) -> String {
    body["shift_id"].as_str().expect("shift_id").to_owned()
}

/// Register a patient through the real endpoint so the S3 shift-attach runs.
async fn register(app: &axum::Router, token: &str, complaint: &str) -> Uuid {
    let patient_id = Uuid::now_v7();
    let (status, _) = post_json(
        app,
        "/v1/patients",
        token,
        serde_json::json!({ "patient_id": patient_id, "chief_complaint": complaint }),
    )
    .await;
    assert_eq!(status, 200, "registering {complaint}");
    patient_id
}

// ---------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn start_shift_returns_an_active_shift(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let app = routes::router(test_state(pool).await);

    let (status, body) = post_empty(&app, SHIFTS, &mint("doc-joey-a")).await;

    assert_eq!(status, 200);
    assert_eq!(body["status"], "active");
    assert!(body["ended_at"].is_null(), "a new shift has not ended");
    assert!(
        body["patients"].as_array().unwrap().is_empty(),
        "nobody on it yet"
    );
    assert!(!shift_id_of(&body).is_empty());
}

/// The partial unique index is the referee — not a read-then-write check, which
/// two simultaneous starts could both pass. 409 (not 500) is the contract: the
/// client's correct response is to fetch the shift it already has.
#[sqlx::test]
async fn starting_a_second_shift_is_409(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let app = routes::router(test_state(pool).await);
    let token = mint("doc-joey-a");

    let (first, _) = post_empty(&app, SHIFTS, &token).await;
    assert_eq!(first, 200);

    let (second, body) = post_empty(&app, SHIFTS, &token).await;
    assert_eq!(
        second, 409,
        "a unique-index violation must surface as Conflict, not 500"
    );
    assert_eq!(body["error"], "shift_already_active");
}

/// Two clinicians in the same org each get their own shift — the index is per
/// clinician, not per org.
#[sqlx::test]
async fn different_clinicians_hold_separate_shifts(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    seed_org_and_clinician(&pool, ORG_A, "doc-tony-a").await;
    let app = routes::router(test_state(pool).await);

    let (s1, b1) = post_empty(&app, SHIFTS, &mint("doc-joey-a")).await;
    let (s2, b2) = post_empty(&app, SHIFTS, &mint("doc-tony-a")).await;

    assert_eq!((s1, s2), (200, 200));
    assert_ne!(shift_id_of(&b1), shift_id_of(&b2));
}

// ---------------------------------------------------------------------------
// Resume — GET /v1/shifts/active
// ---------------------------------------------------------------------------

/// A clinician who has never started one. 404 is the normal first-boot answer,
/// not an error condition — the client responds by POSTing a new shift.
#[sqlx::test]
async fn active_shift_is_404_before_any_shift_exists(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let app = routes::router(test_state(pool).await);

    assert_eq!(get(&app, ACTIVE, Some(&mint("doc-joey-a"))).await, 404);
}

/// The dropped-phone request. A device that knows only its token recovers the
/// shift id AND the roster — in registration order, which is the durable form of
/// ShiftManager's stable-list invariant.
///
/// This also proves the route ordering: `/v1/shifts/active` must be matched as a
/// literal, not captured by `/v1/shifts/{id}` (which would reject "active" as a
/// malformed Uuid instead of answering).
#[sqlx::test]
async fn active_shift_resumes_with_the_roster_in_registration_order(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let app = routes::router(test_state(pool).await);
    let token = mint("doc-joey-a");

    let (_, started) = post_empty(&app, SHIFTS, &token).await;
    let expected_id = shift_id_of(&started);

    let first = register(&app, &token, "chest pain").await;
    let second = register(&app, &token, "ankle injury").await;
    let third = register(&app, &token, "headache").await;

    let (status, body) = get_json(&app, ACTIVE, &token).await;

    assert_eq!(status, 200);
    assert_eq!(
        shift_id_of(&body),
        expected_id,
        "the same shift, recovered from the token alone"
    );

    let roster: Vec<&str> = body["patients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["patient_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        roster,
        vec![first.to_string(), second.to_string(), third.to_string()],
        "patients come back in the order they were registered"
    );
    assert_eq!(body["patients"][1]["chief_complaint"], "ankle injury");
}

// ---------------------------------------------------------------------------
// Fetch by id
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn get_shift_by_id_matches_the_active_shift(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let app = routes::router(test_state(pool).await);
    let token = mint("doc-joey-a");

    let (_, started) = post_empty(&app, SHIFTS, &token).await;
    register(&app, &token, "chest pain").await;

    let (status, by_id) = get_json(&app, &shift_url(&shift_id_of(&started)), &token).await;
    let (_, by_active) = get_json(&app, ACTIVE, &token).await;

    assert_eq!(status, 200);
    assert_eq!(
        by_id, by_active,
        "both routes describe the same shift identically"
    );
}

#[sqlx::test]
async fn unknown_shift_id_is_404(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let app = routes::router(test_state(pool).await);

    let ghost = Uuid::now_v7().to_string();
    assert_eq!(
        get(&app, &shift_url(&ghost), Some(&mint("doc-joey-a"))).await,
        404
    );
}

// ---------------------------------------------------------------------------
// End
// ---------------------------------------------------------------------------

/// Ending twice is a retry, not an error. The body must be byte-identical the
/// second time — in particular `ended_at` must not slide forward, which is why
/// the UPDATE is conditional on `status = 'active'` rather than unconditional.
#[sqlx::test]
async fn ending_a_shift_is_idempotent(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let app = routes::router(test_state(pool).await);
    let token = mint("doc-joey-a");

    let (_, started) = post_empty(&app, SHIFTS, &token).await;
    let id = shift_id_of(&started);

    let (first_status, first) = post_empty(&app, &end_url(&id), &token).await;
    assert_eq!(first_status, 200);
    assert_eq!(first["status"], "ended");
    assert!(!first["ended_at"].is_null(), "ended_at is stamped");

    let (second_status, second) = post_empty(&app, &end_url(&id), &token).await;
    assert_eq!(second_status, 200, "a retry succeeds");
    assert_eq!(
        second, first,
        "and returns the identical body — ended_at must not move"
    );
}

/// Once ended, the shift stops being "what I'm in the middle of", and the
/// clinician is free to open a new one — the partial index only covers active rows.
#[sqlx::test]
async fn ending_clears_active_and_allows_a_new_shift(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let app = routes::router(test_state(pool).await);
    let token = mint("doc-joey-a");

    let (_, started) = post_empty(&app, SHIFTS, &token).await;
    let first_id = shift_id_of(&started);
    post_empty(&app, &end_url(&first_id), &token).await;

    assert_eq!(
        get(&app, ACTIVE, Some(&token)).await,
        404,
        "no shift in progress"
    );

    let (status, next) = post_empty(&app, SHIFTS, &token).await;
    assert_eq!(status, 200, "the index no longer blocks a new shift");
    assert_ne!(shift_id_of(&next), first_id, "and it is a different shift");

    // The ended shift is still fetchable by id — shifts are durable records,
    // not transient session state.
    let (old_status, old) = get_json(&app, &shift_url(&first_id), &token).await;
    assert_eq!(old_status, 200);
    assert_eq!(old["status"], "ended");
}

#[sqlx::test]
async fn ending_an_unknown_shift_is_404(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let app = routes::router(test_state(pool).await);

    let ghost = Uuid::now_v7().to_string();
    let (status, _) = post_empty(&app, &end_url(&ghost), &mint("doc-joey-a")).await;
    assert_eq!(status, 404);
}

// ---------------------------------------------------------------------------
// Tenancy
// ---------------------------------------------------------------------------

/// A shift id is a bearer-ish opaque handle, so the org scoping has to hold even
/// when the caller knows the id exactly. Read and mutate are both checked: ending
/// someone else's shift would be the more damaging of the two.
#[sqlx::test]
async fn shifts_are_org_scoped(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    seed_org_and_clinician(&pool, ORG_B, "doc-peppas-b").await;
    let app = routes::router(test_state(pool).await);

    let (_, a_shift) = post_empty(&app, SHIFTS, &mint("doc-joey-a")).await;
    let a_id = shift_id_of(&a_shift);

    let intruder = mint("doc-peppas-b");
    assert_eq!(
        get(&app, &shift_url(&a_id), Some(&intruder)).await,
        404,
        "cannot read"
    );

    let (end_status, _) = post_empty(&app, &end_url(&a_id), &intruder).await;
    assert_eq!(end_status, 404, "cannot end");

    // Org B seeing no active shift of its own must not be confused with org A's.
    assert_eq!(get(&app, ACTIVE, Some(&intruder)).await, 404);

    // And org A's shift is untouched by the attempt.
    let (status, still) = get_json(&app, &shift_url(&a_id), &mint("doc-joey-a")).await;
    assert_eq!(status, 200);
    assert_eq!(
        still["status"], "active",
        "the intruder's end must not have landed"
    );
}
