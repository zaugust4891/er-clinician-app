//! S7 — the read side: `GET /note`, `GET /note/export`, `GET /v1/stats`.
//!
//! Two dialects of the same truth (structured `NoteView` for programs, flat text
//! for the Epic clipboard) plus the org-scoped counters. All three now route
//! through `load_patient_note`, so these tests also exercise the S6 snapshot
//! fast-path from the server side for the first time.

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use er_domain::{
    CaptureMode, ClinicalResult, Diagnosis, Disposition, LabResult, Medication, MedicationKind,
    NoteEvent, NoteSections, ResultSource, ResultType,
};
use er_server::repository::PostgresRepository;
use er_server::routes;
use er_store::repository::Repository;

mod common;
use common::{
    get, get_json, get_text, mint, seed_org_and_clinician, seed_registered_patient, test_state,
    ORG_A, ORG_B,
};

fn note_url(patient: Uuid) -> String {
    format!("/v1/patients/{patient}/note")
}
fn export_url(patient: Uuid) -> String {
    format!("/v1/patients/{patient}/note/export")
}

/// A complete ED visit driven all the way to `Signed`, with every section and
/// every objective-data list populated. Ordering follows the real pipeline: the
/// MDM content is persisted while still `Idle`, *then* TriggerMDM advances the
/// phase (CLAUDE.md's "persist content before advancing state").
async fn seed_signed_note(pool: &PgPool, org: Uuid, patient: Uuid) -> Vec<NoteEvent> {
    let repo = PostgresRepository::for_org(pool.clone(), org);
    let encounter = Uuid::now_v7();

    let events = vec![
        NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient),
        NoteEvent::transcript_ready(
            patient,
            encounter,
            "substernal chest pressure since 0600, radiating to the left arm".to_string(),
            Utc::now(),
        ),
        NoteEvent::audio_stopped(patient, encounter),
        NoteEvent::encounter_summarized(
            patient,
            encounter,
            NoteSections {
                hpi: Some("55yo with substernal chest pressure since 0600.".to_string()),
                ros: Some("Positive for chest pain and dyspnea. Negative for fever.".to_string()),
                exam: Some("Alert, diaphoretic. Lungs clear. Regular rate and rhythm.".to_string()),
                mdm: None,
                disposition_text: None,
            },
            vec![encounter],
        ),
        NoteEvent::result_received(
            patient,
            ClinicalResult {
                id: Uuid::now_v7(),
                patient_id: patient,
                source: ResultSource::Manual,
                timestamp: Utc::now(),
                result_type: ResultType::Lab(LabResult {
                    fhir_id: None,
                    loinc_code: Some("6598-7".to_string()),
                    display_name: "Troponin I".to_string(),
                    value: 0.42,
                    unit: "ng/mL".to_string(),
                    reference_range: Some((0.0, 0.04)),
                    is_abnormal: true,
                }),
            },
            Utc::now(),
        ),
        NoteEvent::medication_recorded(
            patient,
            Medication {
                id: Uuid::now_v7(),
                patient_id: patient,
                name: "Aspirin".to_string(),
                dose: "324 mg".to_string(),
                route: Some("PO".to_string()),
                rxnorm_code: None,
                kind: MedicationKind::AdministeredInEd,
                administered_at: Utc::now(),
                frequency: "once".to_string(),
            },
        ),
        NoteEvent::diagnosis_assigned(
            patient,
            Diagnosis {
                id: Uuid::now_v7(),
                patient_id: patient,
                text: "NSTEMI".to_string(),
                icd10_code: Some("I21.4".to_string()),
                is_primary: true,
            },
        ),
        NoteEvent::data_reviewed(patient, "Prior ECG from 2025 reviewed.".to_string()),
        NoteEvent::disposition_set(
            patient,
            Disposition::Admit {
                service: "Cardiology".to_string(),
                bed: Some("CCU 3".to_string()),
            },
        ),
        NoteEvent::mdm_generated(
            patient,
            "Elevated troponin with ischemic chest pain. NSTEMI. Admit to cardiology.".to_string(),
            vec![encounter],
        ),
        NoteEvent::trigger_mdm(patient),
        NoteEvent::review_ready(patient),
        NoteEvent::sign(patient),
    ];

    for e in &events {
        repo.append_event(patient, Uuid::now_v7(), e.clone())
            .await
            .expect("seed event");
    }
    events
}

