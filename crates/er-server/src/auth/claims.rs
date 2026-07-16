use uuid::Uuid;




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
    let header = header.kid.ok_or(AuthError::MissingKid)?;

    // Pick the public key by kid (KeySet refreshes on miss in cognito mode)



    // signature + exp + iss in one call


    let data = jsonwebtoken::decode::<Claims>(token, &key, &validation)?;

    // Cognito-only token_use == "access", client_id is ours
    // dev tokens are minted with token_use=access



    Ok(data.claims)
}



pub struct Keyset {
    jwks: tokio::sync::RwLock<jsonwebtoken::jwk::JwkSet>,
    refresh_url: Option<String>, // Some(...) only in cognito mode
    http: Option<reqwest::Client>,
}

impl Keyset {
    
}