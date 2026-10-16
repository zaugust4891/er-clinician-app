//! Signing keys for access-token validation.
//!
//! Cognito mode fetches the pool's JWKS once at startup and again only when a
//! token names a `kid` the cached set lacks. That refresh is the one upstream call
//! an unauthenticated request can cause, so it is bounded on every axis (S7):
//! HTTPS only, no redirects, a whole-request timeout, a response-byte cap, one
//! fetch in flight at a time, and at most one fetch per cooldown window. A failed
//! fetch keeps the last-known-good keys and never admits the unknown key.
use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use jsonwebtoken::jwk::JwkSet;
use reqwest::{Client, redirect};
use tokio::sync::{Mutex, RwLock};

use crate::auth::AuthError;

/// Limits on one JWKS fetch and on how often a key miss may cause one.
#[derive(Clone, Copy, Debug)]
pub struct RefreshPolicy {
    /// Connect and whole-request (headers + body) budget.
    pub timeout: Duration,
    /// A Cognito JWKS is two RSA keys, well under 4 KiB.
    pub max_bytes: usize,
    /// Minimum spacing between fetch attempts, successful or not. Every unknown
    /// `kid` inside this window is rejected from the cache without upstream work.
    pub cooldown: Duration,
}

impl Default for RefreshPolicy {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            max_bytes: 64 * 1024,
            cooldown: Duration::from_secs(30),
        }
    }
}

pub struct KeySet {
    jwks: RwLock<JwkSet>,
    remote: Option<Remote>, // Some(...) only in cognito mode
}

struct Remote {
    url: String,
    http: Client,
    policy: RefreshPolicy,
    /// Held for the whole refresh, so concurrent misses queue behind one fetch
    /// and then see its result. Records when the last attempt started.
    last_attempt: Mutex<Option<Instant>>,
}

impl KeySet {
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        Ok(Self::from_jwk_set(serde_json::from_str(&contents)?))
    }

    /// A fixed, never-refreshed key set (dev keys, tests).
    pub fn from_jwk_set(jwks: JwkSet) -> Self {
        Self {
            jwks: RwLock::new(jwks),
            remote: None,
        }
    }

    /// Cognito mode. The startup fetch must succeed: the API does not serve
    /// without a verified key set.
    pub async fn from_url(url: String) -> anyhow::Result<Self> {
        Self::remote(url, RefreshPolicy::default(), true).await
    }

    async fn remote(url: String, policy: RefreshPolicy, https_only: bool) -> anyhow::Result<Self> {
        if https_only && !url.starts_with("https://") {
            anyhow::bail!("JWKS URL must use https");
        }
        let http = Client::builder()
            .https_only(https_only)
            .redirect(redirect::Policy::none())
            .connect_timeout(policy.timeout)
            .timeout(policy.timeout)
            .build()?;
        let remote = Remote {
            url,
            http,
            policy,
            last_attempt: Mutex::new(None),
        };
        let jwks = remote.fetch().await?;
        Ok(Self {
            jwks: RwLock::new(jwks),
            remote: Some(remote),
        })
    }

    async fn cached(&self, kid: &str) -> Result<Option<jsonwebtoken::DecodingKey>, AuthError> {
        match self.jwks.read().await.find(kid) {
            Some(jwk) => Ok(Some(jsonwebtoken::DecodingKey::from_jwk(jwk)?)),
            None => Ok(None),
        }
    }

    pub async fn key_for(&self, kid: &str) -> Result<jsonwebtoken::DecodingKey, AuthError> {
        if let Some(key) = self.cached(kid).await? {
            return Ok(key);
        }
        let Some(remote) = &self.remote else {
            return Err(AuthError::UnknownKid(kid.to_string()));
        };

        let mut last_attempt = remote.last_attempt.lock().await;
        // A refresh that finished while this request waited may have added the key.
        if let Some(key) = self.cached(kid).await? {
            return Ok(key);
        }
        if (*last_attempt).is_some_and(|at| at.elapsed() < remote.policy.cooldown) {
            return Err(AuthError::UnknownKid(kid.to_string()));
        }
        *last_attempt = Some(Instant::now());
        // On failure the previous keys stay in place for every other request.
        let jwks = remote.fetch().await?;
        *self.jwks.write().await = jwks;
        drop(last_attempt);

        self.cached(kid)
            .await?
            .ok_or_else(|| AuthError::UnknownKid(kid.to_string()))
    }
}

