use axum::{Json, http::StatusCode, response::{IntoResponse, Response}};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// Missing OR other-org (indistinguishable on purpose
    #[error("not found")]
    NotFound,

    /// The client sent something structurally wrong it should fix before retrying.
    /// &'static str is a quiet security device: a compile-time string literal
    /// CANNOT contain runtime data, so a validation message can never
    /// accidentally interpolate a chief complaint or a transcript into a
    /// response body. The type system is doing HIPAA work here.
    #[error("validation: {0}")]
    Validation(&'static str),

    /// Valid request, but current state forbids it (active shift exists, …).
    #[error("conflict: {0}")]
    Conflict(&'static str),

    /// Our fault. Details go to tracing; the caller learns nothing.
    #[error("internal: {0}")]
    Internal(#[from] anyhow::Error),
}

// Funnel the lower layers in so `?` just works in handlers.
impl From<er_store::StoreError> for ApiError {
    fn from(e: er_store::StoreError) -> Self { Self::Internal(e.into()) }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self { Self::Internal(e.into()) }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            ApiError::Validation(m) => (StatusCode::BAD_REQUEST, *m),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, *m),
            ApiError::Internal(e) => {
                // The ONLY place a 500's cause exists is the server log.
                tracing::error!(error = ?e, "internal error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal")
            }
        };
        // Machine-readable, human-terse, PHI-free. Clients branch on status
        // the code string is for humans reading a debugger, not for parsing.
        (status, Json(json!({ "error": code }))).into_response()

    }
}