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

#[cfg(test)]
mod tests {
    //! Pure-validator tests (deep dive §4, steps 1–5). Every token here is minted in-test with the
    //! real `dev/keys/dev_private.pem` and verified against `dev/keys/jwks.json` — no network, no DB.
    //! Paths are anchored to CARGO_MANIFEST_DIR so they resolve regardless of the test's cwd.
    use super::*;
    use crate::config::DEV_ISSUER;
    use std::path::{Path, PathBuf};

    const KID: &str = "dev-key-1"; // must match dev/keys/jwks.json

    fn dev_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dev/keys")
    }

    fn dev_keyset() -> KeySet {
        KeySet::from_file(&dev_dir().join("jwks.json"))
            .expect("dev/keys/jwks.json — generate with `mint_dev_token jwks > dev/keys/jwks.json`")
    }

    fn dev_cfg() -> AuthConfig {
        AuthConfig::Dev { jwks_path: dev_dir().join("jwks.json") }
    }

    /// Sign a claims blob with the dev private key. `kid: None` omits the header kid.
    fn sign(claims: serde_json::Value, kid: Option<&str>) -> String {
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        header.kid = kid.map(str::to_string);
        let pem = std::fs::read(dev_dir().join("dev_private.pem")).expect("dev_private.pem");
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(&pem).unwrap();
        jsonwebtoken::encode(&header, &claims, &key).unwrap()
    }

    /// A well-formed, currently-valid dev access token for `sub`.
    fn valid_token(sub: &str) -> String {
        sign(serde_json::json!({
            "sub": sub,
            "iss": DEV_ISSUER,
            "token_use": "access",
            "exp": jsonwebtoken::get_current_timestamp() + 3600,
        }), Some(KID))
    }

    // valid token → Ok(Claims) with the right sub (happy path)
    #[tokio::test]
    async fn valid_dev_token_is_accepted() {
        let claims = validate_token(&valid_token("test-doc"), &dev_keyset(), &dev_cfg())
            .await
            .expect("a freshly minted dev token must validate");
        assert_eq!(claims.sub, "test-doc");
    }

    // garbage string → Err(Jwt) (malformed input)
    #[tokio::test]
    async fn garbage_string_is_rejected() {
        let err = validate_token("not-a-jwt", &dev_keyset(), &dev_cfg()).await.unwrap_err();
        assert!(matches!(err, AuthError::Jwt(_)), "got {err:?}");
    }

    // signature that does not verify against the trusted key → Err(Jwt) (forgery/tamper — *the* check)
    #[tokio::test]
    async fn tampered_signature_is_rejected() {
        let token = valid_token("test-doc");
        // Corrupt one character of the signature segment; the payload is untouched and still
        // decodes, so this exercises the signature check specifically, not JSON parsing.
        let mut segs: Vec<String> = token.split('.').map(String::from).collect();
        let sig = &segs[2];
        let first = sig.chars().next().unwrap();
        let replacement = if first == 'A' { 'B' } else { 'A' };
        segs[2] = format!("{replacement}{}", &sig[first.len_utf8()..]);
        let forged = segs.join(".");

        let err = validate_token(&forged, &dev_keyset(), &dev_cfg()).await.unwrap_err();
        assert!(matches!(err, AuthError::Jwt(_)), "got {err:?}");
    }

    // exp in the past → Err(Jwt) (replay of a stale token)
    #[tokio::test]
    async fn expired_token_is_rejected() {
        let token = sign(serde_json::json!({
            "sub": "test-doc",
            "iss": DEV_ISSUER,
            "token_use": "access",
            "exp": jsonwebtoken::get_current_timestamp() - 3600, // well past the 60s default leeway
        }), Some(KID));
        let err = validate_token(&token, &dev_keyset(), &dev_cfg()).await.unwrap_err();
        assert!(matches!(err, AuthError::Jwt(_)), "got {err:?}");
    }

    // iss = someone else → Err(Jwt) (wrong-issuer token)
    #[tokio::test]
    async fn wrong_issuer_is_rejected() {
        let token = sign(serde_json::json!({
            "sub": "test-doc",
            "iss": "https://evil.example",
            "token_use": "access",
            "exp": jsonwebtoken::get_current_timestamp() + 3600,
        }), Some(KID));
        let err = validate_token(&token, &dev_keyset(), &dev_cfg()).await.unwrap_err();
        assert!(matches!(err, AuthError::Jwt(_)), "got {err:?}");
    }

    // header without kid → Err(MissingKid) (key-selection integrity)
    #[tokio::test]
    async fn token_without_kid_is_rejected() {
        let token = sign(serde_json::json!({
            "sub": "test-doc",
            "iss": DEV_ISSUER,
            "token_use": "access",
            "exp": jsonwebtoken::get_current_timestamp() + 3600,
        }), None);
        let err = validate_token(&token, &dev_keyset(), &dev_cfg()).await.unwrap_err();
        assert!(matches!(err, AuthError::MissingKid), "got {err:?}");
    }

    // cognito cfg + token_use:"id" → Err(WrongTokenUse) (ID-token-as-credential, the confused deputy)
    #[tokio::test]
    async fn cognito_rejects_id_token() {
        let cognito = AuthConfig::Cognito {
            region: "us-east-1".into(),
            pool_id: "pool-123".into(),
            client_id: "client-123".into(),
        };
        // iss must match the cognito config's issuer so we get *past* the iss check and reach the
        // token_use check that this test is about. Signed with the dev key (kid dev-key-1) so the
        // dev KeySet can still find the verification key.
        let token = sign(serde_json::json!({
            "sub": "test-doc",
            "iss": cognito.issuer(),
            "token_use": "id",
            "client_id": "client-123",
            "exp": jsonwebtoken::get_current_timestamp() + 3600,
        }), Some(KID));
        let err = validate_token(&token, &dev_keyset(), &cognito).await.unwrap_err();
        assert!(matches!(err, AuthError::WrongTokenUse), "got {err:?}");
    }
}

