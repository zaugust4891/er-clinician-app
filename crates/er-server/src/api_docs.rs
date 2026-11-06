//! Authenticated documentation; snapshot generation never runs in the API process.
use axum::{
    http::{StatusCode, header},
    response::{Html, IntoResponse},
};
pub async fn spec() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        include_str!("../../../docs/api/openapi.json"),
    )
}
pub async fn ui() -> impl IntoResponse {
    Html(include_str!("../../../docs/api/swagger.html"))
}