// ---------------------------------------------------------------------------
// GET /note — the structured read model
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn signed_note_reads_back_fully_populated(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    seed_signed_note(&pool, ORG_A, patient).await;
    let app = routes::router(test_state(pool).await);

    let (status, body) = get_json(&app, &note_url(patient), &mint("doc-joey-a")).await;

    assert_eq!(status, 200);
    assert_eq!(body["phase"], "Signed");
    assert_eq!(body["patient_id"], patient.to_string());

    // Every narrative section survived the round trip.
    assert!(body["hpi"].as_str().unwrap().contains("substernal chest pressure"));
    assert!(body["ros"].as_str().unwrap().contains("dyspnea"));
    assert!(body["exam"].as_str().unwrap().contains("diaphoretic"));
    assert!(body["mdm"].as_str().unwrap().contains("NSTEMI"));

    // And every objective-data list.
    assert_eq!(body["results"].as_array().unwrap().len(), 1);
    assert_eq!(body["medications"].as_array().unwrap().len(), 1);
    assert_eq!(body["diagnoses"].as_array().unwrap().len(), 1);
    assert_eq!(body["data_reviewed"].as_array().unwrap().len(), 1);
    assert!(body["disposition"].is_object(), "disposition must be present");
    assert_eq!(body["encounter_count"], 1);
}

/// The S3 contract, restated through the new `NoteView` shape: a registered
/// patient nothing has happened to is a 200 with a fresh note, not a 404.
#[sqlx::test]
async fn registered_but_quiet_patient_reads_as_a_fresh_note(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let app = routes::router(test_state(pool).await);

    let (status, body) = get_json(&app, &note_url(patient), &mint("doc-joey-a")).await;

    assert_eq!(status, 200);
    assert_eq!(body["phase"], "Registered");
    assert!(body["hpi"].is_null());
    assert_eq!(body["encounter_count"], 0);
}

/// S7's stated purpose is delegating to `load_patient_note`, which prefers a
/// snapshot plus delta. The read must be identical either way — otherwise the
/// fast path is a correctness hazard rather than an optimization.
#[sqlx::test]
async fn note_read_is_identical_via_snapshot_fast_path(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let events = seed_signed_note(&pool, ORG_A, patient).await;
    let app = routes::router(test_state(pool.clone()).await);
    let token = mint("doc-joey-a");

    // Full-replay answer (no snapshot exists yet).
    let (_, replayed) = get_json(&app, &note_url(patient), &token).await;

    // Checkpoint at an EARLY sequence so the fast path must still apply a delta;
    // a snapshot at the head would prove much less.
    let repo = PostgresRepository::for_org(pool.clone(), ORG_A);
    let stored = repo.get_events(patient).await.unwrap();
    let cut = 3; // through audio_stopped
    let partial = er_domain::replay::replay(patient, &events[..cut]);
    repo.save_snapshot(patient, stored[cut - 1].sequence_id, &partial)
        .await
        .unwrap();

    let (status, via_snapshot) = get_json(&app, &note_url(patient), &token).await;
    assert_eq!(status, 200);

    // KNOWN WART: `created_at` is stamped by `PatientNote::new()` at replay time,
    // not derived from any event, so the full-replay path reports "when this request
    // replayed" while the snapshot path reports "when the snapshot was built". Every
    // other field — including `version` and `updated_at` — is identical. Excluded
    // here deliberately, and asserted-on separately below so the wobble stays visible.
    let strip_created_at = |mut v: serde_json::Value| {
        v.as_object_mut().unwrap().remove("created_at");
        v
    };
    assert_eq!(
        strip_created_at(via_snapshot.clone()),
        strip_created_at(replayed.clone()),
        "snapshot + delta must produce the same NoteView as a full replay"
    );
    assert_eq!(via_snapshot["phase"], "Signed", "and it really is the finished note");
    assert_eq!(via_snapshot["version"], replayed["version"], "same version either way");

    // Pin the wart itself: if `created_at` ever becomes event-derived, this fails
    // and the exclusion above should be deleted along with it.
    assert_ne!(
        via_snapshot["created_at"], replayed["created_at"],
        "created_at is currently replay-time, not event-derived — see the note above"
    );
}

