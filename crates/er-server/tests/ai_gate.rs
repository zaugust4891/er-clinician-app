//! Phase 4 Step 7 server-owned AI acceptance gates.

use sqlx::PgPool;
use uuid::Uuid;

use er_ai::summarizer::{AppSummarizer, MockSummarizer};
use er_domain::{CaptureMode, Disposition, NoteEvent, NotePhase, NoteSections};
use er_pipeline::speech_to_text::{AppSpeechToText, MockSpeechToText};
use er_server::repository::PostgresRepository;
use er_server::{
    routes,
    worker::{self, WorkerCtx},
};
use er_store::repository::Repository;

mod common;
use common::{
    ORG_A, ORG_B, ensure_test_audio_infra, get_json, mint, post_json, seed_org_and_clinician,
    seed_registered_patient, test_state_with_audio,
};

fn mdm_url(patient_id: Uuid) -> String {
    format!("/v1/patients/{patient_id}/mdm")
}

#[sqlx::test(migrations = "./migrations")]
async fn interrupted_mdm_content_is_repaired_without_another_provider_call(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    seed_org_and_clinician(&pool, ORG_A, "mdm-recover").await;
    let patient_id = seed_registered_patient(&pool, ORG_A).await;
    let repo = PostgresRepository::for_org(pool.clone(), ORG_A);
    let encounter_id = Uuid::now_v7();
    for event in [
        NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient),
        NoteEvent::audio_stopped(patient_id, encounter_id),
    ] {
        repo.append_event(patient_id, Uuid::now_v7(), event)
            .await
            .unwrap();
    }
    let app = routes::router(test_state_with_audio(pool.clone(), &audio).await);
    let job_id = Uuid::now_v7();
    assert_eq!(
        post_json(
            &app,
            &mdm_url(patient_id),
            &mint("mdm-recover"),
            serde_json::json!({"job_id":job_id})
        )
        .await
        .0,
        202
    );
    repo.mark_job_running(job_id).await.unwrap();
    repo.append_event(
        patient_id,
        Uuid::now_v7(),
        NoteEvent::mdm_generated(
            patient_id,
            "synthetic result saved before interruption".into(),
            vec![encounter_id],
        ),
    )
    .await
    .unwrap();
    let ctx = WorkerCtx {
        pool: pool.clone(),
        s3: audio.s3.clone(),
        sqs: audio.sqs.clone(),
        bucket: audio.config.bucket.clone(),
        queue_url: audio.config.stt_queue_url.clone(),
        stt: AppSpeechToText::Mock(MockSpeechToText::new()),
        summarizer: AppSummarizer::Mock(MockSummarizer::new()),
        wait_time_seconds: 0,
        limits: er_server::config::WorkerLimits::default(),
    };
    assert_eq!(worker::poll_once(&ctx).await.unwrap(), 1);
    assert_eq!(repo.job_row(job_id).await.unwrap().unwrap().status, "done");
    let note = er_app::load_patient_note(&repo, patient_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(note.phase, NotePhase::Finalizing);
    assert_eq!(
        note.note_sections.mdm.as_deref(),
        Some("synthetic result saved before interruption")
    );
    let calls: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_log WHERE job_id=$1")
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        calls, 0,
        "repair does not regenerate or meter the saved content"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn final_job_deadline_marks_mdm_receipt_failed_before_dlq(pool: PgPool) {
    use std::time::Duration;
    let audio = ensure_test_audio_infra().await;
    seed_org_and_clinician(&pool, ORG_A, "mdm-timeout").await;
    let patient_id = seed_registered_patient(&pool, ORG_A).await;
    let app = routes::router(test_state_with_audio(pool.clone(), &audio).await);
    let job_id = Uuid::now_v7();
    assert_eq!(
        post_json(
            &app,
            &mdm_url(patient_id),
            &mint("mdm-timeout"),
            serde_json::json!({"job_id":job_id})
        )
        .await
        .0,
        202
    );
    let mut held = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("mdm:{patient_id}"))
        .execute(&mut *held)
        .await
        .unwrap();
    let ctx = WorkerCtx {
        pool: pool.clone(),
        s3: audio.s3.clone(),
        sqs: audio.sqs.clone(),
        bucket: audio.config.bucket.clone(),
        queue_url: audio.config.stt_queue_url.clone(),
        stt: AppSpeechToText::Mock(MockSpeechToText::new()),
        summarizer: AppSummarizer::Mock(MockSummarizer::new()),
        wait_time_seconds: 0,
        limits: er_server::config::WorkerLimits {
            visibility_seconds: 0,
            job_timeout: Duration::from_millis(800),
            terminal_timeout: Duration::from_millis(400),
            ..Default::default()
        },
    };
    for _ in 0..3 {
        assert_eq!(worker::poll_once(&ctx).await.unwrap(), 1);
    }
    let repo = PostgresRepository::for_org(pool.clone(), ORG_A);
    let row = repo.job_row(job_id).await.unwrap().unwrap();
    assert_eq!(row.status, "failed");
    assert_eq!(row.error.as_deref(), Some("job_deadline_exceeded"));
    assert_eq!(worker::poll_once(&ctx).await.unwrap(), 0);
    let dlq = audio
        .sqs
        .receive_message()
        .queue_url(&audio.config.stt_dlq_url)
        .wait_time_seconds(1)
        .send()
        .await
        .unwrap();
    assert_eq!(dlq.messages().len(), 1);
    held.rollback().await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn a_repeated_mdm_request_enqueues_once_and_jobs_are_org_scoped(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    seed_org_and_clinician(&pool, ORG_A, "mdm-owner").await;
    seed_org_and_clinician(&pool, ORG_B, "mdm-other").await;
    let patient_id = seed_registered_patient(&pool, ORG_A).await;
    let app = routes::router(test_state_with_audio(pool.clone(), &audio).await);
    let job_id = Uuid::now_v7();
    let request = serde_json::json!({ "job_id": job_id });

    let first = post_json(
        &app,
        &mdm_url(patient_id),
        &mint("mdm-owner"),
        request.clone(),
    )
    .await;
    let second = post_json(&app, &mdm_url(patient_id), &mint("mdm-owner"), request).await;
    assert_eq!(first.0, 202, "first request failed: {}", first.1);
    assert_eq!(second.0, 202, "retry failed: {}", second.1);
    assert_eq!(first.1["job_id"], job_id.to_string());
    assert_eq!(second.1["job_id"], job_id.to_string());

    let queued = audio.drain_jobs().await;
    assert_eq!(
        queued.len(),
        1,
        "the durable receipt must enqueue exactly once"
    );
    assert_eq!(
        queued[0],
        serde_json::json!({
            "kind": "GenerateMdm",
            "patient_id": patient_id,
            "job_id": job_id,
        })
    );

    let owner = get_json(&app, &format!("/v1/jobs/{job_id}"), &mint("mdm-owner")).await;
    assert_eq!(owner.0, 200);
    assert_eq!(owner.1["status"], er_api::job_status::QUEUED);
    let other = get_json(&app, &format!("/v1/jobs/{job_id}"), &mint("mdm-other")).await;
    assert_eq!(
        other.0, 404,
        "foreign job ids must not be an existence oracle"
    );
}

#[sqlx::test(migrations = "./migrations")]
async fn mdm_job_persists_content_before_state_and_records_usage(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    seed_org_and_clinician(&pool, ORG_A, "mdm-lifecycle").await;
    let patient_id = seed_registered_patient(&pool, ORG_A).await;
    let repo = PostgresRepository::for_org(pool.clone(), ORG_A);
    let encounter_id = Uuid::now_v7();

    for event in [
        NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient),
        NoteEvent::audio_stopped(patient_id, encounter_id),
        NoteEvent::transcript_ready(
            patient_id,
            encounter_id,
            "synthetic chest pressure".to_string(),
            chrono::Utc::now(),
        ),
        NoteEvent::encounter_summarized(
            patient_id,
            encounter_id,
            NoteSections {
                hpi: Some("Substernal pressure for two hours.".to_string()),
                ros: None,
                exam: Some("Cardiovascular: regular rhythm.".to_string()),
                mdm: None,
                disposition_text: None,
            },
            vec![encounter_id],
        ),
        NoteEvent::disposition_set(
            patient_id,
            Disposition::Discharge {
                instructions: "Return for worsening symptoms.".to_string(),
                follow_up: Some("Primary care in two days.".to_string()),
            },
        ),
    ] {
        repo.append_event(patient_id, Uuid::now_v7(), event)
            .await
            .unwrap();
    }

    let app = routes::router(test_state_with_audio(pool.clone(), &audio).await);
    let token = mint("mdm-lifecycle");
    let job_id = Uuid::now_v7();
    let accepted = post_json(
        &app,
        &mdm_url(patient_id),
        &token,
        serde_json::json!({ "job_id": job_id }),
    )
    .await;
    assert_eq!(accepted.0, 202);
    assert_eq!(accepted.1["status"], er_api::job_status::QUEUED);

    let ctx = WorkerCtx {
        pool: pool.clone(),
        s3: audio.s3.clone(),
        sqs: audio.sqs.clone(),
        bucket: audio.config.bucket.clone(),
        queue_url: audio.config.stt_queue_url.clone(),
        stt: AppSpeechToText::Mock(MockSpeechToText::new()),
        summarizer: AppSummarizer::Mock(MockSummarizer::new()),
        wait_time_seconds: 1,
        limits: er_server::config::WorkerLimits::default(),
    };
    assert_eq!(worker::poll_once(&ctx).await.unwrap(), 1);

    let status = get_json(&app, &format!("/v1/jobs/{job_id}"), &token).await;
    assert_eq!(status.0, 200);
    assert_eq!(status.1["status"], er_api::job_status::DONE);

    let events = repo.get_events(patient_id).await.unwrap();
    let mdm_index = events
        .iter()
        .position(|stored| matches!(stored.event, NoteEvent::MdmGenerated { .. }))
        .expect("generated MDM event");
    let trigger_index = events
        .iter()
        .position(|stored| matches!(stored.event, NoteEvent::TriggerMDM { .. }))
        .expect("MDM phase transition event");
    assert!(
        mdm_index < trigger_index,
        "content must be durable before state advances"
    );

    let note = er_app::load_patient_note(&repo, patient_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(note.phase, NotePhase::Finalizing);
    assert_eq!(
        note.note_sections.mdm.as_deref(),
        Some("[mock MDM: moderate complexity, 2 diagnoses considered]")
    );

    let usage = sqlx::query_as::<_, (String, Option<String>, Option<Uuid>, i64)>(
        "SELECT kind, model, job_id, cost_micros FROM usage_log WHERE org_id = $1",
    )
    .bind(ORG_A)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        usage,
        vec![("mdm".to_string(), Some("mock".to_string()), Some(job_id), 0)]
    );

    er_server::queue::send_job(
        &audio.sqs,
        &audio.config.stt_queue_url,
        &er_server::queue::Job::GenerateMdm { patient_id, job_id },
    )
    .await
    .unwrap();
    assert_eq!(worker::poll_once(&ctx).await.unwrap(), 1);
    assert_eq!(
        repo.get_events(patient_id).await.unwrap().len(),
        events.len()
    );
    let usage_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_log WHERE job_id = $1")
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(usage_count, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn empty_note_fails_the_job_without_calling_or_metering_the_model(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    seed_org_and_clinician(&pool, ORG_A, "mdm-empty").await;
    let patient_id = seed_registered_patient(&pool, ORG_A).await;
    let repo = PostgresRepository::for_org(pool.clone(), ORG_A);
    let encounter_id = Uuid::now_v7();
    repo.append_event(
        patient_id,
        Uuid::now_v7(),
        NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient),
    )
    .await
    .unwrap();
    repo.append_event(
        patient_id,
        Uuid::now_v7(),
        NoteEvent::audio_stopped(patient_id, encounter_id),
    )
    .await
    .unwrap();

    let app = routes::router(test_state_with_audio(pool.clone(), &audio).await);
    let token = mint("mdm-empty");
    let job_id = Uuid::now_v7();
    assert_eq!(
        post_json(
            &app,
            &mdm_url(patient_id),
            &token,
            serde_json::json!({ "job_id": job_id }),
        )
        .await
        .0,
        202
    );
    let ctx = WorkerCtx {
        pool: pool.clone(),
        s3: audio.s3.clone(),
        sqs: audio.sqs.clone(),
        bucket: audio.config.bucket.clone(),
        queue_url: audio.config.stt_queue_url.clone(),
        stt: AppSpeechToText::Mock(MockSpeechToText::new()),
        summarizer: AppSummarizer::Mock(MockSummarizer::new()),
        wait_time_seconds: 1,
        limits: er_server::config::WorkerLimits::default(),
    };
    assert_eq!(worker::poll_once(&ctx).await.unwrap(), 1);

    let status = get_json(&app, &format!("/v1/jobs/{job_id}"), &token).await;
    assert_eq!(status.1["status"], er_api::job_status::FAILED);
    assert_eq!(status.1["error"], "nothing_to_generate_from");
    let usage_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM usage_log WHERE job_id = $1")
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(usage_count, 0);
    assert!(
        repo.get_events(patient_id)
            .await
            .unwrap()
            .iter()
            .all(|stored| !matches!(stored.event, NoteEvent::MdmGenerated { .. }))
    );
}
