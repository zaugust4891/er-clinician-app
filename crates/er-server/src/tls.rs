//! S8: encryption on the ALB → API hop with a per-task, in-memory certificate.
//!
//! The ALB does not validate target certificates, so this certificate provides
//! confidentiality on the backend hop only. Which peers may reach the listener is
//! decided by target registration, the task security group and IAM — this is
//! **not** mutual TLS and the certificate authenticates nothing. No private key is
//! distributed or written to disk; each task generates its own at startup.
use std::{net::SocketAddr, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use rsa::{Pkcs1v15Sign, RsaPrivateKey, pkcs1::EncodeRsaPrivateKey, pkcs8::EncodePublicKey};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs1KeyDer};
use sha2::{Digest, Sha256};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Semaphore, mpsc},
    task::JoinHandle,
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

/// A peer that connects and never completes a handshake is dropped after this.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound on handshakes in flight; excess connections are closed, not queued.
const MAX_PENDING_HANDSHAKES: usize = 256;
/// Common name only; the ALB ignores it and no client is expected to pin it.
const SUBJECT: &[u8] = b"er-server";

// DER fragments: sha256WithRSAEncryption with NULL parameters, the commonName
// attribute type, the explicit v3 version tag, and SHA-256's DigestInfo prefix.
const SHA256_WITH_RSA: &[u8] = &[
    0x30, 0x0d, 0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b, 0x05, 0x00,
];
const COMMON_NAME: &[u8] = &[0x06, 0x03, 0x55, 0x04, 0x03];
const VERSION_3: &[u8] = &[0xa0, 0x03, 0x02, 0x01, 0x02];
const SHA256_DIGEST_INFO: &[u8] = &[
    0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05,
    0x00, 0x04, 0x20,
];

/// The task's certificate and the server configuration that presents it.
pub struct ServerIdentity {
    pub certificate: CertificateDer<'static>,
    config: Arc<rustls::ServerConfig>,
}

impl ServerIdentity {
    /// Generate a fresh 2048-bit RSA key and self-signed certificate. RSA keeps
    /// the ALB's backend negotiation on its widely supported ECDHE-RSA suites.
    pub fn generate() -> anyhow::Result<Self> {
        let key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048)
            .map_err(|_| anyhow::anyhow!("tls_key_generation_failed"))?;
        let certificate = self_signed_certificate(&key, Utc::now())?;
        let key_der = key
            .to_pkcs1_der()
            .map_err(|_| anyhow::anyhow!("tls_key_encoding_failed"))?;
        let private_key =
            PrivateKeyDer::Pkcs1(PrivatePkcs1KeyDer::from(key_der.as_bytes().to_vec()));
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
            .map_err(|_| anyhow::anyhow!("tls_configuration_failed"))?
            .with_no_client_auth()
            .with_single_cert(vec![certificate.clone()], private_key)
            .map_err(|_| anyhow::anyhow!("tls_configuration_failed"))?;
        Ok(Self {
            certificate,
            config: Arc::new(config),
        })
    }
}

/// A TLS listener for `axum::serve`. Handshakes run in their own tasks, so one
/// slow or hostile peer cannot stall accepting the next connection; only
/// completed handshakes reach the HTTP server.
pub struct TlsListener {
    local_addr: SocketAddr,
    ready: mpsc::Receiver<(TlsStream<TcpStream>, SocketAddr)>,
    /// Held only so dropping the listener stops accepting.
    _acceptor: AbortOnDrop,
}

