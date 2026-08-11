use axum::{Json, Router, http::StatusCode, middleware, response::IntoResponse, routing::{get, post, put}};
use axum::extract::{State, Path, Query};

use er_store::Repository;
use serde_json::json;
use uuid::Uuid;

use crate::repository::PostgresRepository;
use crate::error::ApiError;
use crate::{auth::{self, AuthedClinician}, state::AppState};

/// log identifiers, never content.
/// patient_id = %id is always fine; payload = ?event never is — an event payload can contain a full transcript. 

/// Map from (path, method) -> handler
/// caputre state clone so Middleware's State(state) parameter can be injected as require_auth's State<AppState> first argument.
/// So Router<AppState> becomes Router<()> aka Router which we return from fn router()
/// `Router<()>` can be served by `axum::serve`, because "state `()` means nothing left to inject.
/// '.route_layer' applies only to requests that match a route already on this router. 
/// A request that matches no route here falls through to the verified router's '/healthz' (or 404) without running 'require_auth'
/// '.layer' would wrap everything, including the fallback - which would run auth on unmatched paths and if placed globally, on '/healthz'
/// using '.route_layer' scopes the gate to '/v1' routes and leaves the health probe open (no token) for load balancer
pub fn router(state: AppState) -> Router {
    let v1 = Router::new()
        .route("/v1/me", get(me))
        .route("/v1/patients", post(register_patient))
        .route("/v1/patients/{id}/note", get(patient_note))
        // Same path, two methods: chain onto ONE MethodRouter. A second `.route()`
        // with an identical path string panics at construction ("Overlapping method route").
        .route("/v1/patients/{id}/events", post(append_events).get(events_since))
        .route("/v1/patients/{id}/snapshot", put(put_snapshot).get(get_snapshot))
        .route("/v1/patients/{id}/note/export", get(note_export))
        // `/active` is a literal and `{id}` a capture; axum matches literals first,
        // so the resume route is never swallowed by the by-id route.
        .route("/v1/shifts", post(start_shift))
        .route("/v1/shifts/active", get(active_shift))
        .route("/v1/shifts/{id}", get(get_shift))
        .route("/v1/shifts/{id}/end", post(end_shift))
        .route("/v1/stats", get(stats))
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::middleware::require_auth));
    
    // No auth - load balancer probe. This is the tail expression (no `let`, no `;`),
    // so its value — a Router<()> after `.with_state` — is what `router` returns.
    Router::new()
        .route("/healthz", get(healthz))
        .merge(v1)
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "status": "ok" })))
}

async fn me(authed: AuthedClinician) -> impl IntoResponse {
    Json(json!({ "clinician_id": authed.clinician_id, "org_id": authed.org_id }))
}

async fn patient_note(State(state): State<AppState>, authed: AuthedClinician, Path(patient_id): Path<Uuid>) -> Result<Json<er_api::NoteView>, ApiError> {
    let repo = state.repo_for(&authed); // the ONLY data-access pattern
    // Guard first, load second. 'Patient not in your org' and 'patient doesn't exist' both become 404 here,
    // treats 'no events' ->  a registered patient nothing has happened to yet.
    ensure_patient_in_org(&repo, patient_id).await?;
    // `?` alone — no map_err. `impl From<StoreError> for ApiError` (error.rs) turns a
    // storage failure into ApiError::Internal, which logs the cause and tells the
    // caller nothing but 500.
    let repository: &dyn er_store::Repository = &repo; 
    let loaded_note = er_app::load_patient_note(repository, patient_id).await;
    let loaded_note = loaded_note
        .map_err(|e| ApiError::Internal(anyhow::Error::new(e)))?;
    let patient_note = loaded_note.unwrap_or_else(|| er_domain::PatientNote::new(patient_id));
    Ok(Json(er_api::NoteView::from(&patient_note)))
}