impl Remote {
    async fn fetch(&self) -> Result<JwkSet, AuthError> {
        let mut response = self.http.get(self.url.as_str()).send().await?;
        // Redirects are not followed, so a 3xx arrives here and must fail too.
        if !response.status().is_success() {
            return Err(AuthError::JwksStatus(response.status().as_u16()));
        }
        let max = self.policy.max_bytes;
        if response
            .content_length()
            .is_some_and(|len| len > max as u64)
        {
            return Err(AuthError::JwksTooLarge(max));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if body.len() + chunk.len() > max {
                return Err(AuthError::JwksTooLarge(max));
            }
            body.extend_from_slice(&chunk);
        }
        let jwks: JwkSet = serde_json::from_slice(&body).map_err(AuthError::JwksInvalid)?;
        if jwks.keys.is_empty() {
            return Err(AuthError::JwksEmpty);
        }
        // Every key must be an RS256 signing key before the set may replace the
        // cached one; otherwise a bad refresh would discard last-known-good keys.
        // A key without a `kid` could never be selected, so it counts as unusable.
        let mut kids = BTreeSet::new();
        for jwk in &jwks.keys {
            // Selection is by kid, so two keys with the same label make a set
            // ambiguous. Never replace good cached keys with such a response.
            let uniquely_named = jwk
                .common
                .key_id
                .as_deref()
                .is_some_and(|kid| !kid.is_empty() && kids.insert(kid));
            if !uniquely_named || !rs256_signing_key(jwk) {
                return Err(AuthError::JwksUnusableKey);
            }
        }
        Ok(jwks)
    }
}

/// Cognito publishes 2048-bit keys; anything shorter is not a key we accept.
const MIN_MODULUS_BITS: usize = 2048;

/// Cognito signs access tokens with RS256 RSA keys only.
fn rs256_signing_key(jwk: &jsonwebtoken::jwk::Jwk) -> bool {
    use jsonwebtoken::jwk::{AlgorithmParameters, KeyAlgorithm, PublicKeyUse};
    matches!(&jwk.algorithm, AlgorithmParameters::RSA(p) if rsa_public_key(&p.n, &p.e))
        && jwk
            .common
            .key_algorithm
            .is_none_or(|alg| alg == KeyAlgorithm::RS256)
        && jwk
            .common
            .public_key_use
            .as_ref()
            .is_none_or(|usage| *usage == PublicKeyUse::Signature)
        && jsonwebtoken::DecodingKey::from_jwk(jwk).is_ok()
}

/// `DecodingKey::from_jwk` only base64-decodes the components, so a zero or
/// even exponent or a tiny modulus would pass it and then fail every
/// signature. Require parameters that can form a real RSA public key.
fn rsa_public_key(n: &str, e: &str) -> bool {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use rsa::BigUint;
    let (Ok(n), Ok(e)) = (URL_SAFE_NO_PAD.decode(n), URL_SAFE_NO_PAD.decode(e)) else {
        return false;
    };
    // Big-endian: the last byte holds the low bit. Both values must be odd.
    let odd = |bytes: &[u8]| bytes.last().is_some_and(|b| b & 1 == 1);
    if !odd(&n) || !odd(&e) {
        return false;
    }
    let (n, e) = (BigUint::from_bytes_be(&n), BigUint::from_bytes_be(&e));
    // `new` bounds the modulus size and the exponent range (2..2^33).
    n.bits() >= MIN_MODULUS_BITS && rsa::RsaPublicKey::new(n, e).is_ok()
}

#[cfg(test)]
mod tests {
    //! A loopback JWKS server stands in for Cognito. These tests use the private
    //! plain-HTTP constructor; `from_url` itself refuses anything but https.
    use super::*;
    use crate::auth::claims::validate_token;
    use crate::config::{AuthConfig, DEV_ISSUER};
    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    enum Serve {
        Jwks(serde_json::Value),
        Status(u16),
        Redirect(String),
        Raw(String),
    }

    struct Upstream {
        serve: StdMutex<Serve>,
        hits: AtomicUsize,
        delay: Duration,
    }

