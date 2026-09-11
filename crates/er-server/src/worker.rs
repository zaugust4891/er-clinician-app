//! The transcription worker: poll, transcribe, assemble, publish (Step 6 S7).
//!
//! This is the consumer half of everything S0–S6 built. The API's job ended the
//! moment it wrote an `audio_chunks` row and put a `chunk_id` on a queue (S4);
//! this module is what turns that id into a per-chunk transcript, and a finished
//! encounter into one server-authored `TranscriptReady` event.
//!
//! **Why the logic lives here and not in the binary.** `bin/worker.rs` is a boot
//! sequence and a `loop`. Everything with a rule in it — §C2's at-least-once
//! delivery and three-strike retry, §C3's index-ordered assembly with gap
//! markers, §C4's deterministic event id — is a named function in this module, so
//! the tests can drive one cycle at a time with [`poll_once`] instead of spawning
//! a process and hoping about timing.
//!
//! **The second author.** Every event before Step 6 was written by a device
//! holding a clinician's JWT. This one is written by a headless process that
//! holds no token at all, so it cannot ask "who is the caller?" — it asks "who
//! owns this row?" The chunk row carries `org_id`, captured at mint time by the
//! authenticated request that created it, and the worker resumes that tenancy by
//! constructing `PostgresRepository::for_org` from it (§C4, D6.8). There is no
//! code path here that queries without an org: the one unscoped read,
//! [`PostgresRepository::chunk_row_any_org`], exists only to *discover* the org
//! and its result is used for nothing else.
//!
//! **PHI (§C5).** Queue messages carry an opaque id; logs carry ids, statuses and
//! durations. Transcript text lives in `audio_chunks.transcript` and in the event
//! payload, and is never logged — `log identifiers, never content`, the same
//! house rule `routes.rs` follows.

use std::time::Duration;

use anyhow::{Context, anyhow};
use aws_sdk_sqs::types::{Message, MessageSystemAttributeName};
use chrono::{DateTime, Utc};
use uuid::Uuid;

use er_ai::deidentify::Deidentifier;
use er_ai::error::AiError;
use er_ai::summarizer::{AppSummarizer, Summarizer};
use er_domain::{NoteEvent, NotePhase};
use er_pipeline::speech_to_text::{AppSpeechToText, SpeechToText};
use er_store::repository::Repository;

use crate::queue::Job;
use crate::repository::{ChunkRow, NewUsage, PostgresRepository};

/// The namespace for [`transcript_event_id`].
///
/// A UUIDv5 is `hash(namespace, name)`, so the namespace is an arbitrary constant
/// that must never change: rotating it would re-derive every future event id and
/// break the idempotency this whole design rests on. It is written literally here
/// rather than derived from anything, precisely so nothing can drift into it.
const TRANSCRIPT_EVENT_NS: Uuid = Uuid::from_u128(0x6b3f9c1e_7d24_5a88_9f01_2c5e8ab41d70);

/// How many deliveries a chunk gets before its message becomes the DLQ's problem.
///
/// This mirrors `AUDIO_JOB_MAX_RECEIVE_COUNT` in `aws.rs`, which is what actually
/// configures the redrive policy. The worker needs its own copy because it has to
/// recognise the *last* delivery from the inside — that is the one delivery on
/// which it must record the autopsy and salvage the encounter, because there will
/// not be another (§C2, D6.14).
const MAX_DELIVERIES: u32 = 3;

/// What one encounter's assembled transcript says where a chunk's audio could not
/// be transcribed.
///
/// The marker is visible on purpose. A clinician reading a note must be able to
/// tell "nothing was said here" from "we lost this minute" — silently closing the
/// gap would turn a known hole in a medical record into an invisible one.
fn gap_marker(chunk_index: i32) -> String {
    format!("[segment {chunk_index} audio unavailable — transcription failed]")
}

/// Everything one worker process needs, and nothing about HTTP.
///
/// `stt` is the concrete [`AppSpeechToText`] rather than a generic `S:
/// SpeechToText`, which is the whole reason S5 built that enum: the trait is
/// RPITIT and not dyn-compatible, so "Deepgram if a key is configured, mock if
/// not" has to be absorbed once, at boot, into a single concrete type (D6.2).
pub struct WorkerCtx {
    pub pool: sqlx::PgPool,
    pub s3: aws_sdk_s3::Client,
    pub sqs: aws_sdk_sqs::Client,
    pub bucket: String,
    pub queue_url: String,
    pub stt: AppSpeechToText,
    pub summarizer: AppSummarizer,
    /// SQS long-poll duration. The binary sets 20 s — one held connection instead
    /// of a busy loop of empty receives, which is both cheaper and lower latency
    /// (§C2). Tests set 0–1 s so a suite is not paced by a network wait.
    pub wait_time_seconds: i32,
}

impl WorkerCtx {
    /// Resume the tenancy a chunk row carries.
    fn repo_for_chunk(&self, row: &ChunkRow) -> PostgresRepository {
        PostgresRepository::for_org(self.pool.clone(), row.org_id)
    }
}

