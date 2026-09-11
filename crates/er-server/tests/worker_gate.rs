//! Step 6 S7 — the transcription worker, driven one cycle at a time.
//!
//! Where `audio_complete_gate.rs` stops at "a job reached the queue", these tests
//! pick that job up. They drive the real [`er_server::worker::poll_once`] against
//! real MinIO, real ElasticMQ and real Postgres, with the STT seam swapped for
//! `MockSpeechToText` in **echo** mode — which is what makes the assertions exact
//! rather than approximate: chunk 1's audio literally becomes chunk 1's words, so
//! index ordering and attribution are observable in a single `assert_eq!` (S5's
//! reason for building echo mode at all).
//!
//! `poll_once` returns a count, so a test drains the queue deterministically
//! (`while poll_once(&ctx).await? > 0 {}`) instead of spawning `er-worker` and
//! racing its long poll.
//!
//! Requires the compose stack (`docker compose up -d`) and the dev AWS_*
//! variables, the same standing requirement the other audio gates impose. Each
//! test gets its own bucket and queue pair.

use sqlx::PgPool;
use uuid::Uuid;

use er_api::client::{RemoteRepository, ServerApi};
use er_domain::{CaptureMode, NoteEvent};
use er_pipeline::speech_to_text::{AppSpeechToText, MockSpeechToText};
use er_ai::summarizer::{AppSummarizer, MockSummarizer};
use er_server::queue::{self, Job};
use er_server::repository::{ChunkRow, NewChunk, PostgresRepository};
use er_server::{
    routes,
    worker::{self, WorkerCtx},
};
use er_store::repository::Repository;

mod common;
use common::{
    AudioFixture, ORG_A, ensure_test_audio_infra, mint, seed_org_and_clinician,
    seed_registered_patient, spawn_server, test_state_with_audio, tiny_wav,
};

/// A worker wired to this test's infrastructure, echoing whatever it is fed.
fn echo_worker(pool: PgPool, audio: &AudioFixture) -> WorkerCtx {
    WorkerCtx {
        pool,
        s3: audio.s3.clone(),
        sqs: audio.sqs.clone(),
        bucket: audio.config.bucket.clone(),
        queue_url: audio.config.stt_queue_url.clone(),
        stt: AppSpeechToText::Mock(MockSpeechToText { echo: true }),
        summarizer: AppSummarizer::Mock(MockSummarizer::new()),
        // 0 keeps the suite snappy; the binary uses 20 (§C2).
        wait_time_seconds: 0,
    }
}

/// Insert the ledger row and PUT its bytes, exactly as S3 + S4 would have.
///
/// Deliberately not going through the HTTP handlers: these tests are about the
/// consumer half, and building the precondition directly keeps a worker failure
/// from being reported as an upload failure.
async fn upload_chunk(
    pool: &PgPool,
    audio: &AudioFixture,
    patient_id: Uuid,
    encounter_id: Uuid,
    chunk_index: i32,
    is_final: bool,
    spoken: &[u8],
) -> Uuid {
    let chunk_id = Uuid::now_v7();
    let s3_key = format!("org/{ORG_A}/enc/{encounter_id}/{chunk_index}.wav");
    let repo = PostgresRepository::for_org(pool.clone(), ORG_A);

    repo.insert_chunk(NewChunk {
        id: chunk_id,
        patient_id,
        encounter_id,
        chunk_index,
        is_final,
        content_type: "audio/wav",
        s3_key: &s3_key,
    })
    .await
    .expect("insert the chunk row");

    audio
        .s3
        .put_object()
        .bucket(&audio.config.bucket)
        .key(&s3_key)
        .content_type("audio/wav")
        .body(tiny_wav(spoken).into())
        .send()
        .await
        .expect("put the chunk's audio");

    repo.mark_chunk_uploaded(chunk_id)
        .await
        .expect("mark uploaded")
        .expect("the chunk exists");

    chunk_id
}

async fn enqueue(audio: &AudioFixture, chunk_id: Uuid) {
    queue::send_job(
        &audio.sqs,
        &audio.config.stt_queue_url,
        &Job::TranscribeChunk { chunk_id },
    )
    .await
    .expect("enqueue a transcription job");
}

/// Drain the queue, returning how many messages the worker handled in total.
async fn drain(ctx: &WorkerCtx) -> usize {
    let mut total = 0;
    loop {
        let handled = worker::poll_once(ctx).await.expect("a poll cycle");
        if handled == 0 {
            return total;
        }
        total += handled;
    }
}

