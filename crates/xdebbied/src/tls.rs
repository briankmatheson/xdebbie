// SPDX-License-Identifier: GPL-3.0-or-later
//! HTTPS for the API with optional client certificates.
//!
//! The server asks every client for a certificate and accepts the
//! connection either way. A certificate chaining to the cluster CA
//! (`ca_file`, plus anything in `peer_ca_dir`) identifies another xdebbied:
//! its subject is recorded in the request as [`ClientCert`] and the auth
//! middleware treats it like a valid bearer token. That is how a freshly
//! enrolled box fetches the cluster token, and how daemons forward to the
//! leader before tokens are in sync.

use std::{future::Future, io, path::Path, pin::Pin, sync::Arc};

use anyhow::{Context, Result};
use axum_server::{accept::Accept, tls_rustls::{RustlsAcceptor, RustlsConfig}};
use rustls::{
    RootCertStore, ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    server::WebPkiClientVerifier,
};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;
use tower::Service;

/// Who is on the other end of a TLS connection, when they presented a
/// certificate the cluster CA signed. Present (as a request extension) on
/// every request over such a connection; `None` inside otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientCert(pub Option<PeerIdentity>);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerIdentity {
    /// Subject CN: the hostname the cert was issued for.
    pub hostname: String,
    /// `sha256:<hex>` of the certificate DER.
    pub fingerprint: String,
}

impl PeerIdentity {
    fn from_der(der: &[u8]) -> Option<Self> {
        use sha2::Digest;
        let (_, x) = x509_parser::parse_x509_certificate(der).ok()?;
        let hostname = x
            .subject()
            .iter_common_name()
            .next()
            .and_then(|cn| cn.as_str().ok())
            .unwrap_or("")
            .to_string();
        Some(Self { hostname, fingerprint: format!("sha256:{}", hex::encode(sha2::Sha256::digest(der))) })
    }
}

/// Trust anchors for client certificates: `ca_file` and every `*.pem` in
/// `peer_ca_dir`. Unreadable files are skipped with a warning.
pub fn client_roots(ca_file: &Path, peer_ca_dir: &Path) -> RootCertStore {
    let mut roots = RootCertStore::empty();
    let mut files = vec![ca_file.to_path_buf()];
    if let Ok(rd) = std::fs::read_dir(peer_ca_dir) {
        files.extend(rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "pem")));
    }
    for f in files {
        if !f.exists() {
            continue;
        }
        match CertificateDer::pem_file_iter(&f) {
            Ok(iter) => {
                for c in iter.flatten() {
                    if let Err(e) = roots.add(c) {
                        tracing::warn!("CA cert in {}: {e}", f.display());
                    }
                }
            }
            Err(e) => tracing::warn!("reading CA {}: {e}", f.display()),
        }
    }
    roots
}

/// rustls server config: our cert/key, client certs optional but verified
/// against `roots` when presented. No roots = no client auth at all.
pub fn server_config(cert: &Path, key: &Path, roots: RootCertStore) -> Result<Arc<ServerConfig>> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert)
        .with_context(|| format!("reading {}", cert.display()))?
        .collect::<std::result::Result<_, _>>()
        .with_context(|| format!("parsing {}", cert.display()))?;
    let key = PrivateKeyDer::from_pem_file(key).with_context(|| format!("reading {}", key.display()))?;
    let builder = ServerConfig::builder();
    let mut sc = if roots.is_empty() {
        tracing::warn!("no CA certificates to verify client certs against; peers must use bearer tokens");
        builder.with_no_client_auth().with_single_cert(certs, key)
    } else {
        let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
            .allow_unauthenticated()
            .build()
            .context("building client certificate verifier")?;
        builder.with_client_cert_verifier(verifier).with_single_cert(certs, key)
    }
    .context("loading server certificate/key")?;
    sc.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(sc))
}

/// `axum_server` acceptor: TLS handshake, then tag the per-connection
/// service with the verified peer certificate (if any).
#[derive(Clone)]
pub struct PeerCertAcceptor {
    inner: RustlsAcceptor,
}

impl PeerCertAcceptor {
    pub fn new(config: Arc<ServerConfig>) -> Self {
        Self { inner: RustlsAcceptor::new(RustlsConfig::from_config(config)) }
    }
}

impl<S> Accept<TcpStream, S> for PeerCertAcceptor
where
    S: Send + 'static,
{
    type Stream = TlsStream<TcpStream>;
    type Service = WithClientCert<S>;
    type Future = Pin<Box<dyn Future<Output = io::Result<(Self::Stream, Self::Service)>> + Send>>;

    fn accept(&self, stream: TcpStream, service: S) -> Self::Future {
        let fut = self.inner.accept(stream, service);
        Box::pin(async move {
            let (tls, service) = fut.await?;
            let peer = tls
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|chain| chain.first())
                .and_then(|leaf| PeerIdentity::from_der(leaf));
            if let Some(p) = &peer {
                tracing::debug!(peer = %p.hostname, fp = %p.fingerprint, "client certificate accepted");
            }
            Ok((tls, WithClientCert { inner: service, peer: ClientCert(peer) }))
        })
    }
}

/// Per-connection service that stamps [`ClientCert`] onto every request.
#[derive(Clone)]
pub struct WithClientCert<S> {
    inner: S,
    peer: ClientCert,
}

impl<S, B> Service<axum::http::Request<B>> for WithClientCert<S>
where
    S: Service<axum::http::Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: axum::http::Request<B>) -> Self::Future {
        req.extensions_mut().insert(self.peer.clone());
        self.inner.call(req)
    }
}