    impl Upstream {
        fn set(&self, serve: Serve) {
            *self.serve.lock().unwrap() = serve;
        }
        fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }
    }

    async fn upstream(serve: Serve, delay: Duration) -> (Arc<Upstream>, String) {
        let state = Arc::new(Upstream {
            serve: StdMutex::new(serve),
            hits: AtomicUsize::new(0),
            delay,
        });
        let handler_state = state.clone();
        let app = axum::Router::new().route(
            "/jwks.json",
            axum::routing::get(move || {
                let state = handler_state.clone();
                async move {
                    state.hits.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(state.delay).await;
                    let serve = state.serve.lock().unwrap().clone();
                    match serve {
                        Serve::Jwks(body) => axum::Json(body).into_response(),
                        Serve::Status(code) => StatusCode::from_u16(code).unwrap().into_response(),
                        Serve::Redirect(to) => {
                            (StatusCode::FOUND, [(header::LOCATION, to)]).into_response()
                        }
                        Serve::Raw(body) => body.into_response(),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (state, format!("http://{addr}/jwks.json"))
    }

    fn dev_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../dev/keys")
    }

    /// The dev public key published under `kid`.
    fn jwks_with(kid: &str) -> serde_json::Value {
        let raw = std::fs::read_to_string(dev_dir().join("jwks.json")).unwrap();
        let mut jwks: serde_json::Value = serde_json::from_str(&raw).unwrap();
        jwks["keys"][0]["kid"] = kid.into();
        jwks
    }

    /// The dev key's signature under `kid`.
    fn token_with_kid(kid: &str) -> String {
        let pem = std::fs::read(dev_dir().join("dev_private.pem")).unwrap();
        signed(kid, &jsonwebtoken::EncodingKey::from_rsa_pem(&pem).unwrap())
    }

    /// An independent pair, so rotation changes the key material, not just its label.
    fn fresh_pair(kid: &str) -> (serde_json::Value, jsonwebtoken::EncodingKey) {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        use rsa::{pkcs1::EncodeRsaPrivateKey, traits::PublicKeyParts};
        let private = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
        let jwks = serde_json::json!({ "keys": [{
            "kty": "RSA", "alg": "RS256", "use": "sig", "kid": kid,
            "n": URL_SAFE_NO_PAD.encode(private.n().to_bytes_be()),
            "e": URL_SAFE_NO_PAD.encode(private.e().to_bytes_be()),
        }] });
        let der = private.to_pkcs1_der().unwrap();
        let key = jsonwebtoken::EncodingKey::from_rsa_der(der.as_bytes());
        (jwks, key)
    }

    fn signed(kid: &str, key: &jsonwebtoken::EncodingKey) -> String {
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        header.kid = Some(kid.to_string());
        let claims = serde_json::json!({
            "sub": "synthetic-doc",
            "iss": DEV_ISSUER,
            "token_use": "access",
            "exp": jsonwebtoken::get_current_timestamp() + 600,
        });
        jsonwebtoken::encode(&header, &claims, key).unwrap()
    }

    fn dev_cfg() -> AuthConfig {
        AuthConfig::Dev {
            jwks_path: dev_dir().join("jwks.json"),
        }
    }

    fn policy(cooldown: Duration) -> RefreshPolicy {
        RefreshPolicy {
            timeout: Duration::from_millis(500),
            max_bytes: 16 * 1024,
            cooldown,
        }
    }

    async fn loopback(url: &str, policy: RefreshPolicy) -> anyhow::Result<KeySet> {
        KeySet::remote(url.to_string(), policy, false).await
    }

    #[tokio::test]
    async fn production_constructor_requires_https() {
        let err = KeySet::from_url("http://127.0.0.1:9/jwks.json".into())
            .await
            .err()
            .expect("plain http must be refused before any request");
        assert!(err.to_string().contains("https"), "got {err}");
    }

    #[tokio::test]
    async fn rotation_admits_the_new_key_and_drops_the_retired_one() {
        let (up, url) = upstream(Serve::Jwks(jwks_with("old")), Duration::ZERO).await;
        let keys = loopback(&url, policy(Duration::ZERO)).await.unwrap();
        validate_token(&token_with_kid("old"), &keys, &dev_cfg())
            .await
            .expect("token signed under the startup key");

        let (new_jwks, new_key) = fresh_pair("new");
        up.set(Serve::Jwks(new_jwks));
        validate_token(&signed("new", &new_key), &keys, &dev_cfg())
            .await
            .expect("a rotated key is fetched on first use");

        // The retired key's signature relabeled with the new kid must not verify.
        let err = validate_token(&token_with_kid("new"), &keys, &dev_cfg())
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::Jwt(_)), "got {err:?}");

        let err = validate_token(&token_with_kid("old"), &keys, &dev_cfg())
            .await
            .unwrap_err();
        assert!(matches!(err, AuthError::UnknownKid(_)), "got {err:?}");
        assert_eq!(up.hits(), 3, "startup + one refresh per miss");
    }

    #[tokio::test]
    async fn unusable_refresh_keeps_last_known_good_keys() {
        let (up, url) = upstream(Serve::Jwks(jwks_with("good")), Duration::ZERO).await;
        let keys = loopback(&url, policy(Duration::ZERO)).await.unwrap();

        let mut broken = jwks_with("rotated");
        broken["keys"][0]["n"] = "!!not-base64!!".into();
        up.set(Serve::Jwks(broken));
        let err = keys.key_for("rotated").await.err().unwrap();
        assert!(matches!(err, AuthError::JwksUnusableKey), "got {err:?}");
        validate_token(&token_with_kid("good"), &keys, &dev_cfg())
            .await
            .expect("a malformed refresh never replaces good keys");

        // Parseable and decodable, but not an RS256 signing key.
        for incompatible in [
            serde_json::json!({ "kty": "oct", "kid": "rotated", "k": "c3ludGhldGlj" }),
            {
                let mut enc = jwks_with("rotated")["keys"][0].clone();
                enc["use"] = "enc".into();
                enc
            },
            {
                let mut rs512 = jwks_with("rotated")["keys"][0].clone();
                rs512["alg"] = "RS512".into();
                rs512
            },
        ] {
            up.set(Serve::Jwks(serde_json::json!({ "keys": [incompatible] })));
            let err = keys.key_for("rotated").await.err().unwrap();
            assert!(matches!(err, AuthError::JwksUnusableKey), "got {err:?}");
            validate_token(&token_with_kid("good"), &keys, &dev_cfg())
                .await
                .expect("an incompatible refresh never replaces RSA keys");
        }
    }

    #[tokio::test]
    async fn duplicate_kids_never_replace_good_keys() {
        let (up, url) = upstream(Serve::Jwks(jwks_with("good")), Duration::ZERO).await;
        let keys = loopback(&url, policy(Duration::from_secs(30)))
            .await
            .unwrap();
        let mut ambiguous = jwks_with("rotated");
        let duplicate = ambiguous["keys"][0].clone();
        ambiguous["keys"].as_array_mut().unwrap().push(duplicate);
        up.set(Serve::Jwks(ambiguous));

        assert!(matches!(
            keys.key_for("rotated").await,
            Err(AuthError::JwksUnusableKey)
        ));
        validate_token(&token_with_kid("good"), &keys, &dev_cfg())
            .await
            .expect("ambiguous refresh preserves the previous signing keys");
        assert!(matches!(
            keys.key_for("rotated").await,
            Err(AuthError::UnknownKid(_))
        ));
        assert_eq!(up.hits(), 2, "failed refresh also consumes the cooldown");
        assert!(loopback(&url, policy(Duration::ZERO)).await.is_err());
    }

    #[tokio::test]
    async fn invalid_rsa_parameters_never_replace_good_keys() {
        let (up, url) = upstream(Serve::Jwks(jwks_with("good")), Duration::ZERO).await;
        let good_n = jwks_with("good")["keys"][0]["n"]
            .as_str()
            .unwrap()
            .to_owned();
        // The final character carries the modulus' low bits; "A" clears them.
        let even_n = format!("{}A", &good_n[..good_n.len() - 1]);
        for (field, value, what) in [
            ("e", "AA".to_owned(), "zero exponent"),
            ("e", "AAAA".to_owned(), "zero exponent, padded"),
            ("e", "AQ".to_owned(), "exponent 1"),
            ("e", "AAE".to_owned(), "exponent 1 with a leading zero"),
            ("e", "AQAA".to_owned(), "even exponent"),
            ("n", "AQAB".to_owned(), "17-bit modulus"),
            ("n", even_n, "even modulus"),
        ] {
            let mut bad = jwks_with("rotated");
            bad["keys"][0][field] = value.into();
            up.set(Serve::Jwks(bad));
            let keys = loopback(&url, policy(Duration::ZERO)).await;
            assert!(keys.is_err(), "startup refuses a {what}");
        }

        up.set(Serve::Jwks(jwks_with("good")));
        let keys = loopback(&url, policy(Duration::from_secs(60)))
            .await
            .unwrap();
        let mut bad = jwks_with("rotated");
        bad["keys"][0]["e"] = "AA".into();
        up.set(Serve::Jwks(bad));
        let err = keys.key_for("rotated").await.err().unwrap();
        assert!(matches!(err, AuthError::JwksUnusableKey), "got {err:?}");
        let err = keys.key_for("rotated").await.err().unwrap();
        assert!(
            matches!(err, AuthError::UnknownKid(_)),
            "cooldown: got {err:?}"
        );
        validate_token(&token_with_kid("good"), &keys, &dev_cfg())
            .await
            .expect("cached keys still serve throughout the cooldown");
    }

    #[tokio::test]
    async fn kidless_refresh_keeps_last_known_good_keys() {
        let (up, url) = upstream(Serve::Jwks(jwks_with("good")), Duration::ZERO).await;
        // A long cooldown: a bad replacement would break valid tokens until it ends.
        let keys = loopback(&url, policy(Duration::from_secs(60)))
            .await
            .unwrap();

        let mut missing = jwks_with("rotated");
        missing["keys"][0].as_object_mut().unwrap().remove("kid");
        up.set(Serve::Jwks(missing));
        let err = keys.key_for("rotated").await.err().unwrap();
        assert!(matches!(err, AuthError::JwksUnusableKey), "got {err:?}");
        validate_token(&token_with_kid("good"), &keys, &dev_cfg())
            .await
            .expect("a kid-less refresh never replaces good keys");

        let kidless = loopback(&url, policy(Duration::ZERO)).await;
        assert!(kidless.is_err(), "startup also refuses a kid-less key set");

        let mut empty = jwks_with("rotated");
        empty["keys"][0]["kid"] = "".into();
        up.set(Serve::Jwks(empty));
        let err = keys.key_for("rotated").await.err().unwrap();
        assert!(
            matches!(err, AuthError::UnknownKid(_)),
            "cooldown: got {err:?}"
        );
        let fresh = loopback(&url, policy(Duration::ZERO)).await;
        assert!(fresh.is_err(), "an empty kid is refused like a missing one");
        validate_token(&token_with_kid("good"), &keys, &dev_cfg())
            .await
            .expect("cached keys still serve throughout the cooldown");
    }

    #[tokio::test]
    async fn unavailable_endpoint_keeps_last_known_good_keys() {
        let (up, url) = upstream(Serve::Jwks(jwks_with("good")), Duration::ZERO).await;
        let keys = loopback(&url, policy(Duration::ZERO)).await.unwrap();

        up.set(Serve::Status(503));
        let err = keys.key_for("unknown").await.err().unwrap();
        assert!(matches!(err, AuthError::JwksStatus(503)), "got {err:?}");
        keys.key_for("good")
            .await
            .expect("cached key survives a failed refresh");

        up.set(Serve::Jwks(serde_json::json!({ "keys": [] })));
        let err = keys.key_for("unknown").await.err().unwrap();
        assert!(matches!(err, AuthError::JwksEmpty), "got {err:?}");
        keys.key_for("good")
            .await
            .expect("an empty key set never replaces good keys");
    }

    #[tokio::test]
    async fn concurrent_random_kids_cause_one_upstream_fetch() {
        let (up, url) = upstream(Serve::Jwks(jwks_with("good")), Duration::from_millis(50)).await;
        let keys = Arc::new(
            loopback(&url, policy(Duration::from_secs(60)))
                .await
                .unwrap(),
        );

        let tasks: Vec<_> = (0..200)
            .map(|_| {
                let keys = keys.clone();
                tokio::spawn(async move { keys.key_for(&uuid::Uuid::new_v4().to_string()).await })
            })
            .collect();
        for task in tasks {
            assert!(
                task.await.unwrap().is_err(),
                "a random kid is never admitted"
            );
        }

        assert_eq!(up.hits(), 2, "startup + one coalesced refresh");
        keys.key_for("good").await.expect("known key still served");
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let (target, target_url) = upstream(Serve::Jwks(jwks_with("good")), Duration::ZERO).await;
        let (_, url) = upstream(Serve::Redirect(target_url), Duration::ZERO).await;
        let err = loopback(&url, policy(Duration::ZERO)).await.err().unwrap();
        assert!(err.to_string().contains("302"), "got {err}");
        assert_eq!(target.hits(), 0);
    }

    #[tokio::test]
    async fn oversized_and_malformed_bodies_are_rejected() {
        let (up, url) = upstream(Serve::Raw("x".repeat(20 * 1024)), Duration::ZERO).await;
        let err = loopback(&url, policy(Duration::ZERO)).await.err().unwrap();
        assert!(err.to_string().contains("exceeds"), "got {err}");

        up.set(Serve::Raw("not json".into()));
        assert!(loopback(&url, policy(Duration::ZERO)).await.is_err());
    }

    #[tokio::test]
    async fn slow_endpoint_is_bounded_by_the_timeout() {
        let (_, url) = upstream(Serve::Jwks(jwks_with("good")), Duration::from_secs(5)).await;
        let started = Instant::now();
        assert!(loopback(&url, policy(Duration::ZERO)).await.is_err());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "fetch must time out"
        );
    }
}
