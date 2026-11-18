//! Network work runs outside the command owner. Every request is reserved on disk.
use crate::{DeviceError as Error, DeviceHandle, Result};
use er_api::{client::ServerApi, *};
use er_store::{StoreError, sync::SyncBinding};
use std::sync::Arc;
use uuid::Uuid;

pub trait AccessTokenProvider: Send + Sync {
    /// Platform auth owns sign-in/refresh. Tokens never enter SQLite.
    fn access_token(&self, force_refresh: bool) -> Result<String>;
}
#[derive(Clone)]
pub struct SyncConfig {
    pub origin: String,
    pub organization: Uuid,
    pub clinician: Uuid,
}
impl SyncConfig {
    pub fn binding(&self) -> Result<SyncBinding> {
        let url = reqwest::Url::parse(&self.origin).map_err(|_| Error::InvalidInput)?;
        if self.organization.is_nil()
            || self.clinician.is_nil()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
            || url.host_str().is_none()
        {
            return Err(Error::InvalidInput);
        }
        if url.scheme() != "https"
            && !(url.scheme() == "http" && cfg!(any(test, feature = "development-http")))
        {
            return Err(Error::InvalidInput);
        }
        Ok(SyncBinding {
            origin: url.origin().ascii_serialization(),
            organization: self.organization,
            clinician: self.clinician,
        })
    }
}
#[derive(Clone, Debug)]
pub struct SyncView {
    pub status: String,
    pub queued_events: u64,
    pub awaiting_download: u64,
    pub conflicts: u64,
    pub retry_at_ms: Option<i64>,
    pub local_only_preferences: bool,
    pub local_only_context: bool,
}

/// The server limits both count (500) and encoded JSON bytes (2 MiB).
/// Leave space below the byte cap and count escaped strings as actually sent.
pub(crate) fn bounded_event_batch(events: Vec<er_store::StoredEvent>) -> Result<Vec<EventUp>> {
    let mut batch = Vec::new();
    let mut bytes = 13usize;
    for row in events.into_iter().take(500) {
        let event = EventUp {
            event_id: row.event_id,
            event: row.event,
        };
        let size = serde_json::to_vec(&event)
            .map_err(|_| Error::Recovery)?
            .len()
            + 1;
        if bytes + size > 1_900_000 {
            if batch.is_empty() {
                return Err(Error::InvalidInput);
            }
            break;
        }
        bytes += size;
        batch.push(event);
    }
    Ok(batch)
}