/// One receive → dispatch → delete-or-release cycle.
///
/// Returns how many messages were handled, so a test can drive the worker
/// deterministically — `while poll_once(&ctx).await? > 0 {}` drains the queue and
/// then stops — instead of spawning the binary and racing it.
///
/// **The delete is the contract.** SQS redelivers anything not deleted before its
/// visibility timeout expires, so "don't delete" *is* the retry mechanism (§C2).
/// Exactly one path in this function deletes a message: the one where the chunk
/// reached a terminal state we are content with. Every failure path leaves the
/// message alone and lets the lease lapse.
pub async fn poll_once(ctx: &WorkerCtx) -> anyhow::Result<usize> {
    let messages = receive_batch(ctx).await?;
    let handled = messages.len();

    for message in &messages {
        handle_message(ctx, message).await;
    }

    Ok(handled)
}

/// The long poll, on its own.
///
/// Separated from the processing half so [`run`] can put *only this* inside a
/// `tokio::select!`. Cancelling a receive is free — SQS simply never leases the
/// message — whereas cancelling a `handle_message` would abandon a transcription
/// somewhere in the middle, leave the chunk stuck at `transcribing`, and make the
/// next delivery pay a second time for a Deepgram call we already made.
async fn receive_batch(ctx: &WorkerCtx) -> anyhow::Result<Vec<Message>> {
    let received = ctx
        .sqs
        .receive_message()
        .queue_url(&ctx.queue_url)
        .max_number_of_messages(1)
        .wait_time_seconds(ctx.wait_time_seconds)
        // Which delivery this is, 1-based. Without it the worker cannot tell a
        // first attempt from a last one, and "record the autopsy on the final
        // strike" has nothing to trigger on.
        .message_system_attribute_names(MessageSystemAttributeName::ApproximateReceiveCount)
        .send()
        .await
        .with_context(|| format!("receive from {}", ctx.queue_url))?;

    Ok(received.messages().to_vec())
}

/// Dispatch one message and decide its fate.
///
/// Infallible by design: one poison message must not abort a polling cycle, and
/// there is no caller above this that could do anything more useful with an error
/// than log it. Every outcome is either "delete" or "leave it for SQS".
async fn handle_message(ctx: &WorkerCtx, message: &Message) {
    let delivery = delivery_count(message);
    let body = message.body().unwrap_or_default();

    let job = match serde_json::from_str::<Job>(body) {
        Ok(job) => job,
        Err(error) => {
            // A body we cannot read is exactly what a dead-letter queue is for.
            // Deleting it would be tidier and would destroy the only copy of the
            // evidence; striking out costs three deliveries and leaves a corpse
            // the Step 9 alarm can point at.
            tracing::error!(
                %error,
                delivery,
                "undecodable job body; leaving it to strike out to the dead-letter queue"
            );
            return;
        }
    };

    match job {
        Job::TranscribeChunk { chunk_id } => {
            match handle_transcribe_chunk(ctx, chunk_id).await {
                Ok(()) => {
                    delete_message(ctx, message, chunk_id).await;
                }
                Err(error) if non_retryable_ai_error(&error).is_some() => {
                    tracing::error!(
                        %chunk_id,
                        delivery,
                        error_kind = non_retryable_ai_error(&error).unwrap_or("unknown"),
                        "encounter summarization failed permanently; transcript retained"
                    );
                    delete_message(ctx, message, chunk_id).await;
                }
                Err(error) if delivery >= MAX_DELIVERIES && retryable_ai_error(&error) => {
                    // The audio row is already terminal and the transcript is
                    // durable. Preserve it as done and let only the summary
                    // message move to the DLQ; routing this through
                    // record_failure would re-enter assembly and pay for a
                    // second provider call on the final delivery.
                    tracing::error!(
                        %chunk_id,
                        delivery,
                        error = %format!("{error:#}"),
                        "encounter summarization exhausted retries; transcript retained"
                    );
                }
                Err(error) if delivery >= MAX_DELIVERIES => {
                    // Last delivery. Two obligations before letting go, in this
                    // order: record why (the row is the autopsy), then salvage
                    // the encounter, because the note is worth more than the
                    // chunk — a gap-marked transcript still documents the visit
                    // (§C3, D6.14).
                    tracing::error!(
                        %chunk_id,
                        delivery,
                        error = %format!("{error:#}"),
                        "chunk transcription failed on its final delivery; marking failed"
                    );
                    if let Err(mark_error) = record_failure(ctx, chunk_id, &error).await {
                        tracing::error!(
                            %chunk_id,
                            error = %format!("{mark_error:#}"),
                            "could not record the terminal failure for this chunk"
                        );
                    }
                    // Deliberately NOT deleted: the redrive policy moves it to the
                    // DLQ on the next delivery attempt.
                }
                Err(error) => {
                    // Strikes 1..MAX-1: say nothing to SQS. The visibility timeout
                    // expires and the message comes back on its own.
                    tracing::warn!(
                        %chunk_id,
                        delivery,
                        error = %format!("{error:#}"),
                        "chunk transcription failed; leaving the message for redelivery"
                    );
                }
            }
        }
        Job::GenerateMdm { patient_id, job_id } => {
            match handle_generate_mdm(ctx, patient_id, job_id).await {
                Ok(()) => delete_message(ctx, message, job_id).await,
                Err(error) if delivery >= MAX_DELIVERIES => {
                    tracing::error!(
                        %job_id,
                        %patient_id,
                        delivery,
                        error = %format!("{error:#}"),
                        "MDM generation failed on its final delivery"
                    );
                    if let Err(mark_error) = record_mdm_terminal_failure(ctx, job_id).await {
                        tracing::error!(
                            %job_id,
                            error = %format!("{mark_error:#}"),
                            "could not record terminal MDM job failure"
                        );
                    }
                    // Leave the message for the queue's redrive policy and DLQ.
                }
                Err(error) => {
                    tracing::warn!(
                        %job_id,
                        %patient_id,
                        delivery,
                        error = %format!("{error:#}"),
                        "MDM generation failed transiently; leaving for redelivery"
                    );
                }
            }
        }
    }
}

