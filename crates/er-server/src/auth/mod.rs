pub mod claims;
pub mod keys;
pub mod resolver;
pub mod middleware;
pub mod extractor;

use uuid::Uuid;
/// What the middleware inserts and handlers extract.
/// Clone is required (extensions + cache).
#[derive(Clone, Debug)]
pub struct AuthedClinician { 
    pub clinician_id: Uuid,
    pub org_id: Uuid 
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("missing or malformed Authorization header")] MissingToken,
    #[error("jwt validation failed: {0}")] Jwt(#[from] jsonwebtoken::errors::Error),
    #[error("token header has no kid")] MissingKid,
    #[error("no key with kid {0} in key set")] UnknownKid(String),
    #[error("token_use is not 'access'")] WrongTokenUse,
    #[error("client_id mismatch")] WrongClientId,
    #[error("no clinician for sub {0}")] UnknownClinician(String),
    #[error("db error during auth: {0}")] Db(#[from] sqlx::Error),
}