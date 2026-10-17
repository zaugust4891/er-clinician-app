//! S9 — the client half of the wire contract (compiled only for `feature = "client"`).
//!
//! Two types, two jobs:
//!
//! * [`ServerApi`] speaks the whole HTTP API — URL building, bearer token, JSON
//!   encode/decode, status handling. Some of what it exposes (shifts, export) is
//!   not a storage operation at all, which is exactly why it is its own type.
//! * [`RemoteRepository`] adapts the seven `Repository` methods onto `ServerApi`
//!   so `EventBus`, `ShiftManager`, and `load_patient_note` can talk to the server
//!   without knowing a network is involved.
//!
//! Nothing here decides *policy* — when to publish, when to snapshot, how to
//! replay. That already lives in `er-app`; S9 only moves the existing storage
//! calls across HTTP.

use er_domain::{NoteEvent, PatientId, PatientNote};
use er_store::repository::{Repository, StoredEvent};
use er_store::{StoreError, StoreResult};
use uuid::Uuid;

/// How many events the cursor loop asks for per request. The server clamps to
/// its own `MAX_PAGE`, so this is a request, not a promise.
const DEFAULT_PAGE_SIZE: usize = 500;

/// Decode a successful response body. One place so every endpoint reports a
/// malformed/short body the same way: a transport-class failure, because no
/// server *rejection* is involved — the server said 200 and then lied.
async fn decode<T: serde::de::DeserializeOwned>(resp: reqwest::Response) -> StoreResult<T> {
    resp.json::<T>().await.map_err(StoreError::backend)
}

pub struct ServerApi {
    /// Normalized without a trailing slash, so `url()` can always join with a
    /// leading-slash path and never produce `//v1/...`.
    base: String,
    /// Pasted dev JWT today; a Cognito access token in Phase 5. Same header either way.
    token: String,
    /// Built once and reused — the connection pool (§C8) is the entire point.
    http: reqwest::Client,
}