/// Fetch, transcribe, store — and then ask whether this chunk was the last one.
///
/// The happy path is five statements. That is the point of S0–S6: every rule this
/// function relies on is enforced somewhere else — the status machine is in SQL,
/// the tenancy is on the row, the vendor is behind a trait.
pub async fn handle_transcribe_chunk(ctx: &WorkerCtx, chunk_id: Uuid) -> anyhow::Result<()> {
    let row = PostgresRepository::chunk_row_any_org(&ctx.pool, chunk_id)
        .await
        .context("look up the chunk row")?
        .ok_or_else(|| anyhow!("no audio_chunks row for chunk {chunk_id}"))?;

    // At-least-once delivery means duplicates are normal traffic, not an error
    // (§C2). A chunk that is already terminal has nothing left to do — and the
    // status machine would refuse to reopen it anyway, since
    // `mark_chunk_transcribing` only accepts `uploaded` and `transcribing`. So
    // this is a shrug, and the shrug ends in a delete.
    if er_api::chunk_status::is_terminal(&row.status) {
        tracing::info!(
            %chunk_id,
            status = %row.status,
            "duplicate delivery for an already-terminal chunk; nothing to do"
        );
        return try_assemble(ctx, &row).await;
    }

    let repo = ctx.repo_for_chunk(&row);

    repo.mark_chunk_transcribing(chunk_id)
        .await
        .context("mark the chunk transcribing")?
        .ok_or_else(|| anyhow!("chunk {chunk_id} vanished while claiming it"))?;

    let bytes = ctx
        .s3
        .get_object()
        .bucket(&ctx.bucket)
        .key(&row.s3_key)
        .send()
        .await
        .with_context(|| format!("fetch audio for chunk {chunk_id}"))?
        .body
        .collect()
        .await
        .with_context(|| format!("read audio body for chunk {chunk_id}"))?
        .into_bytes();

    let result = ctx
        .stt
        .transcribe(&bytes, &row.content_type)
        .await
        .with_context(|| format!("transcribe chunk {chunk_id}"))?;

    let duration_ms = duration_to_millis(result.audio_duration_secs);

    repo.store_chunk_transcript(chunk_id, &result.text, duration_ms)
        .await
        .context("store the chunk transcript")?
        .ok_or_else(|| anyhow!("chunk {chunk_id} vanished while storing its transcript"))?;

    // Identifiers, sizes and durations — never a word of what was said (§C5).
    tracing::info!(
        %chunk_id,
        encounter_id = %row.encounter_id,
        chunk_index = row.chunk_index,
        audio_bytes = bytes.len(),
        duration_ms,
        transcript_chars = result.text.len(),
        "chunk transcribed"
    );

    // Every chunk that goes terminal asks the same question, and all but one get
    // the same answer. There is no coordinator deciding who is last: assembly is
    // a pure function of the rows, so whoever finishes last happens to be the one
    // who finds the encounter complete (§C3).
    try_assemble(ctx, &row).await
}

/// Record a terminal failure and then try to salvage the encounter around it.
///
/// Split out because the ordering matters and is easy to get backwards. The row
/// must say `failed` *before* assembly runs, or assembly will look at a chunk
/// still marked `transcribing`, decide the encounter is not ready, and the note
/// will never be published at all.
async fn record_failure(
    ctx: &WorkerCtx,
    chunk_id: Uuid,
    error: &anyhow::Error,
) -> anyhow::Result<()> {
    let row = PostgresRepository::chunk_row_any_org(&ctx.pool, chunk_id)
        .await
        .context("look up the failing chunk row")?
        .ok_or_else(|| anyhow!("no audio_chunks row for chunk {chunk_id}"))?;

    let repo = ctx.repo_for_chunk(&row);
    // `{:#}` renders the whole anyhow context chain on one line. The column is a
    // PHI-free autopsy — vendor status codes and our own context strings, never
    // audio and never transcript text (§C5).
    let summary = truncate_error(&format!("{error:#}"));
    repo.mark_chunk_failed(chunk_id, &summary)
        .await
        .context("mark the chunk failed")?;

    // The note outranks the chunk: an encounter whose other segments transcribed
    // fine still becomes a usable note, with this one marked as a visible gap.
    try_assemble(ctx, &row).await
}

