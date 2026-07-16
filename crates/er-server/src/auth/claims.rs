use uuid::Uuid;

use crate::auth::AuthError;
use crate::auth::keys::KeySet;
use crate::config::AuthConfig;




#[derive(Debug, serde::Deserialize)]
pub struct Claims {
    pub sub: String,
    #[serde(rename = "custom:org_id")]
    pub org_id: Option<Uuid>, // Convenience, cross-check ONLY. Never authoritative. Clinicians row is the authority. Access tokens may not carry it.
    pub token_use: Option<String>,
    pub exp: usize,
    pub iss: String,
    pub client_id: Option<String>,
}


pub async fn validate_token(token: &str, keys: &KeySet, auth: &AuthConfig) -> Result<Claims, AuthError> {
    // Decode only the header - this does not trust the payload
    let header = jsonwebtoken::decode_header(token)?;
    let kid = header.kid.ok_or(AuthError::MissingKid)?;

    // Pick the public key by kid (KeySet refreshes on miss in cognito mode)
    let key = keys.key_for(&kid).await?;


    // signature + exp + iss in one call
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.set_issuer(&[auth.issuer()]);
    validation.set_required_spec_claims(&["exp", "iss"]);

    let data = jsonwebtoken::decode::<Claims>(token, &key, &validation)?;

    // Cognito-only token_use == "access", client_id is ours
    // dev tokens are minted with token_use=access

    if let AuthConfig::Cognito { client_id, .. } = auth {
        if data.claims.token_use.as_deref() != Some("access") { return Err(AuthError::WrongTokenUse); }
        if data.claims.client_id.as_deref() != Some(client_id) { return Err(AuthError::WrongClientId); }
    }   

    Ok(data.claims)
}