async fn chunk(pool: &PgPool, chunk_id: Uuid) -> ChunkRow {
    PostgresRepository::for_org(pool.clone(), ORG_A)
        .chunk_row(chunk_id)
        .await
        .expect("read the chunk row")
        .expect("the chunk exists")
}

/// Every `TranscriptReady` on a patient's stream as `(encounter_id, text)`.
///
/// Read as the typed enum rather than as JSON: `NoteEvent` is an externally
/// tagged enum, so a JSON probe would have to spell `["TranscriptReady"]["text"]`
/// and would silently return `null` — a passing-looking test — if the
/// representation ever changed.
async fn transcript_events(pool: &PgPool, patient_id: Uuid) -> Vec<(Uuid, String)> {
    PostgresRepository::for_org(pool.clone(), ORG_A)
        .get_events(patient_id)
        .await
        .expect("read the event stream")
        .into_iter()
        .filter_map(|stored| match stored.event {
            NoteEvent::TranscriptReady {
                encounter_id, text, ..
            } => Some((encounter_id, text)),
            _ => None,
        })
        .collect()
}

/// How many messages the queue is holding, visible or leased to a consumer.
///
/// The only way to observe "the worker did not delete it" quickly: a received but
/// undeleted message is *invisible*, not gone, so a plain receive would report an
/// empty queue for the length of the visibility timeout and prove nothing.
async fn queue_depth(audio: &AudioFixture) -> i64 {
    use aws_sdk_sqs::types::QueueAttributeName;

    let attributes = audio
        .sqs
        .get_queue_attributes()
        .queue_url(&audio.config.stt_queue_url)
        .attribute_names(QueueAttributeName::ApproximateNumberOfMessages)
        .attribute_names(QueueAttributeName::ApproximateNumberOfMessagesNotVisible)
        .send()
        .await
        .expect("read the queue attributes");

    let read = |name: QueueAttributeName| -> i64 {
        attributes
            .attributes()
            .and_then(|a| a.get(&name))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };

    read(QueueAttributeName::ApproximateNumberOfMessages)
        + read(QueueAttributeName::ApproximateNumberOfMessagesNotVisible)
}

/// Read one poison job after the main queue's redrive policy moves it.
async fn receive_dlq_job(audio: &AudioFixture) -> Option<serde_json::Value> {
    let received = audio
        .sqs
        .receive_message()
        .queue_url(&audio.config.stt_dlq_url)
        .max_number_of_messages(1)
        .wait_time_seconds(1)
        .send()
        .await
        .expect("read the test DLQ");
    let message = received.messages().first()?;
    let body = serde_json::from_str(message.body().unwrap_or_default())
        .expect("the dead-lettered job remains valid JSON");
    if let Some(receipt) = message.receipt_handle() {
        audio
            .sqs
            .delete_message()
            .queue_url(&audio.config.stt_dlq_url)
            .receipt_handle(receipt)
            .send()
            .await
            .expect("delete the observed DLQ job");
    }
    Some(body)
}

/// Make failed messages reappear immediately so a test can walk the three-strike
/// path in milliseconds instead of waiting out three 300 s visibility timeouts.
///
/// This is the honest way to drive the retry logic: the *same* message comes back
/// with `ApproximateReceiveCount` incrementing 1 → 2 → 3, which is exactly what
/// production does, only slower. Re-sending a fresh message would not work — each
/// send is a new message whose receive count starts at 1, so the worker would
/// never see a final delivery.
async fn make_retries_immediate(audio: &AudioFixture) {
    use aws_sdk_sqs::types::QueueAttributeName;

    audio
        .sqs
        .set_queue_attributes()
        .queue_url(&audio.config.stt_queue_url)
        .attributes(QueueAttributeName::VisibilityTimeout, "0")
        .send()
        .await
        .expect("shorten the visibility timeout for this test");
}

async fn seed(pool: &PgPool) -> (Uuid, Uuid) {
    seed_org_and_clinician(pool, ORG_A, "worker-sub").await;
    let patient_id = seed_registered_patient(pool, ORG_A).await;
    let encounter_id = Uuid::now_v7();
    PostgresRepository::for_org(pool.clone(), ORG_A)
        .append_event(
            patient_id,
            Uuid::now_v7(),
            NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient),
        )
        .await
        .expect("seed AudioStarted before a worker-authored transcript");
    (patient_id, encounter_id)
}