/// §C3's assembly rule and §C4's idempotent publish.
///
/// Runs after *every* terminal chunk and is a no-op for all but the last one.
/// `row` supplies the encounter, the org and the patient — it is the chunk that
/// just went terminal, so its tenancy is the encounter's tenancy.
pub async fn try_assemble(ctx: &WorkerCtx, row: &ChunkRow) -> anyhow::Result<()> {
    let repo = ctx.repo_for_chunk(row);
    // Org-scoped and `ORDER BY chunk_index` in SQL. Order comes from the index the
    // device stamped, never from arrival time or `created_at` — chunk 2 routinely
    // finishes before chunk 1 on ED wifi (§C3).
    let chunks = repo
        .encounter_chunks(row.encounter_id)
        .await
        .context("read the encounter's chunks")?;

    let Some(final_index) = final_chunk_index(&chunks) else {
        // No chunk claims finality yet, so the encounter is still recording. Not
        // our moment; say nothing.
        return Ok(());
    };

    if !assembly_ready(&chunks, final_index) {
        // Someone is still cooking — or an index is missing entirely, which is
        // §C3's named stall case: correct behaviour (the audio genuinely is not
        // all there) and Step 9's janitor to sweep up.
        return Ok(());
    }

    let text = assemble_transcript(&chunks, final_index);
    let occurred = clinical_time(&chunks);
    let event_id = transcript_event_id(row.encounter_id);

    let sequence_id = repo
        .append_event(
            row.patient_id,
            event_id,
            NoteEvent::transcript_ready(row.patient_id, row.encounter_id, text.clone(), occurred),
        )
        .await
        .context("append the TranscriptReady event")?;

    tracing::info!(
        encounter_id = %row.encounter_id,
        patient_id = %row.patient_id,
        %event_id,
        sequence_id,
        chunks = chunks.len(),
        "encounter transcript assembled and published"
    );

    summarize_encounter_for(ctx, row, &text).await
}

/// Produce one summary for an assembled encounter.
///
/// The transaction-scoped advisory lock covers the existence check, provider
/// call, and event append. Duplicate chunk deliveries therefore wait and then
/// observe the first summary instead of paying for a competing call.
async fn summarize_encounter_for(
    ctx: &WorkerCtx,
    row: &ChunkRow,
    transcript: &str,
) -> anyhow::Result<()> {
    if transcript.trim().is_empty() {
        return Ok(());
    }

    let repo = ctx.repo_for_chunk(row);
    let lock_key = format!("summary:{}", row.encounter_id);
    let mut lock = ctx.pool.begin().await.context("begin summary lock")?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(&lock_key)
        .execute(&mut *lock)
        .await
        .context("acquire summary lock")?;

    let events = repo
        .get_events(row.patient_id)
        .await
        .context("check for an existing encounter summary")?;
    if events.iter().any(|stored| {
        matches!(
            &stored.event,
            NoteEvent::EncounterSummarized { encounter_id, .. }
                if *encounter_id == row.encounter_id
        )
    }) {
        lock.commit().await.context("release summary lock")?;
        return Ok(());
    }

    let note = er_app::load_patient_note(&repo, row.patient_id)
        .await
        .context("replay note before summarization")?
        .ok_or_else(|| anyhow!("patient note has no events"))?;
    let encounter_number = note
        .encounters
        .iter()
        .position(|encounter| encounter.id == row.encounter_id)
        .map(|index| index as u32 + 1)
        .ok_or_else(|| anyhow!("encounter is absent from the replayed note"))?;
    let safe_transcript = Deidentifier::scrub(transcript);

    let generated = ctx
        .summarizer
        .summarize_encounter(&safe_transcript, &note.note_sections, encounter_number)
        .await
        .context("summarize the assembled encounter")?;

    repo.append_event(
        row.patient_id,
        Uuid::now_v7(),
        NoteEvent::encounter_summarized(
            row.patient_id,
            row.encounter_id,
            generated.value,
            vec![row.encounter_id],
        ),
    )
    .await
    .context("append EncounterSummarized")?;

    if let Err(error) = repo
        .record_usage(NewUsage {
            clinician_id: None,
            patient_id: Some(row.patient_id),
            job_id: None,
            kind: "summarize",
            model: &generated.model,
            usage: &generated.usage,
        })
        .await
    {
        tracing::error!(
            encounter_id = %row.encounter_id,
            error = %error,
            "summary succeeded but its usage row could not be recorded"
        );
    }

    lock.commit().await.context("release summary lock")?;
    Ok(())
}

