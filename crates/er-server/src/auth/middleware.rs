use axum::extract::{FromRequestParts, State};
use axum::http::header::HeaderMap;
use axum::response::IntoResponse;

use reqwest::StatusCode;

use crate::auth::{AuthError, AuthedClinician, claims};
use crate::state::AppState;

/*
Success — `authed = AuthedClinician { clinician_id: …0002, org_id: …0001 }`.

- `req.extensions_mut() returns `&mut Extensions`. `Extensions` is a type-indexed map — a
  map whose *key is a Rust type*, at most one value per type. `.insert(authed)` stores the
  `AuthedClinician` under the key "the `AuthedClinician` type." This is the side-channel: axum can't
  add a typed argument to the downstream handler directly, so middleware stashes a value here and
  the handler's extractor fishes it back out by type (Part 2).
- `next.run(req).await` hands the *mutated* request (now carrying identity in extensions) to the
  rest of the pipeline, runs `me`, and returns its `Response` unchanged.

Failure (say, expired token → `Err(AuthError::Jwt(..))`):

- `tracing::warn!(error = %err, …)` logs the reason server-side; `%err` uses the value's
  `Display` (your `thiserror` `#[error("…")]` strings). This is the *only* place the reason exists.
- `StatusCode::UNAUTHORIZED.into_response()` — `.into_response()` (from axum's `IntoResponse`)
  builds a `Response` with status 401 and an *empty body*. The caller learns nothing; the logs know
  everything. Crucially, **`next` is never called** — the handler cannot run.

*/
pub async fn require_auth(
    State(state): State<AppState>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    match authenticate(&state, req.headers()).await {
        Ok(authed) => {
            req.extensions_mut().insert(authed);
            next.run(req).await
        }
        Err(err) => {
            tracing::warn!(error = %err, "auth rejected");
            axum::http::StatusCode::UNAUTHORIZED.into_response()
        }
    }
}

//Tracing our `Bearer eyJ0…` header
// **`.and_then(|v| v.strip_prefix("Bearer "))`** → `Option<&str>`. Checks the scheme *and* strips
// it in one move. `Basic abc123` → `None`. → `Some("eyJ0…")`.

/*

The return value (no `?`, no `;` — it *is* the result). Take `claims.sub = "test-doc"` — the one
field we trust as identity — and resolve to a tenant:

- cache miss → `SELECT id, org_id FROM clinicians WHERE cognito_sub = 'test-doc'` →
  `Ok(AuthedClinician { …0002, …0001 })`, also cached.
- valid token but *no clinicians row* → `Err(UnknownClinician)` → 401 (the "my token works but I
  still get 401" case — a valid signature is not membership).

The key insight: `org_id` comes from the *database row*, never from the token. The token proves only
"caller is `sub = test-doc`"; the `clinicians` table decides what org that means.

*/

async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<AuthedClinician, AuthError> {
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(AuthError::MissingToken)?;
    //Hands the bare token to the pure validator from S3 → `Ok(Claims { sub: "test-doc", … })`. `?`
    //propagates any `Jwt` / `MissingKid` / … straight to the middleware's `Err` arm → 401. (Real field: `state.config.auth_mode`.)
    let claims = claims::validate_token(token, &state.keys, &state.config.auth_mode).await?;
    state.resolver.resolve(&claims.sub).await
}

impl<S: Send + Sync> FromRequestParts<S> for AuthedClinician {
    type Rejection = StatusCode;
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _: &S,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthedClinician>()
            .cloned()
            .ok_or(StatusCode::UNAUTHORIZED)
    }
}

/*
GET /v1/me HTTP/1.1
host: localhost:8080
authorization: Bearer eryeri8iIJCdpfmsd;vmsv;ksvs

AppState (Build once at boot, cloned per request):
-state.keys -> a KeySet trusting dev-key-1
-state.resolver -> a ClinicianResolver whose clinicians table has one row:
    cognito_sub = "test-doc"-> id = ...0002, org_id = ...0001
-state.config.auth_mode -> AuthConfig::Dev { .. }


The decoded JQT payload we carry around:
'''json
{
    "sub": "test-doc",
    "iss": "https://dev.local",
    "token_use": "access",
    "exp": 1784324441
}
'''



*/