struct AbortOnDrop(JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl TlsListener {
    pub fn new(listener: TcpListener, identity: &ServerIdentity) -> std::io::Result<Self> {
        let local_addr = listener.local_addr()?;
        let acceptor = TlsAcceptor::from(identity.config.clone());
        let (tx, ready) = mpsc::channel(64);
        let pending = Arc::new(Semaphore::new(MAX_PENDING_HANDSHAKES));
        let task = tokio::spawn(async move {
            loop {
                let (tcp, peer) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    // Transient (e.g. descriptor exhaustion): back off, keep serving.
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let Ok(permit) = pending.clone().try_acquire_owned() else {
                    continue; // dropping `tcp` closes it
                };
                let (acceptor, tx) = (acceptor.clone(), tx.clone());
                tokio::spawn(async move {
                    let _permit = permit;
                    // Failed handshakes (plain HTTP, scanners, timeouts) are dropped
                    // without a response body or a log line carrying peer input.
                    let handshake = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp));
                    if let Ok(Ok(tls)) = handshake.await {
                        let _ = tx.send((tls, peer)).await;
                    }
                });
            }
        });
        Ok(Self {
            local_addr,
            ready,
            _acceptor: AbortOnDrop(task),
        })
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.ready.recv().await {
            Some(accepted) => accepted,
            // The acceptor task never exits while `self` holds it; if it panicked,
            // stop accepting rather than spin. Graceful shutdown still completes.
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        Ok(self.local_addr)
    }
}

/// A minimal X.509 v3 certificate, self-signed with SHA-256/RSA. Validity starts
/// a day early to tolerate clock skew and covers more than any task lifetime.
fn self_signed_certificate(
    key: &RsaPrivateKey,
    now: DateTime<Utc>,
) -> anyhow::Result<CertificateDer<'static>> {
    let spki = key
        .to_public_key()
        .to_public_key_der()
        .map_err(|_| anyhow::anyhow!("tls_key_encoding_failed"))?;
    let mut serial = [0u8; 16];
    rsa::rand_core::RngCore::fill_bytes(&mut rsa::rand_core::OsRng, &mut serial);
    serial[0] = (serial[0] & 0x7f) | 0x40; // positive and minimally encoded
    let name = seq(&[&tlv(0x31, &seq(&[COMMON_NAME, &tlv(0x0c, SUBJECT)]))]);
    let validity = seq(&[
        &time(now - chrono::Duration::days(1)),
        &time(now + chrono::Duration::days(365)),
    ]);
    let tbs = seq(&[
        VERSION_3,
        &tlv(0x02, &serial),
        SHA256_WITH_RSA,
        &name,
        &validity,
        &name,
        spki.as_bytes(),
    ]);
    let digest_info = [SHA256_DIGEST_INFO, Sha256::digest(&tbs).as_slice()].concat();
    let signature = key
        .sign(Pkcs1v15Sign::new_unprefixed(), &digest_info)
        .map_err(|_| anyhow::anyhow!("tls_certificate_signing_failed"))?;
    let bits = [&[0x00][..], &signature[..]].concat();
    Ok(CertificateDer::from(seq(&[
        &tbs,
        SHA256_WITH_RSA,
        &tlv(0x03, &bits),
    ])))
}

/// RFC 5280: UTCTime through 2049, GeneralizedTime from 2050.
fn time(at: DateTime<Utc>) -> Vec<u8> {
    use chrono::Datelike;
    if at.year() < 2050 {
        tlv(0x17, at.format("%y%m%d%H%M%SZ").to_string().as_bytes())
    } else {
        tlv(0x18, at.format("%Y%m%d%H%M%SZ").to_string().as_bytes())
    }
}

fn seq(parts: &[&[u8]]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

/// DER tag-length-value with definite, minimal length encoding.
fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    if body.len() < 0x80 {
        out.push(body.len() as u8);
    } else {
        let len = body.len().to_be_bytes();
        let skip = len.iter().take_while(|b| **b == 0).count();
        out.push(0x80 | (len.len() - skip) as u8);
        out.extend_from_slice(&len[skip..]);
    }
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_use_minimal_definite_form() {
        assert_eq!(tlv(0x04, &[0; 3])[..2], [0x04, 3]);
        assert_eq!(tlv(0x04, &[0; 0x80])[..3], [0x04, 0x81, 0x80]);
        assert_eq!(tlv(0x04, &[0; 0x1234])[..4], [0x04, 0x82, 0x12, 0x34]);
    }

    #[test]
    fn validity_switches_to_generalized_time_in_2050() {
        let at = |y| chrono::TimeZone::with_ymd_and_hms(&Utc, y, 1, 2, 3, 4, 5).unwrap();
        assert_eq!(time(at(2049)), tlv(0x17, b"490102030405Z"));
        assert_eq!(time(at(2050)), tlv(0x18, b"20500102030405Z"));
    }
}
