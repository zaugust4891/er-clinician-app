//! Real-provider synthetic gate. Credentials arrive on stdin; no response bodies
//! or errors from HTTP/serde/providers are ever formatted into console output.
use std::{io::Read, time::Duration};

use chrono::Utc;
use er_api::{AudioUploadRequest, RegisterPatientRequest, client::RemoteRepository};
use er_domain::result::{VitalSign, VitalType};
use er_domain::{CaptureMode, ClinicalResult, NoteEvent, ResultSource, ResultType};
use er_store::Repository;
use serde::Deserialize;
use uuid::Uuid;

type Result<T> = std::result::Result<T, &'static str>;

#[derive(Deserialize)]
struct Input {
    base_url: String,
    token_a: String,
    token_b: String,
    id_token: String,
    unprovisioned_token: String,
    audio_path: String,
    expected_terms: Vec<String>,
}

fn check(ok: bool, label: &'static str) -> Result<()> {
    if ok { Ok(()) } else { Err(label) }
}

async fn append(repo: &RemoteRepository, patient: Uuid, event: NoteEvent) -> Result<()> {
    let id = Uuid::now_v7();
    let seq = repo
        .append_event(patient, id, event.clone())
        .await
        .map_err(|_| "append")?;
    let retry = repo
        .append_event(patient, id, event)
        .await
        .map_err(|_| "append_retry")?;
    check(seq == retry, "event_idempotency")
}

async fn denial(
    input: &Input,
    token: &str,
    path: &str,
    method: reqwest::Method,
    expected: u16,
) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "http_client")?;
    let response = client
        .request(method, format!("{}{path}", input.base_url))
        .header("User-Agent", "er-note/synthetic-gate")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| "denial_transport")?;
    check(response.status().as_u16() == expected, "denial_status")
}

