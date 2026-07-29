use std::assert_eq;

use tower::ServiceExt;

use er_server::{routes, state::AppState, auth::keys::KeySet};
use std::{sync::Arc, path::Path};
use sqlx::PgPool;

async fn test_state(pool: PgPool) -> AppState {
    // cwd for integration tests is the crate dir (crates/er-server), so the repo-root
    // dev keys are two levels up. `concat!(env!("CARGO_MANIFEST_DIR"), ...)` would be
    // cwd-independent if you prefer.
    let keys = Arc::new(
        KeySet::from_file(Path::new("../../dev/keys/jwks.json")).expect("dev jwks"),
    );
    AppState::for_tests(pool, keys)
}

use tower::ServiceExt;                    // brings `.oneshot`
use axum::{body::Body, http::{Request, header}};

async fn get(app: &axum::Router, path: &str, token: Option<&str>) -> u16 {
    let mut req = Request::builder().method("GET").uri(path);
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let response = app.clone().oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    response.status().as_u16()
}



#[sqlx::test]
async fn no_token_is_401_and_healthz_is_open(pool: PgPool) {
    let app = routes::router(test_state(pool).await);
    assert_eq!(get(&app, "/v1/me", None).await, 401);
    assert_eq!(get(&app, "/healthz", None).await, 200);
}



#[sqlx::test]




#[sqlx::test]



#[sqlx::test]