/// The literal Step 6 gate through the same client methods the CLI uses:
/// authenticated intent, direct presigned PUT, completion, queue worker, cursor
/// replay, and a transcript on the encounter.
#[sqlx::test(migrations = "./migrations")]
async fn a_wav_uploaded_through_the_client_lands_on_the_replayed_note(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    seed_org_and_clinician(&pool, ORG_A, "worker-e2e").await;
    let token = mint("worker-e2e");
    let base = spawn_server(routes::router(
        test_state_with_audio(pool.clone(), &audio).await,
    ))
    .await;
    let api = ServerApi::new(&base, &token);
    let remote = RemoteRepository::new(&base, &token);

    let patient_id = Uuid::now_v7();
    api.register_patient(&er_api::RegisterPatientRequest {
        patient_id,
        chief_complaint: "synthetic chest pain".to_string(),
        display_label: Some("Test Room".to_string()),
    })
    .await
    .expect("register through ServerApi");

    let encounter_id = Uuid::now_v7();
    remote
        .append_event(
            patient_id,
            Uuid::now_v7(),
            NoteEvent::audio_started(patient_id, encounter_id, CaptureMode::Ambient),
        )
        .await
        .expect("publish AudioStarted through the remote repository");

    let chunk_id = Uuid::now_v7();
    let intent = api
        .request_audio_upload(
            patient_id,
            encounter_id,
            &er_api::AudioUploadRequest {
                chunk_id,
                chunk_index: 0,
                is_final: true,
                content_type: "audio/wav".to_string(),
            },
        )
        .await
        .expect("request an upload intent through ServerApi");
    api.put_presigned(
        &intent.upload_url,
        tiny_wav(b"synthetic substernal pressure"),
        "audio/wav",
    )
    .await
    .expect("PUT through the presigned URL");
    let completed = api
        .complete_audio(chunk_id)
        .await
        .expect("complete and enqueue through ServerApi");
    assert_eq!(completed.status, er_api::chunk_status::UPLOADED);

    // A device may stop before the async worker gets the message. The worker's
    // TranscriptReady must follow AudioStarted in the stream; it does not depend
    // on whether AudioStopped has already arrived.
    remote
        .append_event(
            patient_id,
            Uuid::now_v7(),
            NoteEvent::audio_stopped(patient_id, encounter_id),
        )
        .await
        .expect("publish AudioStopped through the remote repository");

    let ctx = echo_worker(pool.clone(), &audio);
    assert_eq!(drain(&ctx).await, 1);
    let finished = api.audio_chunk(chunk_id).await.expect("read final status");
    assert_eq!(finished.status, er_api::chunk_status::DONE);
    assert_eq!(finished.duration_ms, Some(30_000));

    let note = er_app::load::load_patient_note(&remote, patient_id)
        .await
        .expect("replay through RemoteRepository")
        .expect("the event stream creates a note");
    assert_eq!(note.phase, er_domain::NotePhase::Idle);
    let encounter = note
        .encounters
        .iter()
        .find(|encounter| encounter.id == encounter_id)
        .expect("the encounter survives replay");
    assert_eq!(
        encounter.transcript.as_deref(),
        Some("synthetic substernal pressure")
    );

    let events = remote.get_events(patient_id).await.expect("read event ids");
    let transcript = events
        .iter()
        .find(|stored| matches!(stored.event, NoteEvent::TranscriptReady { .. }))
        .expect("one worker-authored transcript event");
    assert_eq!(
        transcript.event_id,
        worker::transcript_event_id(encounter_id)
    );
    assert_eq!(
        events
            .iter()
            .filter(|stored| matches!(stored.event, NoteEvent::TranscriptReady { .. }))
            .count(),
        1
    );
    let transcript_position = events
        .iter()
        .position(|stored| matches!(stored.event, NoteEvent::TranscriptReady { .. }))
        .unwrap();
    let summary_position = events
        .iter()
        .position(|stored| matches!(stored.event, NoteEvent::EncounterSummarized { .. }))
        .expect("the worker publishes a summary");
    assert!(
        transcript_position < summary_position,
        "the durable source transcript must precede generated prose"
    );
    assert_eq!(
        note.note_sections.hpi.as_deref(),
        Some("[mock HPI from encounter transcript]")
    );

    let usage = sqlx::query_as::<_, (String, Option<String>, i64)>(
        "SELECT kind, model, cost_micros FROM usage_log WHERE org_id = $1",
    )
    .bind(ORG_A)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(usage, vec![("summarize".into(), Some("mock".into()), 0)]);
}

