use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
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
    fn from(e: er_store::StoreError) -> Self {
        Self::Internal(e.into())
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        Self::Internal(e.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            ApiError::Validation(m) => (StatusCode::BAD_REQUEST, *m),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, *m),
            ApiError::Internal(e) => {
                // A stable code plus the request span's id. SQL, serde and
                // provider error text can echo row values or request input.
                let error_kind = crate::telemetry::error_kind(e);
                if let Some(suppressed) = crate::telemetry::admit(error_kind) {
                    tracing::error!(error_kind, suppressed, "internal error");
                }
                (StatusCode::INTERNAL_SERVER_ERROR, "internal")
            }
        };
        // Machine-readable, human-terse, PHI-free. Clients branch on status
        // the code string is for humans reading a debugger, not for parsing.
        (status, Json(json!({ "error": code }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A database error carrying row values is logged as its code only.
    #[test]
    fn internal_errors_log_a_code_not_the_error_text() {
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::registry()
            .with(crate::telemetry::json_layer(move || writer.clone()));
        let response = tracing::subscriber::with_default(subscriber, || {
            let source = sqlx::Error::Protocol("row (CANARYROW, chest pain)".into());
            ApiError::from(er_store::StoreError::backend(source)).into_response()
        });
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let text = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(!text.contains("CANARYROW"), "{text}");
        let line: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(line["error_kind"], "db_protocol");
    }
}
