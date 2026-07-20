use axum::{Json, Router, http::StatusCode, middleware, response::IntoResponse, routing::get};
use serde_json::json;

use crate::{auth::{self, AuthedClinician}, state::AppState};

/// Map from (path, method) -> handler
/// caputre state clone so Middleware's State(state) parameter can be injected as require_auth's State<AppState> first argument.
/// So Router<AppState> becomes Router<()> aka Router which we return from fn router()
/// `Router<()>` can be served by `axum::serve`, because "state `()` means nothing left to inject.
/// '.route_layer' applis only to requests that match a route already on this router. 
/// A request that matches no route here falls through to the merfed router's '/healthz' (or 404) without running 'require_auth'
/// '.layer' would wrap everything, including the fallback - which would run auth on unmatched paths and if placed globally, on '/healthz'
/// using '.route_layer' scopes the gate to '/v1' routes and leaves the health probe open (no token) for load balancer
pub fn router(state: AppState) -> Router {
    let v1 = Router::new()
        .route("/v1/me", get(me))
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