#[sqlx::test(migrations = "./migrations")]
async fn the_transcript_is_scrubbed_before_the_model_sees_it(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    let (patient_id, encounter_id) = seed(&pool).await;
    let mock = MockSummarizer::new();
    let captured = mock.captured_transcripts();
    let ctx = WorkerCtx {
        pool: pool.clone(),
        s3: audio.s3.clone(),
        sqs: audio.sqs.clone(),
        bucket: audio.config.bucket.clone(),
        queue_url: audio.config.stt_queue_url.clone(),
        stt: AppSpeechToText::Mock(MockSpeechToText { echo: true }),
        summarizer: AppSummarizer::Mock(mock),
        wait_time_seconds: 0,
    };
    let raw = b"Mrs. Alvarez MRN 12345678 SSN 111-22-3333 phone 510-555-1234";
    let chunk_id = upload_chunk(
        &pool,
        &audio,
        patient_id,
        encounter_id,
        0,
        true,
        raw,
    )
    .await;
    enqueue(&audio, chunk_id).await;
    assert_eq!(drain(&ctx).await, 1);

    let seen = captured.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(!seen[0].contains("12345678"));
    assert!(!seen[0].contains("111-22-3333"));
    assert!(!seen[0].contains("510-555-1234"));
    assert!(seen[0].contains("[MRN]"));
    assert!(seen[0].contains("[SSN]"));
    assert!(seen[0].contains("[PHONE]"));
    // The server stores no name field, so regex-only scrubbing cannot honestly
    // promise to remove a spoken surname. The provider BAA is the pilot control.
    assert!(seen[0].contains("Mrs. Alvarez"));
}

/// The S7 gate in one test: three chunks uploaded **out of order**, one job each,
/// and the encounter assembles into one event whose text is exact.
#[sqlx::test(migrations = "./migrations")]
async fn an_encounter_assembles_in_index_order_after_arriving_out_of_order(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    let (patient_id, encounter_id) = seed(&pool).await;

    // Index 2 is final and uploaded FIRST — the case that fails if assembly ever
    // sorts by arrival time, created_at, or chunk id instead of chunk_index (§C3).
    let two = upload_chunk(
        &pool,
        &audio,
        patient_id,
        encounter_id,
        2,
        true,
        b"segment two.",
    )
    .await;
    let zero = upload_chunk(
        &pool,
        &audio,
        patient_id,
        encounter_id,
        0,
        false,
        b"segment zero.",
    )
    .await;
    let one = upload_chunk(
        &pool,
        &audio,
        patient_id,
        encounter_id,
        1,
        false,
        b"segment one.",
    )
    .await;

    for id in [two, zero, one] {
        enqueue(&audio, id).await;
    }

    let ctx = echo_worker(pool.clone(), &audio);
    assert_eq!(drain(&ctx).await, 3, "one message handled per chunk");

    for id in [zero, one, two] {
        assert_eq!(chunk(&pool, id).await.status, "done");
    }
    // The mock reports a fixed 30 s, so a real number in this column proves the
    // worker stored the STT's duration rather than a default.
    assert_eq!(chunk(&pool, one).await.duration_ms, Some(30_000));

    let events = transcript_events(&pool, patient_id).await;
    assert_eq!(events.len(), 1, "one encounter, one TranscriptReady");
    assert_eq!(events[0].0, encounter_id);
    assert_eq!(events[0].1, "segment zero.\n\nsegment one.\n\nsegment two.");
}

/// Assembly must not fire while the encounter is still recording, and must not
/// fire twice when it finishes.
#[sqlx::test(migrations = "./migrations")]
async fn no_event_is_published_until_the_final_chunk_is_terminal(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    let (patient_id, encounter_id) = seed(&pool).await;
    let ctx = echo_worker(pool.clone(), &audio);

    let zero = upload_chunk(
        &pool,
        &audio,
        patient_id,
        encounter_id,
        0,
        false,
        b"still going.",
    )
    .await;
    enqueue(&audio, zero).await;
    drain(&ctx).await;

    assert_eq!(chunk(&pool, zero).await.status, "done");
    assert!(
        transcript_events(&pool, patient_id).await.is_empty(),
        "nobody has claimed finality, so the encounter is still open"
    );

    let one = upload_chunk(
        &pool,
        &audio,
        patient_id,
        encounter_id,
        1,
        true,
        b"all done.",
    )
    .await;
    enqueue(&audio, one).await;
    drain(&ctx).await;

    let events = transcript_events(&pool, patient_id).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].1, "still going.\n\nall done.");
}

