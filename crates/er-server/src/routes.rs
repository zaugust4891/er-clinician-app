use axum::{Json, Router, http::StatusCode, middleware, response::IntoResponse, routing::get};
use axum::extract::{State, Path};
use er_store::Repository;
use serde_json::json;
use uuid::Uuid;

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
        .route("/v1/patients/{id}/note", get(patient_note))
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

async fn patient_note(State(state): State<AppState>, authed: AuthedClinician, Path(patient_id): Path<Uuid>) -> Result<Json<serde_json::Value>, StatusCode> {
    let repo = state.repo_for(&authed); // the ONLY data-access pattern
    let events = repo.get_events(patient_id).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if events.is_empty() {
        return Err(StatusCode::NOT_FOUND); // Cross-tenant and nonexistent look identical
    }
    // replay wants `&[NoteEvent]`: collect the `.event` field of each StoredEvent into a
    // Vec that owns them, then pass a shared borrow (`&Vec<T>` coerces to `&[T]`).
    let note_events: Vec<_> = events.into_iter().map(|e| e.event).collect();
    let note = er_domain::replay::replay(patient_id, &note_events); // returns PatientNote, not Result
    Ok(Json(json!({ "phase": format!("{:?}", note.phase), "hpi": note.note_sections.hpi.clone() })))
}








/*  
/// State(state) is the same as 
/// async fn(wrapper: State<AppState>) {
///     let State(state) = wrapper     
/// }
/// or
/// let state = state.0  
*/