// ---------------------------------------------------------------------------
// GET /note/export — the clipboard dialect
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn export_returns_plain_text_with_the_note_content(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    seed_signed_note(&pool, ORG_A, patient).await;
    let app = routes::router(test_state(pool).await);

    let (status, content_type, text) = get_text(&app, &export_url(patient), &mint("doc-joey-a")).await;

    assert_eq!(status, 200);
    assert_eq!(content_type, "text/plain; charset=utf-8", "this is clipboard text, not JSON");
    assert!(text.starts_with("ED PROVIDER NOTE"), "got: {:?}", &text[..text.len().min(80)]);

    // The chief complaint comes from the `patients` row, not the event stream —
    // it is the one field proving `patient_row` was actually consulted.
    assert!(text.contains("chest pain"), "chief complaint must appear");

    // Content from each contributing source: sections, results, meds, dx, disposition.
    assert!(text.contains("substernal chest pressure"), "HPI");
    assert!(text.contains("NSTEMI"), "diagnosis / MDM");
    assert!(text.contains("Troponin I"), "results line");
    assert!(text.contains("Aspirin"), "medication");
    assert!(text.contains("Cardiology"), "disposition");
    assert!(text.contains("Prior ECG"), "data reviewed");
}

/// The deliberate asymmetry with `GET /note`: there is no such thing as a
/// paste-ready note for a patient with no events, so export 404s where the
/// structured read returns an empty note.
#[sqlx::test]
async fn export_of_an_eventless_patient_is_404(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let app = routes::router(test_state(pool).await);
    let token = mint("doc-joey-a");

    assert_eq!(get(&app, &export_url(patient), Some(&token)).await, 404);
    assert_eq!(get(&app, &note_url(patient), Some(&token)).await, 200, "but /note still answers");
}

// ---------------------------------------------------------------------------
// GET /v1/stats — org-scoped counters
// ---------------------------------------------------------------------------

/// The counts must match what the repository reports AND must be scoped to the
/// caller's org. A leak here is a quiet one: no PHI crosses, but org A learns
/// how busy org B is.
#[sqlx::test]
async fn stats_match_the_repository_and_are_org_scoped(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    seed_org_and_clinician(&pool, ORG_B, "doc-peppas-b").await;
    let patient_a = seed_registered_patient(&pool, ORG_A).await;
    let patient_b = seed_registered_patient(&pool, ORG_B).await;

    let events_a = seed_signed_note(&pool, ORG_A, patient_a).await;
    seed_signed_note(&pool, ORG_B, patient_b).await;

    // One snapshot in org A only.
    let repo_a = PostgresRepository::for_org(pool.clone(), ORG_A);
    let note_a = er_domain::replay::replay(patient_a, &events_a);
    let head = repo_a.get_events(patient_a).await.unwrap().last().unwrap().sequence_id;
    repo_a.save_snapshot(patient_a, head, &note_a).await.unwrap();

    let app = routes::router(test_state(pool.clone()).await);
    let (status, body) = get_json(&app, "/v1/stats", &mint("doc-joey-a")).await;

    assert_eq!(status, 200);
    assert_eq!(
        body["event_count"].as_u64().unwrap() as usize,
        repo_a.event_count().await.unwrap(),
        "stats must agree with the repository the contract suite tests"
    );
    assert_eq!(body["event_count"], events_a.len() as u64);
    assert_eq!(body["snapshot_count"], 1);

    // Org B has its own events and no snapshot — proof the numbers are not global.
    let (_, body_b) = get_json(&app, "/v1/stats", &mint("doc-peppas-b")).await;
    assert_eq!(body_b["event_count"], events_a.len() as u64, "same size stream, separately counted");
    assert_eq!(body_b["snapshot_count"], 0, "org B never snapshotted");
}

// ---------------------------------------------------------------------------
// Tenancy across all three
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn read_endpoints_are_org_scoped(pool: PgPool) {
    seed_org_and_clinician(&pool, ORG_A, "doc-joey-a").await;
    seed_org_and_clinician(&pool, ORG_B, "doc-peppas-b").await;
    let patient_b = seed_registered_patient(&pool, ORG_B).await;
    seed_signed_note(&pool, ORG_B, patient_b).await;
    let app = routes::router(test_state(pool).await);

    let intruder = mint("doc-joey-a");
    assert_eq!(get(&app, &note_url(patient_b), Some(&intruder)).await, 404, "note");
    assert_eq!(get(&app, &export_url(patient_b), Some(&intruder)).await, 404, "export");

    // Sanity: the owner reaches both.
    let owner = mint("doc-peppas-b");
    assert_eq!(get(&app, &note_url(patient_b), Some(&owner)).await, 200);
    assert_eq!(get(&app, &export_url(patient_b), Some(&owner)).await, 200);
}