/// §C4's whole point: the same job twice changes nothing. The event id is derived
/// from the encounter, so the second assembly collides on `event_id` and becomes a
/// read of the first one's `sequence_id`.
#[sqlx::test(migrations = "./migrations")]
async fn a_replayed_job_changes_nothing(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    let (patient_id, encounter_id) = seed(&pool).await;
    let ctx = echo_worker(pool.clone(), &audio);

    let only = upload_chunk(
        &pool,
        &audio,
        patient_id,
        encounter_id,
        0,
        true,
        b"only one.",
    )
    .await;
    enqueue(&audio, only).await;
    drain(&ctx).await;

    let before = transcript_events(&pool, patient_id).await;
    assert_eq!(before.len(), 1);

    // Exactly what an at-least-once queue does on a bad day: deliver it again.
    enqueue(&audio, only).await;
    assert_eq!(drain(&ctx).await, 1, "the duplicate was received");

    let after = transcript_events(&pool, patient_id).await;
    assert_eq!(after, before, "no second event, and no changed text");
    assert_eq!(
        chunk(&pool, only).await.transcript.as_deref(),
        Some("only one."),
        "a duplicate must not blank an already-stored transcript"
    );
    let all = PostgresRepository::for_org(pool.clone(), ORG_A)
        .get_events(patient_id)
        .await
        .unwrap();
    assert_eq!(
        all.iter()
            .filter(|stored| matches!(stored.event, NoteEvent::EncounterSummarized { .. }))
            .count(),
        1,
        "duplicate deliveries must not call or persist a second summary"
    );
    let metered: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM usage_log WHERE org_id = $1 AND kind = 'summarize'")
            .bind(ORG_A)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(metered, 1);
}

/// The event id is computable from outside the worker — which is what makes the
/// idempotency auditable rather than merely asserted.
#[sqlx::test(migrations = "./migrations")]
async fn the_published_event_id_is_the_uuidv5_of_the_encounter(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    let (patient_id, encounter_id) = seed(&pool).await;
    let ctx = echo_worker(pool.clone(), &audio);

    let only = upload_chunk(
        &pool,
        &audio,
        patient_id,
        encounter_id,
        0,
        true,
        b"one segment.",
    )
    .await;
    enqueue(&audio, only).await;
    drain(&ctx).await;

    let stored: Uuid =
        sqlx::query_scalar("SELECT event_id FROM events WHERE event_type = 'TranscriptReady'")
            .fetch_one(&pool)
            .await
            .expect("the event was written");

    assert_eq!(stored, worker::transcript_event_id(encounter_id));
}