/// Generate an MDM from the durable note, preserving Phase 3's content-before-
/// state ordering. The patient lock also turns duplicate SQS deliveries into a
/// terminal-status read instead of a second provider call.
pub async fn handle_generate_mdm(
    ctx: &WorkerCtx,
    patient_id: Uuid,
    job_id: Uuid,
) -> anyhow::Result<()> {
    let discovered = PostgresRepository::job_row_any_org(&ctx.pool, job_id)
        .await
        .context("look up the MDM job")?
        .ok_or_else(|| anyhow!("no jobs row for job {job_id}"))?;
    if discovered.patient_id != patient_id || discovered.kind != "mdm" {
        return Err(anyhow!("queue identifiers do not match the MDM receipt"));
    }
    if er_api::job_status::is_terminal(&discovered.status) {
        return Ok(());
    }

    let repo = PostgresRepository::for_org(ctx.pool.clone(), discovered.org_id);
    let lock_key = format!("mdm:{patient_id}");
    let mut lock = ctx.pool.begin().await.context("begin MDM lock")?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(&lock_key)
        .execute(&mut *lock)
        .await
        .context("acquire MDM lock")?;

    let current = repo
        .job_row(job_id)
        .await
        .context("re-read the MDM job")?
        .ok_or_else(|| anyhow!("MDM job disappeared"))?;
    if er_api::job_status::is_terminal(&current.status) {
        lock.commit().await.context("release MDM lock")?;
        return Ok(());
    }
    repo.mark_job_running(job_id)
        .await
        .context("mark MDM job running")?;

    let note = match er_app::load_patient_note(&repo, patient_id)
        .await
        .context("replay note for MDM")?
    {
        Some(note) => note,
        None => {
            repo.mark_job_failed(job_id, "note_not_found").await?;
            lock.commit().await?;
            return Ok(());
        }
    };

    // Recover the two content-before-state crash windows without regenerating.
    if note
        .note_sections
        .mdm
        .as_deref()
        .is_some_and(|mdm| !mdm.trim().is_empty())
    {
        if note.phase == NotePhase::Idle {
            repo.append_event(
                patient_id,
                Uuid::now_v7(),
                NoteEvent::trigger_mdm(patient_id),
            )
            .await
            .context("complete recovered MDM phase transition")?;
        }
        if matches!(note.phase, NotePhase::Idle | NotePhase::Finalizing) {
            repo.mark_job_done(job_id).await?;
            lock.commit().await?;
            return Ok(());
        }
    }

    if note.phase != NotePhase::Idle {
        repo.mark_job_failed(job_id, "note_not_idle").await?;
        lock.commit().await?;
        return Ok(());
    }

    let objective = er_app::objective_data::assemble_objective_data(&note);
    let has_hpi = note
        .note_sections
        .hpi
        .as_deref()
        .is_some_and(|hpi| !hpi.trim().is_empty());
    if !has_hpi && objective.trim().is_empty() && note.disposition.is_none() {
        repo.mark_job_failed(job_id, "nothing_to_generate_from")
            .await?;
        lock.commit().await?;
        return Ok(());
    }

    let disposition = note
        .disposition
        .as_ref()
        .map(er_app::conversions::disposition::format_disposition_for_mdm)
        .unwrap_or_else(|| "Disposition not yet determined".to_string());
    let source_ids = note
        .encounters
        .iter()
        .map(|encounter| encounter.id)
        .collect::<Vec<_>>();

    let generated = match ctx
        .summarizer
        .generate_mdm(
            &note.note_sections,
            &Deidentifier::scrub(&objective),
            &Deidentifier::scrub(&disposition),
        )
        .await
    {
        Ok(generated) => generated,
        Err(error) if !error.is_retryable() => {
            repo.mark_job_failed(job_id, permanent_ai_code(&error))
                .await?;
            lock.commit().await?;
            return Ok(());
        }
        Err(error) => return Err(error).context("generate MDM"),
    };

    // Clinical content first, state transition second. A crash between these
    // writes is repaired by the recovery branch above.
    repo.append_event(
        patient_id,
        Uuid::now_v7(),
        NoteEvent::mdm_generated(patient_id, generated.value, source_ids),
    )
    .await
    .context("append MdmGenerated")?;
    repo.append_event(
        patient_id,
        Uuid::now_v7(),
        NoteEvent::trigger_mdm(patient_id),
    )
    .await
    .context("append TriggerMDM")?;

    if let Err(error) = repo
        .record_usage(NewUsage {
            clinician_id: Some(discovered.clinician_id),
            patient_id: Some(patient_id),
            job_id: Some(job_id),
            kind: "mdm",
            model: &generated.model,
            usage: &generated.usage,
        })
        .await
    {
        tracing::error!(
            %job_id,
            error = %error,
            "MDM succeeded but its usage row could not be recorded"
        );
    }
    repo.mark_job_done(job_id)
        .await
        .context("mark MDM job done")?;
    lock.commit().await.context("release MDM lock")?;
    Ok(())
}

async fn record_mdm_terminal_failure(ctx: &WorkerCtx, job_id: Uuid) -> anyhow::Result<()> {
    let row = PostgresRepository::job_row_any_org(&ctx.pool, job_id)
        .await?
        .ok_or_else(|| anyhow!("no jobs row for job {job_id}"))?;
    let repo = PostgresRepository::for_org(ctx.pool.clone(), row.org_id);
    repo.mark_job_failed(job_id, "generation_failed_after_retries")
        .await?;
    Ok(())
}

