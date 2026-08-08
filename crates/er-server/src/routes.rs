use axum::{Json, Router, http::StatusCode, middleware, response::IntoResponse, routing::{get, post}};
use axum::extract::{State, Path};

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
        .route("/v1/patients/{id}/events", post(append_events))
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

async fn patient_note(State(state): State<AppState>, authed: AuthedClinician, Path(patient_id): Path<Uuid>) -> Result<Json<serde_json::Value>, ApiError> {
    let repo = state.repo_for(&authed); // the ONLY data-access pattern
    // Guard first, load second. "Not yours" and "doesn't exist" both become 404 here,
    // which frees the load below to treat "no events" as the honest answer it is:
    // a registered patient nothing has happened to yet.
    ensure_patient_in_org(&repo, patient_id).await?;
    // `?` alone — no map_err. `impl From<StoreError> for ApiError` (error.rs) turns a
    // storage failure into ApiError::Internal, which logs the cause and tells the
    // caller nothing but 500.
    let events = repo.get_events(patient_id).await?;
    // replay wants `&[NoteEvent]`: collect the `.event` field of each StoredEvent into a
    // Vec that owns them, then pass a shared borrow (`&Vec<T>` coerces to `&[T]`).
    // An empty Vec is fine: replay folds zero events onto PatientNote::new, whose
    // phase is Registered.
    let note_events: Vec<_> = events.into_iter().map(|e| e.event).collect();
    let note = er_domain::replay::replay(patient_id, &note_events); // returns PatientNote, not Result
    Ok(Json(json!({ "phase": format!("{:?}", note.phase), "hpi": note.note_sections.hpi.clone() })))
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












/*  
/// State(state) is the same as 
/// async fn(wrapper: State<AppState>) {
///     let State(state) = wrapper     
/// }
/// or
/// let state = state.0  
*/