/// A chunk whose object was never uploaded fails every delivery. On the last one
/// the worker records the autopsy and salvages the rest of the encounter — the
/// note outranks the chunk (D6.14).
#[sqlx::test(migrations = "./migrations")]
async fn a_chunk_whose_audio_is_missing_becomes_a_visible_gap(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    let (patient_id, encounter_id) = seed(&pool).await;
    let ctx = echo_worker(pool.clone(), &audio);

    let zero = upload_chunk(
        &pool,
        &audio,
        patient_id,
        encounter_id,
        0,
        false,
        b"good audio.",
    )
    .await;

    // Row and status say uploaded; the object is not there. This is the shape of
    // a device that crashed between the PUT and the completion call, and of a
    // bucket lifecycle that expired an object early.
    let missing = Uuid::now_v7();
    PostgresRepository::for_org(pool.clone(), ORG_A)
        .insert_chunk(NewChunk {
            id: missing,
            patient_id,
            encounter_id,
            chunk_index: 1,
            is_final: true,
            content_type: "audio/wav",
            s3_key: "org/does/not/exist.wav",
        })
        .await
        .expect("insert the ghost chunk");
    PostgresRepository::for_org(pool.clone(), ORG_A)
        .mark_chunk_uploaded(missing)
        .await
        .expect("mark uploaded")
        .expect("the chunk exists");

    // The healthy chunk first, on the normal settings.
    enqueue(&audio, zero).await;
    drain(&ctx).await;
    assert_eq!(chunk(&pool, zero).await.status, "done");
    assert!(
        transcript_events(&pool, patient_id).await.is_empty(),
        "index 1 has not finished, so there is nothing to assemble yet"
    );

    // Now the doomed one. Zero visibility means each failed delivery reappears
    // instantly, so the SAME message comes back with ApproximateReceiveCount
    // 1 → 2 → 3 — the real retry path, compressed.
    make_retries_immediate(&audio).await;
    enqueue(&audio, missing).await;

    for delivery in 1..=3 {
        assert_eq!(
            worker::poll_once(&ctx).await.expect("a poll cycle"),
            1,
            "delivery {delivery} of the doomed chunk"
        );
    }

    let failed = chunk(&pool, missing).await;
    assert_eq!(failed.status, "failed");
    let recorded = failed.error.expect("an autopsy was recorded");
    assert!(
        !recorded.is_empty() && recorded.len() <= 501,
        "the failure reason is present and bounded, got {} bytes",
        recorded.len()
    );

    let events = transcript_events(&pool, patient_id).await;
    assert_eq!(events.len(), 1, "the encounter was salvaged, not abandoned");
    assert_eq!(
        events[0].1,
        "good audio.\n\n[segment 1 audio unavailable — transcription failed]"
    );

    let repo = PostgresRepository::for_org(pool.clone(), ORG_A);
    let note = er_app::load::load_patient_note(&repo, patient_id)
        .await
        .expect("replay the salvaged note")
        .expect("AudioStarted and TranscriptReady produce a note");
    assert_eq!(
        note.encounters[0].transcript.as_deref(),
        Some("good audio.\n\n[segment 1 audio unavailable — transcription failed]"),
        "the usable segment and visible gap survive full replay"
    );

    // The fourth receive attempt is where SQS applies maxReceiveCount=3 and
    // moves the still-undeleted message instead of handing it to the worker.
    assert_eq!(
        worker::poll_once(&ctx)
            .await
            .expect("trigger queue redrive"),
        0,
        "the poison message must leave the main queue after three deliveries"
    );
    let dead_letter = receive_dlq_job(&audio)
        .await
        .expect("the poison job must be retained in the dead-letter queue");
    assert_eq!(
        dead_letter,
        serde_json::json!({
            "kind": "TranscribeChunk",
            "chunk_id": missing,
        }),
        "the DLQ retains the opaque job for operator diagnosis"
    );
}

/// A message the worker cannot decode is left on the queue to strike out to the
/// DLQ. Deleting it would be tidier and would destroy the only evidence.
#[sqlx::test(migrations = "./migrations")]
async fn an_undecodable_message_is_not_deleted(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    let ctx = echo_worker(pool.clone(), &audio);

    audio
        .sqs
        .send_message()
        .queue_url(&audio.config.stt_queue_url)
        .message_body(r#"{"kind":"GenerateMdm","patient_id":"whatever"}"#)
        .send()
        .await
        .expect("send a job this worker predates");

    assert_eq!(
        worker::poll_once(&ctx).await.expect("a poll cycle"),
        1,
        "the message was received"
    );

    // Still on the queue — leased to this consumer, not deleted. When the lease
    // lapses SQS redelivers, and the redrive policy eventually takes it to the
    // DLQ, which is the only place a human will ever find out this happened.
    assert_eq!(
        queue_depth(&audio).await,
        1,
        "an undecodable message must survive to reach the dead-letter queue"
    );
}

/// A job naming a chunk that does not exist is poison, not a retryable blip — but
/// it must not take the polling cycle down with it.
#[sqlx::test(migrations = "./migrations")]
async fn a_job_for_an_unknown_chunk_does_not_stop_the_worker(pool: PgPool) {
    let audio = ensure_test_audio_infra().await;
    let (patient_id, encounter_id) = seed(&pool).await;
    let ctx = echo_worker(pool.clone(), &audio);

    enqueue(&audio, Uuid::now_v7()).await;
    assert_eq!(worker::poll_once(&ctx).await.expect("a poll cycle"), 1);

    // The very next cycle still does real work.
    let only = upload_chunk(
        &pool,
        &audio,
        patient_id,
        encounter_id,
        0,
        true,
        b"unaffected.",
    )
    .await;
    enqueue(&audio, only).await;
    drain(&ctx).await;

    assert_eq!(chunk(&pool, only).await.status, "done");
    assert_eq!(transcript_events(&pool, patient_id).await.len(), 1);
}