fn permanent_ai_code(error: &AiError) -> &'static str {
    match error {
        AiError::Refused { .. } => "provider_refused",
        AiError::Summarization(_) => "invalid_model_output",
        _ => "generation_failed",
    }
}

fn non_retryable_ai_error(error: &anyhow::Error) -> Option<&'static str> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<AiError>())
        .filter(|error| !error.is_retryable())
        .map(permanent_ai_code)
}

fn retryable_ai_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<AiError>())
        .is_some_and(AiError::is_retryable)
}

/// The index of the chunk the device marked final, if one has arrived.
///
/// A partial unique index guarantees at most one per encounter (S0), so this
/// cannot be ambiguous — but it can be absent, which is the interesting case.
fn final_chunk_index(chunks: &[ChunkRow]) -> Option<i32> {
    chunks.iter().find(|c| c.is_final).map(|c| c.chunk_index)
}

/// Is every index `0..=final_index` present and finished?
///
/// Both halves matter. "Finished" excludes a chunk still uploading or
/// transcribing; "present" excludes the device that died after uploading 0 and 2
/// of 3, where waiting forever is the correct answer (§C3).
fn assembly_ready(chunks: &[ChunkRow], final_index: i32) -> bool {
    (0..=final_index).all(|index| {
        chunks
            .iter()
            .any(|c| c.chunk_index == index && er_api::chunk_status::is_terminal(&c.status))
    })
}

