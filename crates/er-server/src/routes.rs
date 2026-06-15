use axum::{Router, routing::get, response::IntoResponse, Json, http::StatusCode};
use serde_json::json;

use crate::state::AppState;

pub fn router(state: AppState) -> Router {
    Router::new().route("/healthz", get(healthz)).with_state(state)
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "status": "ok" })))
}
