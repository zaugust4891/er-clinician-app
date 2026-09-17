use reqwest::Client;
use std::path::Path;
use tokio::sync::RwLock;

use crate::auth::AuthError;

pub struct KeySet {
    jwks: tokio::sync::RwLock<jsonwebtoken::jwk::JwkSet>,
    refresh_url: Option<String>, // Some(...) only in cognito mode
    http: Option<reqwest::Client>,
}

impl KeySet {
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let deserialized_keyset = serde_json::from_str::<jsonwebtoken::jwk::JwkSet>(&contents)?;
        let keyset = tokio::sync::RwLock::new(deserialized_keyset);

        Ok(Self {
            jwks: keyset,
            refresh_url: None,
            http: None,
        })
    }

    pub async fn from_url(url: String) -> anyhow::Result<Self> {
        let http = Client::new();
        let jwks: jsonwebtoken::jwk::JwkSet = http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(Self {
            jwks: RwLock::new(jwks),
            refresh_url: Some(url),
            http: Some(http),
        })
    }

    async fn refresh(&self) -> Result<(), AuthError> {
        let url = self.refresh_url.as_ref().ok_or(AuthError::NotRefreshable)?;
        let client = self.http.as_ref().ok_or(AuthError::NotRefreshable)?;
        let jwks: jsonwebtoken::jwk::JwkSet = client
            .get(url.as_str())
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        *self.jwks.write().await = jwks;
        Ok(())
    }

    pub async fn key_for(&self, kid: &str) -> Result<jsonwebtoken::DecodingKey, AuthError> {
        if let Some(jwk) = self.jwks.read().await.find(kid) {
            return Ok(jsonwebtoken::DecodingKey::from_jwk(jwk)?);
        }

        if self.refresh_url.is_some() {
            self.refresh().await?;
            if let Some(jwk) = self.jwks.read().await.find(kid) {
                return Ok(jsonwebtoken::DecodingKey::from_jwk(jwk)?);
            }
        }
        Err(AuthError::UnknownKid(kid.to_string()))
    }
}