impl ServerApi {
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Self {
        let base = base.into().trim_end_matches('/').to_owned();
        Self {
            base,
            token: token.into(),
            http: reqwest::Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// Every call funnels through here: attach the bearer token, send, and split
    /// the two failure layers (§C8) into the two `StoreError` variants that mean
    /// different things to a caller:
    ///
    /// * no usable response at all (offline, DNS, timeout) → `Backend`
    /// * a response that says no (401/404/409/…)            → `Remote { status, .. }`
    ///
    /// The status survives translation because the *same* code means different
    /// things per endpoint: a snapshot 404 is normal ("no checkpoint, replay"),
    /// an event-upload 404 is a real error ("that patient isn't registered").
    async fn send(&self, req: reqwest::RequestBuilder) -> StoreResult<reqwest::Response> {
        let resp = req
            // AWS Common Rule Set rejects a missing User-Agent. Keep this static:
            // no device, clinician or patient identifier belongs in the header.
            .header(
                reqwest::header::USER_AGENT,
                concat!("er-note/", env!("CARGO_PKG_VERSION")),
            )
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(StoreError::backend)?; // transport failure
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        // S2 renders every rejection as {"error": "<code>"}. A body that isn't
        // that shape (a proxy's HTML 502, say) degrades to an empty message
        // rather than masking the status code, which is the part callers branch on.
        let message = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v["error"].as_str().map(String::from))
            .unwrap_or_default();
        Err(StoreError::Remote {
            status: status.as_u16(),
            message,
        })
    }

    // -----------------------------------------------------------------------
    // Non-Repository operations — used directly by the CLI (S10)
    // -----------------------------------------------------------------------

    /// Register a client-stampresend returns 200, not 409.
    ///ed patient id (D5.3). Retry-safe: the server's
    /// INSERT is `ON CONFLICT DO NOTHING`, so a
    /// Deviation from the S9 sketch's `StoreResult<()>`: the server already
    /// answers with the full `PatientView`, and S10's roster needs it.
    pub async fn register_patient(
        &self,
        req: &crate::RegisterPatientRequest,
    ) -> StoreResult<crate::PatientView> {
        let resp = self
            .send(self.http.post(self.url("/v1/patients")).json(req))
            .await?;
        decode(resp).await
    }

    /// Open a shift. A second start while one is open is a `Remote { status: 409 }`
    /// (`shift_already_active`) — the caller's cue to call [`Self::active_shift`].
    pub async fn start_shift(&self) -> StoreResult<crate::ShiftView> {
        let resp = self.send(self.http.post(self.url("/v1/shifts"))).await?;
        decode(resp).await
    }

    /// The resume request a fresh device opens with (D5.6). `Ok(None)` — the
    /// server's 404 — is the normal first-boot answer, not a failure.
    pub async fn active_shift(&self) -> StoreResult<Option<crate::ShiftView>> {
        match self
            .send(self.http.get(self.url("/v1/shifts/active")))
            .await
        {
            Ok(resp) => decode(resp).await.map(Some),
            Err(StoreError::Remote { status: 404, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub async fn get_shift(&self, shift_id: Uuid) -> StoreResult<crate::ShiftView> {
        let resp = self
            .send(self.http.get(self.url(&format!("/v1/shifts/{shift_id}"))))
            .await?;
        decode(resp).await
    }

    /// Close a shift. Idempotent server-side: ending an ended shift returns the
    /// same ended view rather than 409.
    pub async fn end_shift(&self, shift_id: Uuid) -> StoreResult<crate::ShiftView> {
        let resp = self
            .send(
                self.http
                    .post(self.url(&format!("/v1/shifts/{shift_id}/end"))),
            )
            .await?;
        decode(resp).await
    }

    /// The read model for a patient's note (`GET /v1/patients/{id}/note`).
    pub async fn note(&self, patient_id: Uuid) -> StoreResult<crate::NoteView> {
        let resp = self
            .send(
                self.http
                    .get(self.url(&format!("/v1/patients/{patient_id}/note"))),
            )
            .await?;
        decode(resp).await
    }

    /// Clipboard export. `text/plain`, so it is the one endpoint that does not
    /// go through `decode` — there is no JSON to parse.
    pub async fn export(&self, patient_id: Uuid) -> StoreResult<String> {
        let resp = self
            .send(
                self.http
                    .get(self.url(&format!("/v1/patients/{patient_id}/note/export"))),
            )
            .await?;
        resp.text().await.map_err(StoreError::backend)
    }

    pub async fn stats(&self) -> StoreResult<crate::StatsView> {
        let resp = self.send(self.http.get(self.url("/v1/stats"))).await?;
        decode(resp).await
    }

    /// Create or retrieve the idempotent server-side MDM job.
    pub async fn request_mdm(
        &self,
        patient_id: Uuid,
        job_id: Uuid,
    ) -> StoreResult<crate::MdmAccepted> {
        let resp = self
            .send(
                self.http
                    .post(self.url(&format!("/v1/patients/{patient_id}/mdm")))
                    .json(&crate::MdmRequest { job_id }),
            )
            .await?;
        decode(resp).await
    }

    /// Observe a job. The body contains status metadata only; note prose arrives
    /// through the normal event replay path after completion.
    pub async fn job(&self, job_id: Uuid) -> StoreResult<crate::JobView> {
        let resp = self
            .send(self.http.get(self.url(&format!("/v1/jobs/{job_id}"))))
            .await?;
        decode(resp).await
    }

    // -----------------------------------------------------------------------
    // Audio upload + transcription observation (Step 6 S8)
    // -----------------------------------------------------------------------

    /// Ask the API for a short-lived, single-object upload URL.
    pub async fn request_audio_upload(
        &self,
        patient_id: Uuid,
        encounter_id: Uuid,
        req: &crate::AudioUploadRequest,
    ) -> StoreResult<crate::AudioUploadResponse> {
        let resp = self
            .send(
                self.http
                    .post(self.url(&format!(
                        "/v1/patients/{patient_id}/encounters/{encounter_id}/audio"
                    )))
                    .json(req),
            )
            .await?;
        decode(resp).await
    }

    /// Confirm that object storage holds a chunk and enqueue its transcription.
    pub async fn complete_audio(&self, chunk_id: Uuid) -> StoreResult<crate::ChunkView> {
        let resp = self
            .send(
                self.http
                    .post(self.url(&format!("/v1/audio/{chunk_id}/complete"))),
            )
            .await?;
        decode(resp).await
    }

    /// Read the server-side ledger state for one audio chunk.
    pub async fn audio_chunk(&self, chunk_id: Uuid) -> StoreResult<crate::ChunkView> {
        let resp = self
            .send(self.http.get(self.url(&format!("/v1/audio/{chunk_id}"))))
            .await?;
        decode(resp).await
    }

    /// Upload bytes through a presigned URL.
    ///
    /// This deliberately bypasses the authenticated request helper: the URL is
    /// its own authorization, so attaching the clinician's bearer token would
    /// cross an authentication boundary unnecessarily. The content type is
    /// signed and therefore must match the upload intent exactly.
    pub async fn put_presigned(
        &self,
        url: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> StoreResult<()> {
        let response = self
            .http
            .put(url)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(bytes)
            .send()
            .await
            // A reqwest transport error can include the complete presigned URL.
            // Replace it at this boundary so the credential never reaches a CLI
            // error, log line, or crash report.
            .map_err(|_| {
                StoreError::backend(std::io::Error::other("presigned upload transport failure"))
            })?;

        if response.status().is_success() {
            return Ok(());
        }

        Err(StoreError::Remote {
            status: response.status().as_u16(),
            // S3-compatible stores return XML bodies that may contain request
            // metadata. The status is enough for the caller's safe category.
            message: "object_storage_rejected_upload".to_string(),
        })
    }
}

/// The storage trait, served over HTTP. Interchangeable with `SqliteRepository`
/// and `PostgresRepository` — that interchangeability is what S11's shared
/// contract suite exists to prove.
pub struct RemoteRepository {
    api: ServerApi,
    page_size: usize,
}

impl RemoteRepository {
    pub fn new(base: impl Into<String>, token: impl Into<String>) -> Self {
        Self::with_page_size(base, token, DEFAULT_PAGE_SIZE)
    }

    /// Same thing with the cursor page size chosen explicitly. Tuning knob in
    /// production; in tests it is how the multi-page loop gets exercised without
    /// seeding 500+ events.
    pub fn with_page_size(
        base: impl Into<String>,
        token: impl Into<String>,
        page_size: usize,
    ) -> Self {
        Self {
            api: ServerApi::new(base, token),
            page_size: page_size.max(1),
        }
    }

    /// The non-storage half of the API, on the same connection pool — so a CLI
    /// holding one `RemoteRepository` doesn't need a second HTTP client for shifts.
    pub fn api(&self) -> &ServerApi {
        &self.api
    }

    fn url(&self, path: &str) -> String {
        self.api.url(path)
    }
}

#[async_trait::async_trait]
impl Repository for RemoteRepository {
    async fn append_event(
        &self,
        patient_id: PatientId,
        event_id: Uuid,
        event: NoteEvent,
    ) -> StoreResult<u64> {
        // EventBus publishes one event at a time, so this is a batch of one. The
        // endpoint is batch-shaped anyway so Phase 5's offline queue can drain a
        // backlog without a new wire contract.
        let body = crate::AppendEventsRequest {
            events: vec![crate::EventUp { event_id, event }],
        };
        let resp = self
            .api
            .send(
                self.api
                    .http
                    .post(self.url(&format!("/v1/patients/{patient_id}/events")))
                    .json(&body), // serializes and sets Content-Type
            )
            .await?;
        let acks: crate::AppendEventsResponse = decode(resp).await?;
        // A resent event_id gets the ORIGINAL sequence_id back (§C2), which is
        // what makes at-least-once delivery add up to exactly-once storage.
        acks.results.first().map(|a| a.sequence_id).ok_or_else(|| {
            StoreError::backend(std::io::Error::other(
                "empty event acknowledgment from server",
            ))
        })
    }

    async fn get_events(&self, patient_id: PatientId) -> StoreResult<Vec<StoredEvent>> {
        self.get_events_since(patient_id, 0).await
    }

    async fn get_events_since(
        &self,
        patient_id: PatientId,
        sequence_id: u64,
    ) -> StoreResult<Vec<StoredEvent>> {
        // The §C3 cursor loop. The trait promises the complete delta, so paging
        // is the client's problem, not the caller's.
        let mut all = Vec::new();
        let mut cursor = sequence_id;
        loop {
            let resp = self
                .api
                .send(self.api.http.get(self.url(&format!(
                    "/v1/patients/{patient_id}/events?since={cursor}&limit={}",
                    self.page_size
                ))))
                .await?;
            let page: crate::EventsPage = decode(resp).await?;
            let has_more = page.has_more;
            // Advance to the last sequence actually received — never cursor + 1,
            // never the page length: sequence ids are shared across patients, so
            // this stream's numbers have gaps by design.
            match page.events.last() {
                Some(last) => cursor = last.sequence_id,
                // "More to come" with nothing in the page would leave the cursor
                // unchanged and spin forever. The server must never say this
                // (S11 pins it); the client's own seatbelt is to fail loudly
                // rather than hang — or, worse, hand back a silently truncated
                // event stream that would replay into a wrong note.
                None if has_more => {
                    return Err(StoreError::backend(std::io::Error::other(
                        "server returned an empty page with has_more = true",
                    )));
                }
                None => break,
            }
            all.extend(page.events.into_iter().map(|e| StoredEvent {
                sequence_id: e.sequence_id,
                event_id: e.event_id,
                event: e.event,
                occurred_at: e.occurred_at,
            }));
            if !has_more {
                break;
            }
        }
        Ok(all)
    }

    async fn save_snapshot(
        &self,
        patient_id: PatientId,
        sequence_id: u64,
        snapshot: &PatientNote,
    ) -> StoreResult<()> {
        let body = crate::SnapshotUp {
            sequence_id,
            snapshot: snapshot.clone(),
        };
        // 200 with an empty body; there is nothing to decode, only to check.
        self.api
            .send(
                self.api
                    .http
                    .put(self.url(&format!("/v1/patients/{patient_id}/snapshot")))
                    .json(&body),
            )
            .await?;
        Ok(())
    }

    async fn get_latest_snapshot(
        &self,
        patient_id: PatientId,
    ) -> StoreResult<Option<(u64, PatientNote)>> {
        match self
            .api
            .send(
                self.api
                    .http
                    .get(self.url(&format!("/v1/patients/{patient_id}/snapshot"))),
            )
            .await
        {
            Ok(resp) => {
                let down: crate::SnapshotDown = decode(resp).await?;
                let mut note = down.snapshot;
                // SAME rule as the SQLite/Postgres impls: `PatientNote::state` is
                // #[serde(skip)], so deserialization cannot restore the state
                // trait object and the first transition would panic without this.
                note.hydrate();
                Ok(Some((down.sequence_id, note)))
            }
            // Normal, not exceptional: no checkpoint (or a checksum-rejected one)
            // → the caller replays from 0. Same shape as the local `Ok(None)`.
            Err(StoreError::Remote { status: 404, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn event_count(&self) -> StoreResult<usize> {
        Ok(self.api.stats().await?.event_count as usize)
    }

    async fn snapshot_count(&self) -> StoreResult<usize> {
        // Deliberately a second /v1/stats call rather than a cached pair: these
        // are diagnostic reads, and a cache here would trade a round trip for
        // stale counts in exactly the tests that check freshness.
        Ok(self.api.stats().await?.snapshot_count as usize)
    }
}
