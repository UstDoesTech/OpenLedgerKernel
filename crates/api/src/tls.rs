//! TLS for the two listeners and for calls between cells.
//!
//! The public listener serves TLS, and the internal listener serves mutual TLS: a peer cell
//! shows a certificate from the same authority as ours, as well as its credential. Both refuse
//! any address but loopback unless TLS is set up, so a bearer token never crosses a network in the
//! clear. See `docs/credentialed-workflows.md` for how certificates reach a cell.

use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rustls::ClientConfig;
use rustls::RootCertStore;
use rustls::ServerConfig;
use rustls::crypto::ring::default_provider;
use rustls::server::WebPkiClientVerifier;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::server::TlsStream;

use crate::reload::Reloadable;

/// Where the certificate, key and authority are, as files. They are read at start and read again
/// when they change (see `crate::reload`), so a rotation needs no restart.
#[derive(Debug, Clone, Default)]
pub struct TlsFiles {
    /// This cell's certificate chain, in PEM.
    pub cert: Option<String>,
    /// This cell's private key, in PEM.
    pub key: Option<String>,
    /// The authority that signs every cell's certificate, in PEM. Peers are checked against it.
    pub ca: Option<String>,
}

/// Why TLS could not be set up.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("{0}")]
    Config(String),
}

fn err(what: &str, e: impl std::fmt::Display) -> TlsError {
    TlsError::Config(format!("{what}: {e}"))
}

fn certs(path: &str) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let found: Result<Vec<_>, _> = CertificateDer::pem_file_iter(Path::new(path))
        .map_err(|e| err("certificate file", e))?
        .collect();
    let found = found.map_err(|e| err("certificate file", e))?;
    if found.is_empty() {
        return Err(TlsError::Config(
            "certificate file holds no certificate".into(),
        ));
    }
    Ok(found)
}

fn key(path: &str) -> Result<PrivateKeyDer<'static>, TlsError> {
    PrivateKeyDer::from_pem_file(Path::new(path)).map_err(|e| err("key file", e))
}

fn roots(path: &str) -> Result<RootCertStore, TlsError> {
    let mut store = RootCertStore::empty();
    for c in certs(path)? {
        store.add(c).map_err(|e| err("authority file", e))?;
    }
    Ok(store)
}

fn pair(
    files: &TlsFiles,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), TlsError> {
    match (&files.cert, &files.key) {
        (Some(c), Some(k)) => Ok((certs(c)?, key(k)?)),
        _ => Err(TlsError::Config(
            "a certificate and a key are both needed".into(),
        )),
    }
}

/// A server that wants no client certificate: the public listener.
pub fn server_config(files: &TlsFiles) -> Result<Arc<ServerConfig>, TlsError> {
    let (chain, k) = pair(files)?;
    let config = ServerConfig::builder_with_provider(Arc::new(default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| err("tls versions", e))?
        .with_no_client_auth()
        .with_single_cert(chain, k)
        .map_err(|e| err("certificate and key", e))?;
    Ok(Arc::new(config))
}

/// A server that requires a client certificate signed by our authority: the internal listener.
pub fn mutual_server_config(files: &TlsFiles) -> Result<Arc<ServerConfig>, TlsError> {
    let (chain, k) = pair(files)?;
    let ca = files
        .ca
        .as_deref()
        .ok_or_else(|| TlsError::Config("an authority file is needed to check peers".into()))?;
    let verifier = WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots(ca)?),
        Arc::new(default_provider()),
    )
    .build()
    .map_err(|e| err("client verifier", e))?;
    let config = ServerConfig::builder_with_provider(Arc::new(default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| err("tls versions", e))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(chain, k)
        .map_err(|e| err("certificate and key", e))?;
    Ok(Arc::new(config))
}

/// What a cell uses to call another: it checks the peer against our authority and shows its own
/// certificate.
pub fn client_config(files: &TlsFiles) -> Result<Arc<ClientConfig>, TlsError> {
    let ca = files
        .ca
        .as_deref()
        .ok_or_else(|| TlsError::Config("an authority file is needed to check peers".into()))?;
    let (chain, k) = pair(files)?;
    let config = ClientConfig::builder_with_provider(Arc::new(default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| err("tls versions", e))?
        .with_root_certificates(roots(ca)?)
        .with_client_auth_cert(chain, k)
        .map_err(|e| err("certificate and key", e))?;
    Ok(Arc::new(config))
}

/// Refuses an address that is not loopback unless TLS is set up for it.
pub fn check_bind(addr: &SocketAddr, tls: bool, what: &str) -> Result<(), String> {
    if tls || addr.ip().is_loopback() {
        Ok(())
    } else {
        Err(format!(
            "{what} is {addr}, which is not loopback, and no TLS certificate and key are set: \
             a token must not cross a network in the clear"
        ))
    }
}

/// Accepts TCP connections, finishes each TLS handshake off the accept loop, and hands the
/// established streams to `axum::serve`. A client that stalls its handshake costs nobody else.
pub struct TlsListener {
    rx: mpsc::Receiver<(TlsStream<tokio::net::TcpStream>, SocketAddr)>,
    local: SocketAddr,
}

impl TlsListener {
    pub fn new(tcp: TcpListener, config: Arc<ServerConfig>) -> io::Result<Self> {
        Self::reloadable(tcp, Reloadable::from_arc(config))
    }

    /// Like [`TlsListener::new`], but each connection is accepted with whatever config is in force
    /// when it arrives, so a new certificate serves from the next connection on. A connection
    /// already open keeps the certificate it began with.
    pub fn reloadable(tcp: TcpListener, config: Reloadable<ServerConfig>) -> io::Result<Self> {
        let local = tcp.local_addr()?;
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            loop {
                let Ok((stream, peer)) = tcp.accept().await else {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                };
                if tx.is_closed() {
                    break;
                }
                let acceptor = TlsAcceptor::from(config.get());
                let tx = tx.clone();
                tokio::spawn(async move {
                    let shaken =
                        tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream))
                            .await;
                    if let Ok(Ok(tls)) = shaken {
                        let _ = tx.send((tls, peer)).await;
                    }
                });
            }
        });
        Ok(TlsListener { rx, local })
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<tokio::net::TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            if let Some(pair) = self.rx.recv().await {
                return pair;
            }
            // The accept loop has ended; there is nothing more to serve.
            std::future::pending::<()>().await;
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(self.local)
    }
}