/// Join the encounter's per-chunk transcripts into one note-ready string.
///
/// Pure, and separate from the database for the usual reason: this is where the
/// rules that are easy to get wrong live — ordering, gap markers, what to do with
/// a chunk that transcribed to nothing — and keeping it a function over rows means
/// all of them are unit-testable with no Postgres, no S3 and no queue.
///
/// Three rules:
///
/// 1. **Index order, not arrival order.** The rows arrive sorted by SQL; the
///    filter below preserves that.
/// 2. **A `failed` chunk becomes a visible marker**, never a silent omission.
/// 3. **A `done` chunk that transcribed to nothing is skipped.** S6's D6.23 made
///    silence a successful empty transcript rather than an error, which is right —
///    but joining `""` with `"\n\n"` separators would punch a blank hole in the
///    note that looks exactly like a formatting bug. Silence contributes nothing;
///    only a genuine failure gets a marker.
fn assemble_transcript(chunks: &[ChunkRow], final_index: i32) -> String {
    chunks
        .iter()
        .filter(|c| c.chunk_index <= final_index)
        .filter_map(|c| {
            if c.status == er_api::chunk_status::DONE {
                let text = c.transcript.as_deref().unwrap_or_default().trim();
                (!text.is_empty()).then(|| text.to_string())
            } else {
                Some(gap_marker(c.chunk_index))
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// When the encounter's audio ended, as far as the record is concerned.
///
/// The latest chunk `created_at` — the moment the last segment's row was minted,
/// which is the closest thing the server has to "the clinician stopped
/// recording". It matters because `apply_event` uses `clinical_occurred_at` for
/// the note's `updated_at` and section timestamps, so replay has to be
/// deterministic: a wall-clock `now()` here would make the same event stream
/// rebuild into a different note tomorrow.
fn clinical_time(chunks: &[ChunkRow]) -> DateTime<Utc> {
    chunks
        .iter()
        .map(|c| c.created_at)
        .max()
        .unwrap_or_else(Utc::now)
}

/// The `TranscriptReady` event id for an encounter — always the same one.
///
/// This is the move that makes the whole assembly race harmless (§C4, D6.7). Two
/// chunks can finish at the same instant and both find the encounter complete;
/// both then call `append_event` with *this* id, and `ON CONFLICT (event_id) DO
/// NOTHING` turns the second one into a read of the first one's `sequence_id`.
/// No lock, no leader election, no "who assembles?" protocol — just a name that
/// two processes independently agree on.
pub fn transcript_event_id(encounter_id: Uuid) -> Uuid {
    Uuid::new_v5(&TRANSCRIPT_EVENT_NS, encounter_id.as_bytes())
}

/// Seconds of audio → the `duration_ms` column.
///
/// Saturating and non-negative: a vendor that reports a nonsense duration should
/// cost us a wrong number in a metering column, not a panic in a worker or a
/// wrapped negative from an `as` cast.
fn duration_to_millis(seconds: f32) -> i32 {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    (seconds * 1000.0).round().min(i32::MAX as f32) as i32
}

/// Keep the autopsy readable. A vendor error chain can be long; the column is for
/// a human deciding what broke, not for a stack trace.
fn truncate_error(message: &str) -> String {
    const MAX: usize = 500;
    if message.len() <= MAX {
        return message.to_string();
    }
    let end = (0..=MAX)
        .rev()
        .find(|i| message.is_char_boundary(*i))
        .unwrap_or(0);
    format!("{}…", &message[..end])
}

/// `ApproximateReceiveCount`, 1-based, defaulting to a first delivery.
///
/// Defaulting to 1 when the attribute is missing is the conservative direction:
/// the worker retries rather than declaring a chunk dead on a delivery it cannot
/// count. A stuck message is visible in the queue; a chunk wrongly marked failed
/// is a gap in a medical record.
fn delivery_count(message: &Message) -> u32 {
    message
        .attributes()
        .and_then(|attributes| attributes.get(&MessageSystemAttributeName::ApproximateReceiveCount))
        .and_then(|value| value.parse().ok())
        .unwrap_or(1)
}

/// Remove a message we are done with. Failing to delete is survivable — the job
/// simply arrives again and the duplicate-delivery shrug above handles it — so it
/// is logged rather than propagated.
async fn delete_message(ctx: &WorkerCtx, message: &Message, chunk_id: Uuid) {
    let Some(receipt) = message.receipt_handle() else {
        tracing::error!(%chunk_id, "message has no receipt handle; cannot delete");
        return;
    };

    if let Err(error) = ctx
        .sqs
        .delete_message()
        .queue_url(&ctx.queue_url)
        .receipt_handle(receipt)
        .send()
        .await
    {
        tracing::warn!(
            %chunk_id,
            error = %error,
            "failed to delete a completed job; it will be redelivered and shrugged at"
        );
    }
}

/// Poll forever, stopping cleanly when the platform asks.
///
/// `shutdown` is the caller's cancellation signal — SIGTERM/SIGINT in the binary,
/// which is what ECS sends before it kills a task on a rolling deploy.
///
/// **Only the receive races the signal.** `tokio::select!` drops the losing
/// future, so anything inside it can be cancelled mid-`await`. That is harmless
/// for a long poll and actively harmful for a transcription — a cancelled
/// `handle_message` would leave a chunk stuck at `transcribing` with its message
/// undeleted, and the redelivery would pay for the same Deepgram call twice. So
/// the signal races [`receive_batch`] alone, and any message already in hand is
/// processed to completion before the next check.
pub async fn run(ctx: &WorkerCtx, mut shutdown: impl Future<Output = ()> + Unpin) {
    loop {
        let messages = tokio::select! {
            biased;
            _ = &mut shutdown => {
                tracing::info!("shutdown signal received; transcription worker stopping");
                return;
            }
            received = receive_batch(ctx) => match received {
                Ok(messages) => messages,
                Err(error) => {
                    // Almost always the queue being unreachable. Back off rather
                    // than spinning a tight loop of failing API calls — and let
                    // the signal interrupt the backoff, so an outage cannot make
                    // shutdown take five seconds longer than it should.
                    tracing::error!(error = %format!("{error:#}"), "poll cycle failed");
                    tokio::select! {
                        biased;
                        _ = &mut shutdown => {
                            tracing::info!("shutdown signal received; transcription worker stopping");
                            return;
                        }
                        _ = tokio::time::sleep(Duration::from_secs(5)) => continue,
                    }
                }
            },
        };

        // Outside the select on purpose: this half is not cancellation-safe.
        for message in &messages {
            handle_message(ctx, message).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(index: i32, status: &str, transcript: Option<&str>, is_final: bool) -> ChunkRow {
        ChunkRow {
            id: Uuid::now_v7(),
            org_id: Uuid::from_u128(0x0A),
            patient_id: Uuid::from_u128(0xB0),
            encounter_id: Uuid::from_u128(0xE1),
            chunk_index: index,
            is_final,
            content_type: "audio/wav".to_string(),
            s3_key: format!("org/enc/{index}.wav"),
            status: status.to_string(),
            transcript: transcript.map(str::to_string),
            error: None,
            duration_ms: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn transcripts_join_in_index_order_regardless_of_arrival() {
        // The rows arrive from SQL sorted; this pins that the join respects that
        // order and inserts exactly one blank line between segments.
        let chunks = vec![
            chunk(0, "done", Some("segment zero."), false),
            chunk(1, "done", Some("segment one."), false),
            chunk(2, "done", Some("segment two."), true),
        ];

        assert_eq!(
            assemble_transcript(&chunks, 2),
            "segment zero.\n\nsegment one.\n\nsegment two."
        );
    }

    #[test]
    fn a_failed_chunk_becomes_a_visible_gap_not_a_silent_omission() {
        let chunks = vec![
            chunk(0, "done", Some("segment zero."), false),
            chunk(1, "failed", None, false),
            chunk(2, "done", Some("segment two."), true),
        ];

        assert_eq!(
            assemble_transcript(&chunks, 2),
            "segment zero.\n\n[segment 1 audio unavailable — transcription failed]\n\nsegment two."
        );
    }

    #[test]
    fn a_chunk_that_transcribed_to_silence_contributes_nothing() {
        // S6's D6.23: a present-but-empty transcript is a successful
        // transcription of a quiet moment. Joining "" would leave a blank hole
        // indistinguishable from a formatting bug, and a gap marker would be an
        // outright lie — the audio was fine.
        let chunks = vec![
            chunk(0, "done", Some("segment zero."), false),
            chunk(1, "done", Some("   "), false),
            chunk(2, "done", Some("segment two."), true),
        ];

        assert_eq!(
            assemble_transcript(&chunks, 2),
            "segment zero.\n\nsegment two."
        );
    }

    #[test]
    fn chunks_after_the_final_index_are_excluded() {
        // Defence in depth: a partial unique index makes a late higher-indexed
        // chunk impossible to create alongside a final one, but assembly should
        // not depend on another table's constraint for its own correctness.
        let chunks = vec![
            chunk(0, "done", Some("kept."), false),
            chunk(1, "done", Some("also kept."), true),
            chunk(2, "done", Some("must not appear."), false),
        ];

        assert_eq!(assemble_transcript(&chunks, 1), "kept.\n\nalso kept.");
    }

    #[test]
    fn assembly_waits_until_every_index_is_terminal() {
        let mut chunks = vec![
            chunk(0, "done", Some("zero."), false),
            chunk(1, "transcribing", None, false),
            chunk(2, "done", Some("two."), true),
        ];
        assert!(!assembly_ready(&chunks, 2), "index 1 is still cooking");

        chunks[1].status = "failed".to_string();
        assert!(
            assembly_ready(&chunks, 2),
            "failed is terminal too — the note gets a gap, not an eternal wait"
        );
    }

    #[test]
    fn a_missing_index_stalls_assembly_forever() {
        // §C3's named stall case: a device that died after uploading 0 and 2.
        // Waiting is correct — the audio genuinely is not all there — and Step 9's
        // sweep is what eventually notices.
        let chunks = vec![
            chunk(0, "done", Some("zero."), false),
            chunk(2, "done", Some("two."), true),
        ];

        assert!(!assembly_ready(&chunks, 2));
    }

    #[test]
    fn finality_is_read_from_the_row_not_inferred_from_the_highest_index() {
        let chunks = vec![
            chunk(0, "done", Some("zero."), false),
            chunk(1, "done", Some("one."), false),
        ];
        assert_eq!(final_chunk_index(&chunks), None, "nobody claimed finality");

        let chunks = vec![
            chunk(0, "done", Some("zero."), true),
            chunk(1, "done", Some("one."), false),
        ];
        assert_eq!(final_chunk_index(&chunks), Some(0));
    }

    #[test]
    fn the_event_id_is_a_pure_function_of_the_encounter() {
        let encounter = Uuid::from_u128(0xE1);
        let other = Uuid::from_u128(0xE2);

        // Same encounter, same id — twice, and forever. This is what turns a
        // duplicate assembly into a no-op instead of a second event (§C4).
        assert_eq!(
            transcript_event_id(encounter),
            transcript_event_id(encounter)
        );
        assert_ne!(transcript_event_id(encounter), transcript_event_id(other));

        // Pinned literally, because the value is a persisted identity: if this
        // changes, every already-published event silently stops matching its own
        // encounter and a replay would double-publish.
        assert_eq!(
            transcript_event_id(encounter).to_string(),
            "ffcb87f8-0ab9-5430-bed1-a1a470075423"
        );
        assert_eq!(transcript_event_id(encounter).get_version_num(), 5);
    }

    #[test]
    fn clinical_time_is_the_last_chunk_moment_not_now() {
        let early = Utc::now() - chrono::Duration::minutes(10);
        let late = Utc::now() - chrono::Duration::minutes(2);

        let mut chunks = vec![
            chunk(0, "done", Some("zero."), false),
            chunk(1, "done", Some("one."), true),
        ];
        chunks[0].created_at = late;
        chunks[1].created_at = early;

        // The max, not the last row — index order and time order are independent.
        assert_eq!(clinical_time(&chunks), late);
    }

    #[test]
    fn a_nonsense_duration_cannot_panic_or_wrap() {
        assert_eq!(duration_to_millis(12.44), 12_440);
        assert_eq!(duration_to_millis(0.0), 0);
        assert_eq!(duration_to_millis(-5.0), 0);
        assert_eq!(duration_to_millis(f32::NAN), 0);
        assert_eq!(duration_to_millis(f32::INFINITY), 0);
        assert_eq!(duration_to_millis(f32::MAX), i32::MAX);
    }

    #[test]
    fn a_long_error_chain_is_truncated_on_a_character_boundary() {
        let long = "€".repeat(400); // 1200 bytes, 3 per character
        let capped = truncate_error(&long);

        assert!(capped.ends_with('…'));
        assert!(capped.len() <= 500 + '…'.len_utf8());
        assert_eq!(truncate_error("short"), "short");
    }

    #[test]
    fn wrapped_ai_errors_keep_their_retry_classification() {
        let transient = anyhow::Error::new(AiError::Transport("temporary".to_string()))
            .context("summarize the assembled encounter");
        assert!(retryable_ai_error(&transient));
        assert_eq!(non_retryable_ai_error(&transient), None);

        let permanent = anyhow::Error::new(AiError::Refused {
            status: 400,
            message: "refused".to_string(),
        })
        .context("summarize the assembled encounter");
        assert!(!retryable_ai_error(&permanent));
        assert_eq!(non_retryable_ai_error(&permanent), Some("provider_refused"));
    }
}