pub(crate) enum Action {
    Discover {
        local: Option<Uuid>,
    },
    Start {
        local: Uuid,
    },
    Roster {
        local: Uuid,
        remote: Uuid,
    },
    Register {
        patient: Uuid,
        complaint: String,
        room: Option<String>,
    },
    Pull {
        patient: Uuid,
        cursor: u64,
    },
    Push {
        patient: Uuid,
        events: Vec<EventUp>,
    },
    ReadAudit {
        patient: Uuid,
    },
    WriteAudit {
        patient: Uuid,
        body: CaptureAuditWrite,
    },
    End {
        local: Uuid,
        remote: Uuid,
    },
    /// Mint (or re-mint) the upload intent and PUT the verified bytes. The
    /// presigned URL lives only inside this action; it is never stored.
    UploadAudio {
        patient: Uuid,
        chunk: AudioJob,
    },
    /// Ask the server to verify the object and queue transcription. Retried with
    /// the same chunk ID until it answers.
    CompleteAudio {
        chunk: AudioJob,
    },
}
#[derive(Clone)]
pub(crate) struct AudioJob {
    pub chunk_id: Uuid,
    pub segment: Uuid,
    pub index: u32,
    pub is_final: bool,
    pub content_type: String,
    pub path: std::path::PathBuf,
    pub byte_len: u64,
    pub sha256: String,
}
pub(crate) enum Answer {
    Discovered(Option<Uuid>, Option<er_api::ShiftView>),
    Started(Uuid, er_api::ShiftView),
    Roster(Uuid, er_api::ShiftView),
    Registered(Uuid, er_api::PatientView),
    Page(Uuid, u64, EventsPage),
    Acknowledged(Uuid, Vec<Uuid>, AppendEventsResponse),
    AuditRead(Uuid, CaptureAuditView),
    AuditSaved(Uuid, CaptureAuditV1, CaptureAuditView),
    Ended(Uuid, er_api::ShiftView),
    AudioUploaded(Uuid),
    AudioCompleted(AudioJob, ChunkView),
    /// The server has no object (or no row) for an uploaded chunk: PUT again.
    AudioReupload(Uuid),
    /// A transient failure the next round retries, with the reason kept visible.
    AudioRetry(Uuid, &'static str),
    /// A permanent refusal for this chunk.
    AudioRejected(Uuid, &'static str),
}
/// Server refusal codes are static; anything else is recorded generically.
fn refusal_code(message: &str) -> &'static str {
    const KNOWN: &[&str] = &[
        "chunk_index_taken",
        "final_chunk_already_set",
        "chunk_id_mismatch",
        "content_type_unsupported",
        "chunk_index_too_large",
        "chunk_content_type_mismatch",
        "chunk_empty",
        "chunk_too_large",
        "chunk_length_missing",
        "chunk_etag_missing",
        "chunk_version_missing",
    ];
    KNOWN
        .iter()
        .find(|k| **k == message)
        .copied()
        .unwrap_or("server_refused")
}
/// 400/409-class answers belong to one chunk, not to the whole sync session.
/// 401, 404, 429 and transport failures keep their usual session handling.
fn audio_refusal(chunk: Uuid, e: StoreError) -> std::result::Result<Answer, StoreError> {
    match e {
        StoreError::Remote {
            status: 400 | 409 | 413 | 422,
            message,
        } => Ok(Answer::AudioRejected(chunk, refusal_code(&message))),
        other => Err(other),
    }
}
/// Read the chunk and prove it is the file that was validated at hand-off.
fn read_verified(job: &AudioJob) -> std::result::Result<Vec<u8>, &'static str> {
    use sha2::Digest;
    let bytes = std::fs::read(&job.path).map_err(|_| "file_missing")?;
    if bytes.len() as u64 != job.byte_len
        || format!("{:x}", sha2::Sha256::digest(&bytes)) != job.sha256
    {
        return Err("file_changed");
    }
    Ok(bytes)
}
impl Action {
    async fn perform(self, api: &ServerApi) -> std::result::Result<Answer, StoreError> {
        Ok(match self {
            Self::Discover { local } => Answer::Discovered(local, api.active_shift().await?),
            Self::Start { local } => Answer::Started(local, api.start_shift().await?),
            Self::Roster { local, remote } => Answer::Roster(local, api.get_shift(remote).await?),
            Self::Register {
                patient,
                complaint,
                room,
            } => Answer::Registered(
                patient,
                api.register_patient(&RegisterPatientRequest {
                    patient_id: patient,
                    chief_complaint: complaint,
                    display_label: room,
                })
                .await?,
            ),
            Self::Pull { patient, cursor } => {
                Answer::Page(patient, cursor, api.events_page(patient, cursor).await?)
            }
            Self::Push { patient, events } => {
                let ids = events.iter().map(|e| e.event_id).collect();
                Answer::Acknowledged(
                    patient,
                    ids,
                    api.append_batch(patient, &AppendEventsRequest { events })
                        .await?,
                )
            }
            Self::ReadAudit { patient } => {
                Answer::AuditRead(patient, api.capture_audit(patient).await?)
            }
            Self::WriteAudit { patient, body } => {
                let sent = body.audit.clone();
                Answer::AuditSaved(patient, sent, api.save_capture_audit(patient, &body).await?)
            }
            Self::End { local, remote } => Answer::Ended(local, api.end_shift(remote).await?),
            Self::UploadAudio { patient, chunk } => {
                let bytes = match read_verified(&chunk) {
                    Ok(bytes) => bytes,
                    Err(reason) => return Ok(Answer::AudioRejected(chunk.chunk_id, reason)),
                };
                let request = AudioUploadRequest {
                    chunk_id: chunk.chunk_id,
                    chunk_index: chunk.index,
                    is_final: chunk.is_final,
                    content_type: chunk.content_type.clone(),
                };
                let intent = match api
                    .request_audio_upload(patient, chunk.segment, &request)
                    .await
                {
                    Ok(intent) => intent,
                    Err(e) => return audio_refusal(chunk.chunk_id, e),
                };
                if intent.chunk_id != chunk.chunk_id {
                    return Ok(Answer::AudioRetry(chunk.chunk_id, "intent_mismatch"));
                }
                // `put_presigned` already strips the URL from its errors.
                match api
                    .put_presigned(&intent.upload_url, bytes, &chunk.content_type)
                    .await
                {
                    Ok(()) => Answer::AudioUploaded(chunk.chunk_id),
                    Err(_) => Answer::AudioRetry(chunk.chunk_id, "upload_failed"),
                }
            }
            Self::CompleteAudio { chunk } => match api.complete_audio(chunk.chunk_id).await {
                Ok(view) => Answer::AudioCompleted(chunk, view),
                Err(StoreError::Remote { status: 404, .. }) => {
                    Answer::AudioReupload(chunk.chunk_id)
                }
                Err(StoreError::Remote {
                    status: 409,
                    message,
                }) if message == "chunk_not_uploaded" => Answer::AudioReupload(chunk.chunk_id),
                Err(e) => return audio_refusal(chunk.chunk_id, e),
            },
        })
    }
}
impl DeviceHandle {
    pub fn sync_status(&self) -> Result<SyncView> {
        self.execute(false, |c| c.sync_view())
    }
    pub fn poll_synchronization(
        &self,
        config: SyncConfig,
        provider: Arc<dyn AccessTokenProvider>,
    ) -> Result<SyncView> {
        self.run_synchronization(config, provider, false)
    }
    pub fn synchronize(
        &self,
        config: SyncConfig,
        provider: Arc<dyn AccessTokenProvider>,
    ) -> Result<SyncView> {
        self.run_synchronization(config, provider, true)
    }
    fn run_synchronization(
        &self,
        config: SyncConfig,
        provider: Arc<dyn AccessTokenProvider>,
        force: bool,
    ) -> Result<SyncView> {
        let _gate = self.sync_gate.try_lock().map_err(|_| Error::Busy)?;
        let binding = config.binding()?;
        // This scope is the dedicated network worker; the core/runtime owner stays free.
        std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| Error::Internal)?;
                    rt.block_on(self.synchronize_inner(binding, provider, force))
                })
                .join()
                .unwrap_or(Err(Error::Internal))
        })
    }
    async fn synchronize_inner(
        &self,
        binding: SyncBinding,
        provider: Arc<dyn AccessTokenProvider>,
        force: bool,
    ) -> Result<SyncView> {
        let check = binding.clone();
        if !self.execute(false, move |c| c.sync_can_connect(&check))? {
            return self.sync_status();
        }
        if force {
            self.execute(true, |c| c.request_sync_refresh())?;
        } else if !self.execute(false, |c| c.scheduled_sync_due())? {
            return self.sync_status();
        }
        let token = match provider.access_token(false) {
            Ok(t) if !t.trim().is_empty() => t,
            _ => {
                self.execute(true, |c| c.sync_failure("authentication", 0))?;
                return self.sync_status();
            }
        };
        let mut api = ServerApi::new(&binding.origin, token);
        let mut refreshed = false;
        // Verify the server-resolved organization and clinician before any clinical write.
        loop {
            if !self.execute(true, |c| c.reserve_request())? {
                return self.sync_status();
            }
            match api.me().await {
                Ok(me) => {
                    if me.org_id != binding.organization || me.clinician_id != binding.clinician {
                        return Err(Error::Scope);
                    }
                    let b = binding.clone();
                    self.execute(true, move |c| c.bind_sync(b))?;
                    break;
                }
                Err(StoreError::Remote { status: 401, .. }) if !refreshed => {
                    refreshed = true;
                    match provider.access_token(true) {
                        Ok(t) if !t.trim().is_empty() => api.replace_access_token(t),
                        _ => {
                            self.execute(true, |c| c.sync_failure("authentication", 0))?;
                            return self.sync_status();
                        }
                    }
                }
                Err(e) => {
                    self.record_network_failure(&api, &e)?;
                    return self.sync_status();
                }
            }
        }
        for _ in 0..7 {
            let Some(action) = self.execute(true, |c| c.next_sync_action())? else {
                break;
            };
            if !self.execute(true, |c| c.reserve_request())? {
                break;
            }
            match action.perform(&api).await {
                Ok(answer) => self.execute(true, move |c| c.accept_sync_answer(answer))?,
                Err(StoreError::Remote { status: 401, .. }) if !refreshed => {
                    // The action's durable intent remains pending. Refresh once;
                    // the next round re-verifies membership before retrying it.
                    refreshed = true;
                    let Ok(token) = provider.access_token(true) else {
                        self.execute(true, |c| c.sync_failure("authentication", 0))?;
                        break;
                    };
                    api.replace_access_token(token);
                    if !self.execute(true, |c| c.reserve_request())? {
                        break;
                    }
                    match api.me().await {
                        Ok(me)
                            if me.org_id == binding.organization
                                && me.clinician_id == binding.clinician => {}
                        Ok(_) => return Err(Error::Scope),
                        Err(e) => {
                            self.record_network_failure(&api, &e)?;
                            break;
                        }
                    }
                }
                Err(e) => {
                    self.record_network_failure(&api, &e)?;
                    break;
                }
            }
        }
        self.sync_status()
    }
    fn record_network_failure(&self, api: &ServerApi, e: &StoreError) -> Result<()> {
        let (status, delay) = match e {
            StoreError::Remote {
                status: 401 | 403, ..
            } => ("authentication", 0),
            StoreError::Remote { status: 404, .. } => ("missing_on_server", 0),
            StoreError::Remote { status: 409, .. } => ("conflict", 2_000),
            StoreError::Remote { status: 429, .. } => (
                "rate_limited",
                api.retry_after_seconds()
                    .saturating_mul(1000)
                    .min(i64::MAX as u64) as i64,
            ),
            StoreError::Remote {
                status: 400..=499, ..
            } => ("rejected", 0),
            _ => ("offline", 2_000),
        };
        self.execute(true, move |c| c.sync_failure(status, delay))
    }
}