async fn lifecycle(input: &Input) -> Result<()> {
    let parsed = reqwest::Url::parse(&input.base_url).map_err(|_| "api_url")?;
    check(
        parsed.scheme() == "https"
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.query().is_none()
            && parsed.fragment().is_none()
            && parsed.path() == "/",
        "api_https_origin",
    )?;
    check(
        !input.expected_terms.is_empty()
            && input.expected_terms.iter().all(|s| !s.trim().is_empty()),
        "fixture_expectations",
    )?;
    for token in ["invalid-token", input.id_token.as_str()] {
        denial(input, token, "/v1/stats", reqwest::Method::GET, 401).await?;
    }
    denial(
        input,
        &input.unprovisioned_token,
        "/v1/stats",
        reqwest::Method::GET,
        401,
    )
    .await?;
    let repo = RemoteRepository::with_page_size(&input.base_url, &input.token_a, 2);
    let foreign = RemoteRepository::new(&input.base_url, &input.token_b);
    // Ensure B is provisioned; otherwise its denial would be a meaningless auth failure.
    foreign
        .api()
        .stats()
        .await
        .map_err(|_| "second_tenant_auth")?;
    // A prior interrupted smoke may leave an open shift. Resume this synthetic user's shift.
    let shift = match repo
        .api()
        .active_shift()
        .await
        .map_err(|_| "active_shift")?
    {
        Some(shift) => shift,
        None => repo.api().start_shift().await.map_err(|_| "start_shift")?,
    };
    let patient = Uuid::now_v7();
    let encounter = Uuid::now_v7();
    let chunk = Uuid::now_v7();
    let job = Uuid::now_v7();
    // Safe correlations permit inspection after timeout without resubmitting another job.
    println!("synthetic_ids patient={patient} encounter={encounter} chunk={chunk} job={job}");
    repo.api()
        .register_patient(&RegisterPatientRequest {
            patient_id: patient,
            chief_complaint: "synthetic deployment verification".into(),
            display_label: None,
        })
        .await
        .map_err(|_| "register_patient")?;
    for path in [
        format!("/v1/patients/{patient}/events"),
        format!("/v1/patients/{patient}/snapshot"),
        format!("/v1/patients/{patient}/note"),
        format!("/v1/patients/{patient}/note/export"),
        format!("/v1/shifts/{}", shift.shift_id),
    ] {
        denial(input, &input.token_b, &path, reqwest::Method::GET, 404).await?;
    }
    denial(
        input,
        &input.token_b,
        &format!("/v1/shifts/{}/end", shift.shift_id),
        reqwest::Method::POST,
        404,
    )
    .await?;
    append(
        &repo,
        patient,
        NoteEvent::audio_started(patient, encounter, CaptureMode::Ambient),
    )
    .await?;
    let intent = repo
        .api()
        .request_audio_upload(
            patient,
            encounter,
            &AudioUploadRequest {
                chunk_id: chunk,
                chunk_index: 0,
                is_final: true,
                content_type: "audio/wav".into(),
            },
        )
        .await
        .map_err(|_| "audio_intent")?;
    check(
        intent.chunk_id == chunk && intent.upload_url.starts_with("https://"),
        "upload_contract",
    )?;
    let audio = std::fs::read(&input.audio_path).map_err(|_| "audio_fixture")?;
    repo.api()
        .put_presigned(&intent.upload_url, audio, "audio/wav")
        .await
        .map_err(|_| "audio_put")?;
    for _ in 0..2 {
        let completed = repo
            .api()
            .complete_audio(chunk)
            .await
            .map_err(|_| "audio_complete")?;
        check(completed.chunk_id == chunk, "chunk_idempotency")?;
    }
    append(&repo, patient, NoteEvent::audio_stopped(patient, encounter)).await?;
    denial(
        input,
        &input.token_b,
        &format!("/v1/audio/{chunk}"),
        reqwest::Method::GET,
        404,
    )
    .await?;
    denial(
        input,
        &input.token_b,
        &format!("/v1/audio/{chunk}/complete"),
        reqwest::Method::POST,
        404,
    )
    .await?;
    tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            let status = repo
                .api()
                .audio_chunk(chunk)
                .await
                .map_err(|_| "audio_status")?;
            check(status.status != "failed", "transcription_failed")?;
            let events = repo
                .get_events(patient)
                .await
                .map_err(|_| "summary_events")?;
            if events
                .iter()
                .any(|e| e.event.event_type() == "EncounterSummarized")
            {
                let transcript = events
                    .iter()
                    .find_map(|e| match &e.event {
                        NoteEvent::TranscriptReady { text, .. } => Some(text.to_lowercase()),
                        _ => None,
                    })
                    .ok_or("transcript_missing")?;
                check(
                    input
                        .expected_terms
                        .iter()
                        .all(|s| transcript.contains(&s.to_lowercase())),
                    "transcript_facts",
                )?;
                check(
                    status.status == "done" && status.duration_ms.unwrap_or(0) > 0,
                    "real_transcription",
                )?;
                return Ok::<(), &'static str>(());
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    })
    .await
    .map_err(|_| "summary_incomplete_deadline")??;
    println!("transcription_summary_passed");
    append(
        &repo,
        patient,
        NoteEvent::result_received(
            patient,
            ClinicalResult {
                id: Uuid::now_v7(),
                patient_id: patient,
                source: ResultSource::Manual,
                timestamp: Utc::now(),
                result_type: ResultType::Vital(VitalSign {
                    vital_type: VitalType::HeartRate,
                    value: 72.0,
                    unit: "bpm".into(),
                }),
            },
            Utc::now(),
        ),
    )
    .await?;
    for _ in 0..2 {
        let accepted = repo
            .api()
            .request_mdm(patient, job)
            .await
            .map_err(|_| "mdm_request")?;
        check(accepted.job_id == job, "mdm_idempotency")?;
    }
    denial(
        input,
        &input.token_b,
        &format!("/v1/jobs/{job}"),
        reqwest::Method::GET,
        404,
    )
    .await?;
    tokio::time::timeout(Duration::from_secs(300), async {
        loop {
            let status = repo.api().job(job).await.map_err(|_| "mdm_status")?;
            check(status.status != "failed", "mdm_failed")?;
            if status.status == "done" {
                return Ok::<(), &'static str>(());
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    })
    .await
    .map_err(|_| "mdm_incomplete_deadline")??;
    let fresh = RemoteRepository::with_page_size(&input.base_url, &input.token_a, 2);
    let events = fresh
        .get_events(patient)
        .await
        .map_err(|_| "fresh_replay")?;
    for kind in ["TranscriptReady", "EncounterSummarized", "MdmGenerated"] {
        check(
            events
                .iter()
                .filter(|e| e.event.event_type() == kind)
                .count()
                == 1,
            "derived_event_idempotency",
        )?;
    }
    append(&repo, patient, NoteEvent::review_ready(patient)).await?;
    append(&repo, patient, NoteEvent::sign(patient)).await?;
    let events = fresh
        .get_events(patient)
        .await
        .map_err(|_| "signed_replay")?;
    let sequence = events.last().ok_or("empty_replay")?.sequence_id;
    let note = er_domain::replay::replay(
        patient,
        &events.into_iter().map(|e| e.event).collect::<Vec<_>>(),
    );
    fresh
        .save_snapshot(patient, sequence, &note)
        .await
        .map_err(|_| "snapshot_save")?;
    check(
        fresh
            .get_latest_snapshot(patient)
            .await
            .map_err(|_| "snapshot_reload")?
            .is_some(),
        "snapshot_missing",
    )?;
    let view = fresh.api().note(patient).await.map_err(|_| "note_reload")?;
    check(
        view.phase == "Signed"
            && view.mdm.as_ref().is_some_and(|s| !s.trim().is_empty())
            && view.results.len() == 1,
        "signed_note",
    )?;
    let export = fresh
        .api()
        .export(patient)
        .await
        .map_err(|_| "export")?
        .to_lowercase();
    check(
        input
            .expected_terms
            .iter()
            .all(|s| export.contains(&s.to_lowercase()))
            && export.contains("72"),
        "export_facts",
    )?;
    repo.api()
        .end_shift(shift.shift_id)
        .await
        .map_err(|_| "end_shift")?;
    check(
        repo.api()
            .active_shift()
            .await
            .map_err(|_| "closed_shift")?
            .is_none(),
        "shift_still_active",
    )?;
    println!("auth_tenancy_replay_snapshot_sign_export_passed");
    Ok(())
}

#[tokio::main]
async fn main() {
    // A default panic hook could print an assertion payload. This process only
    // emits fixed labels even for unexpected failures.
    std::panic::set_hook(Box::new(|_| eprintln!("smoke_internal_failure")));
    let mut bytes = Vec::new();
    let input = std::io::stdin()
        .take(131_072)
        .read_to_end(&mut bytes)
        .ok()
        .and_then(|_| serde_json::from_slice::<Input>(&bytes).ok());
    let result = match input {
        Some(input) => tokio::time::timeout(Duration::from_secs(900), lifecycle(&input))
            .await
            .unwrap_or(Err("smoke_deadline")),
        None => Err("smoke_input"),
    };
    if let Err(label) = result {
        eprintln!("{label}");
        std::process::exit(1);
    }
}