async fn ensure_patient_in_org(repo: &PostgresRepository, patient_id: Uuid) -> Result<(), ApiError> {
    if repo.patient_registered(patient_id).await? {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}


async fn register_patient(
    State(state): State<AppState>,
    authed: AuthedClinician,
    Json(body): Json<er_api::RegisterPatientRequest>,
) -> Result<Json<er_api::PatientView>, ApiError> {
    if body.chief_complaint.trim().is_empty() {
        return Err(ApiError::Validation("chief_complaint_required"));
    }
    let repo = state.repo_for(&authed);
    repo.register_patient(
        body.patient_id, 
        authed.clinician_id, 
        &body.chief_complaint, 
        body.display_label.as_deref(),
    ).await?;

    Ok(Json(er_api::PatientView {
        patient_id: body.patient_id,
        chief_complaint: body.chief_complaint,
        display_label: body.display_label,
        created_at: chrono::Utc::now(),
    }))
}


const MAX_EVENT_BATCH: usize = 500; // one shift's worth of taps; far under the body limit

async fn append_events(
    State(state): State<AppState>,
    authed: AuthedClinician,
    Path(patient_id): Path<Uuid>,
    Json(body): Json<er_api::AppendEventsRequest>,
) -> Result<Json<er_api::AppendEventsResponse>, ApiError> {

    if body.events.is_empty() {
        return Err(ApiError::Validation("empty_batch"));
    }

    if body.events.len() > MAX_EVENT_BATCH {
        return Err(ApiError::Validation("batch_too_large"));
    }

    for e in &body.events {
        if e.event.extract_patient_id() != patient_id {
            return Err(ApiError::Validation("event_patient_mismatch"));
        }
    }

    let repo = state.repo_for(&authed);
    ensure_patient_in_org(&repo, patient_id).await?;

    let mut results = Vec::with_capacity(body.events.len());

    for e in body.events {
        let seq = repo.append_event(patient_id, e.event_id, e.event).await?;
        results.push(er_api::EventAck {
            event_id: e.event_id,
            sequence_id: seq 
        });
    }

    Ok(Json(er_api::AppendEventsResponse { results }))
}

#[derive(serde::Deserialize)]
struct SinceParams {
    #[serde(default)]                        // absent → 0 → "from the beginning"
    since: u64,
    #[serde(default = "default_page")]       // absent → 500
    limit: usize,
}
fn default_page() -> usize { 500 }
const MAX_PAGE: usize = 1_000;



async fn events_since(
    State(state): State<AppState>,
    authed: AuthedClinician,
    Path(patient_id): Path<Uuid>,
    Query(q): Query<SinceParams>,           
) -> Result<Json<er_api::EventsPage>, ApiError> {
    let limit = q.limit.clamp(1, MAX_PAGE);
    let repo = state.repo_for(&authed);
    ensure_patient_in_org(&repo, patient_id).await?;


    let mut events = repo.get_events_since(patient_id, q.since).await?;
    let has_more = events.len() > limit;
    events.truncate(limit);

    Ok(Json(er_api::EventsPage {
    events: events.into_iter().map(|se| er_api::EventDown {
        sequence_id: se.sequence_id,
        event_id:    se.event_id,
        event:       se.event,
        occurred_at: se.occurred_at,
    }).collect(),
    has_more,
    }))

}

// ---------------------------------------------------------------------------
// S6 — snapshot sync
// ---------------------------------------------------------------------------

/// Store the client's checkpoint. The body's note is treated as opaque *state*:
/// we never call domain methods on it, so it needs no `hydrate()` here — the
/// `state` trait object is `#[serde(skip)]` and simply never makes the trip.
///
/// The checksum is computed server-side inside `save_snapshot` over the string
/// the server itself serialized, so client/server serialization differences
/// (key order, float formatting) can never raise a false corruption alarm.
/// It guards storage integrity — write-to-read — which is its one job.
async fn put_snapshot(
    State(state): State<AppState>,
    authed: AuthedClinician,
    Path(patient_id): Path<Uuid>,
    Json(body): Json<er_api::SnapshotUp>,
) -> Result<StatusCode, ApiError> {
    let repo = state.repo_for(&authed);
    ensure_patient_in_org(&repo, patient_id).await?;
    repo.save_snapshot(patient_id, body.sequence_id, &body.snapshot).await?;
    Ok(StatusCode::OK)
}

async fn get_snapshot(
    State(state): State<AppState>,
    authed: AuthedClinician,
    Path(patient_id): Path<Uuid>,
) -> Result<Json<er_api::SnapshotDown>, ApiError> {
    let repo = state.repo_for(&authed);
    ensure_patient_in_org(&repo, patient_id).await?;
    match repo.get_latest_snapshot(patient_id).await? {
        Some((sequence_id, snapshot)) => Ok(Json(er_api::SnapshotDown { sequence_id, snapshot })),
        // "No snapshot yet" is a NORMAL 404, not an error: it mirrors the local
        // `Ok(None)` contract, and the client answers it by replaying from since=0.
        // A checksum mismatch lands here too — `get_latest_snapshot` reports a
        // corrupted row as absent precisely so the caller self-heals by replay.
        None => Err(ApiError::NotFound),
    }
}

async fn note_export(State(state): State<AppState>, authed: AuthedClinician, Path(patient_id): Path<Uuid>) -> Result<impl IntoResponse, ApiError> {
    let repo = state.repo_for(&authed);
    ensure_patient_in_org(&repo, patient_id).await?;

    let note = er_app::load_patient_note(&repo, patient_id)
        .await
        .map_err(|e| ApiError::Internal(anyhow::Error::new(e)))?
        .ok_or(ApiError::NotFound)?; // exporting nothing is 404
    // format_note wants a PatientRecord; the server's PHI-minimal patients row maps onto it with the identity
    // fields deliberately empty.
    let row = repo.patient_row(patient_id).await?.ok_or(ApiError::NotFound)?;
    let record = er_domain::PatientRecord {
        id: patient_id,
        first_name: None,
        last_name: None,
        age: None,
        chief_complaint: row.chief_complaint,
        room: row.display_label,
        created_at: row.created_at,
    };

    // `format_results_text` already answers None for an empty slice, so the
    // emptiness check lives there rather than being repeated at every call site.
    let results_text = er_app::export::format_results_text(&note.results);

    let text = er_app::export::format_note(
        &record, &note.note_sections, note.disposition.as_ref(), 
        results_text.as_deref(), &note.medication, &note.diagnoses, &note.data_reviewed
    );

    Ok(([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], text))
    
}

// ---------------------------------------------------------------------------
// S8 — shifts
// ---------------------------------------------------------------------------

/// Build the wire view for a shift, fetching its roster. Kept in one place so
/// every shift response carries the patient list in the same `position` order.
async fn shift_view(
    repo: &PostgresRepository,
    row: crate::repository::ShiftRow,
) -> Result<er_api::ShiftView, ApiError> {
    let roster = repo.shift_roster(row.id).await?;
    Ok(er_api::ShiftView {
        shift_id: row.id,
        started_at: row.started_at,
        ended_at: row.ended_at,
        status: row.status,
        patients: roster
            .into_iter()
            .map(|p| er_api::PatientView {
                patient_id: p.patient_id,
                chief_complaint: p.chief_complaint,
                display_label: p.display_label,
                created_at: p.created_at,
            })
            .collect(),
    })
}

async fn start_shift(
    State(state): State<AppState>,
    authed: AuthedClinician,
) -> Result<Json<er_api::ShiftView>, ApiError> {
    let repo = state.repo_for(&authed);
    // `None` = the partial unique index rejected a second active shift. 409 tells
    // the client to stop trying to start one and `GET /v1/shifts/active` instead.
    match repo.create_shift(authed.clinician_id).await? {
        Some(row) => Ok(Json(shift_view(&repo, row).await?)),
        None => Err(ApiError::Conflict("shift_already_active")),
    }
}

async fn get_shift(
    State(state): State<AppState>,
    authed: AuthedClinician,
    Path(shift_id): Path<Uuid>,
) -> Result<Json<er_api::ShiftView>, ApiError> {
    let repo = state.repo_for(&authed);
    let row = repo.get_shift(shift_id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(shift_view(&repo, row).await?))
}

/// The resume request: the one a fresh device opens with, and the reason
/// `shift.json` can go away. 404 means "you have no open shift" — a normal
/// answer a first-boot clinician gets, then answers with POST /v1/shifts.
async fn active_shift(
    State(state): State<AppState>,
    authed: AuthedClinician,
) -> Result<Json<er_api::ShiftView>, ApiError> {
    let repo = state.repo_for(&authed);
    let row = repo
        .active_shift(authed.clinician_id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(shift_view(&repo, row).await?))
}

async fn end_shift(
    State(state): State<AppState>,
    authed: AuthedClinician,
    Path(shift_id): Path<Uuid>,
) -> Result<Json<er_api::ShiftView>, ApiError> {
    let repo = state.repo_for(&authed);
    let row = repo.end_shift(shift_id).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(shift_view(&repo, row).await?))
}

async fn stats(State(state): State<AppState>, authed: AuthedClinician) -> Result<Json<er_api::StatsView>, ApiError> {
    let repo = state.repo_for(&authed);
    Ok(Json(er_api::StatsView {
        event_count: repo.event_count().await? as u64,
        snapshot_count: repo.snapshot_count().await? as u64,
    }))
}





/*  
/// State(state) is the same as 
/// async fn(wrapper: State<AppState>) {
///     let State(state) = wrapper     
/// }
/// or
/// let state = state.0  
*/
