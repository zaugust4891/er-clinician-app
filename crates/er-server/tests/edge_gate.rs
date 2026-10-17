//! S8 edge contract: minimal liveness, bounded readiness that fails on database
//! or schema loss, `no-store` responses, and TLS on the ALB → API hop.
use std::sync::Arc;

use axum::{body::Body, http::Request};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

use er_server::{routes, tls};

mod common;
use common::test_state;

async fn probe(app: &axum::Router, path: &str) -> (u16, Option<String>, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let cache = response
        .headers()
        .get("cache-control")
        .map(|value| value.to_str().unwrap().to_owned());
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, cache, body)
}

#[sqlx::test]
async fn database_loss_fails_readiness_but_not_liveness(pool: PgPool) {
    let app = routes::router(test_state(pool.clone()).await);
    let no_store = Some("no-store".to_string());
    let ready = serde_json::json!({ "status": "ready" });
    let unavailable = serde_json::json!({ "status": "unavailable" });
    let live = serde_json::json!({ "status": "ok" });

    assert_eq!(probe(&app, "/readyz").await, (200, no_store.clone(), ready));
    assert_eq!(
        probe(&app, "/healthz").await,
        (200, no_store.clone(), live.clone())
    );

    pool.close().await;
    // The body is static: no pool, host or driver error reaches the caller.
    assert_eq!(
        probe(&app, "/readyz").await,
        (503, no_store.clone(), unavailable)
    );
    assert_eq!(probe(&app, "/healthz").await, (200, no_store, live));
}

#[sqlx::test]
async fn missing_schema_fails_readiness(pool: PgPool) {
    let app = routes::router(test_state(pool.clone()).await);
    assert_eq!(probe(&app, "/readyz").await.0, 200);
    sqlx::query("DROP TABLE jobs CASCADE")
        .execute(&pool)
        .await
        .unwrap();
    let (status, _, body) = probe(&app, "/readyz").await;
    assert_eq!(status, 503);
    assert_eq!(body, serde_json::json!({ "status": "unavailable" }));
}

#[sqlx::test]
async fn clinical_and_rejected_responses_are_not_cacheable(pool: PgPool) {
    let app = routes::router(test_state(pool).await);
    let (status, cache, _) = probe(&app, "/v1/me").await;
    assert_eq!((status, cache.as_deref()), (401, Some("no-store")));
    let (status, cache, _) = probe(&app, "/v1/unknown").await;
    assert_eq!((status, cache.as_deref()), (404, Some("no-store")));
}

/// Accepts exactly the task's certificate and still verifies the handshake
/// signature, proving the server holds the key the certificate names.
#[derive(Debug)]
struct Pinned {
    certificate: CertificateDer<'static>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() == self.certificate.as_ref() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("unexpected certificate".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let algorithms = &self.provider.signature_verification_algorithms;
        rustls::crypto::verify_tls12_signature(message, cert, dss, algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let algorithms = &self.provider.signature_verification_algorithms;
        rustls::crypto::verify_tls13_signature(message, cert, dss, algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        let algorithms = &self.provider.signature_verification_algorithms;
        algorithms.supported_schemes()
    }
}

const READYZ: &[u8] = b"GET /readyz HTTP/1.1\r\nHost: er-server\r\nConnection: close\r\n\r\n";

async fn tls_get(
    addr: std::net::SocketAddr,
    certificate: &CertificateDer<'static>,
    version: &'static rustls::SupportedProtocolVersion,
) -> (rustls::ProtocolVersion, String) {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(Pinned {
        certificate: certificate.clone(),
        provider: provider.clone(),
    });
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[version])
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let name = ServerName::try_from("er-server").unwrap();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let mut stream = connector.connect(name, tcp).await.unwrap();
    let negotiated = stream.get_ref().1.protocol_version().unwrap();
    stream.write_all(READYZ).await.unwrap();
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response).await;
    (negotiated, String::from_utf8_lossy(&response).into_owned())
}

#[sqlx::test]
async fn api_listener_serves_only_encrypted_http(pool: PgPool) {
    let identity = tls::ServerIdentity::generate().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let listener = tls::TlsListener::new(listener, &identity).unwrap();
    let app = routes::router(test_state(pool).await);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    // The ALB may negotiate either version; both carry the readiness response.
    for (version, expected) in [
        (&rustls::version::TLS13, rustls::ProtocolVersion::TLSv1_3),
        (&rustls::version::TLS12, rustls::ProtocolVersion::TLSv1_2),
    ] {
        let (negotiated, response) = tls_get(addr, &identity.certificate, version).await;
        assert_eq!(negotiated, expected);
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains(r#"{"status":"ready"}"#));
    }

    // Plain HTTP to the TLS port never reaches a handler.
    let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    tcp.write_all(READYZ).await.unwrap();
    let mut response = Vec::new();
    let _ = tcp.read_to_end(&mut response).await;
    assert!(!response.starts_with(b"HTTP/"));

    // A fresh task presents a different, independently generated certificate.
    let other = tls::ServerIdentity::generate().unwrap();
    assert_ne!(other.certificate, identity.certificate);
}

#[sqlx::test]
async fn saturated_pool_has_bounded_readiness(pool: PgPool) {
    let small = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(30))
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let app = routes::router(test_state(small.clone()).await);
    assert_eq!(probe(&app, "/readyz").await.0, 200);
    let held = small.acquire().await.unwrap();
    // Readiness must not inherit the pool's much longer checkout timeout.
    let (status, cache, body) =
        tokio::time::timeout(std::time::Duration::from_secs(4), probe(&app, "/readyz"))
            .await
            .expect("readiness bounded independently of pool checkout");
    assert_eq!(status, 503);
    assert_eq!(cache.as_deref(), Some("no-store"));
    assert_eq!(body, serde_json::json!({ "status": "unavailable" }));
    assert_eq!(probe(&app, "/healthz").await.0, 200);
    drop(held);
    assert_eq!(probe(&app, "/readyz").await.0, 200);
    small.close().await;
}

#[sqlx::test]
async fn sync_body_above_waf_window_is_accepted_but_two_mib_cap_remains(pool: PgPool) {
    use common::{ORG_A, mint, seed_org_and_clinician, seed_registered_patient};
    use er_domain::NoteEvent;
    use uuid::Uuid;

    seed_org_and_clinician(&pool, ORG_A, "synthetic-edge-user").await;
    let patient = seed_registered_patient(&pool, ORG_A).await;
    let app = routes::router(test_state(pool.clone()).await);
    let token = mint("synthetic-edge-user");
    for (size, expected) in [(32 * 1024, 200), (2 * 1024 * 1024, 413)] {
        let event = NoteEvent::transcript_ready(
            patient,
            Uuid::now_v7(),
            "x".repeat(size),
            chrono::Utc::now(),
        );
        let body = serde_json::to_vec(&serde_json::json!({
            "events": [{ "event_id": Uuid::now_v7(), "event": event }]
        }))
        .unwrap();
        assert!(body.len() > 8192);
        let response = app
            .clone()
            .oneshot(
                Request::post(format!("/v1/patients/{patient}/events"))
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), expected);
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    assert_eq!(common::event_row_count(&pool, patient).await, 1);
}
