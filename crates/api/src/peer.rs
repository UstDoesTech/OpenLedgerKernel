//! Calling another cell's service. A cross-cell transfer is made by the payer's service asking the
//! payee's service to do the second half, never by reaching into the payee cell's database, so each
//! service keeps the one database secret that is its own.
//!
//! Each call opens its own connection. Calls between cells are few and small, and one connection
//! per call keeps a half-dead connection from ever being reused.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use ledger_core::TenantId;
use rustls::ClientConfig;
use rustls_pki_types::ServerName;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// Why a call to a peer cell did not give an answer.
#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    /// It could not be reached, or it stopped answering. Nothing is known about what it did.
    #[error("the peer cell did not answer: {0}")]
    Unreachable(String),
}

/// How to call one other cell.
pub struct PeerClient {
    host: String,
    port: u16,
    tls: Option<(TlsConnector, ServerName<'static>)>,
    token: String,
}

impl std::fmt::Debug for PeerClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The credential is never shown.
        write!(f, "PeerClient({}:{})", self.host, self.port)
    }
}

impl PeerClient {
    /// `url` is `https://host:port`. Plain `http://` is accepted for a loopback host only, for tests
    /// and a single machine. `tls` is needed for `https`.
    pub fn new(url: &str, token: &str, tls: Option<Arc<ClientConfig>>) -> Result<Self, String> {
        let (secure, rest) = if let Some(r) = url.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = url.strip_prefix("http://") {
            (false, r)
        } else {
            return Err("a peer url starts with https://".to_owned());
        };
        let rest = rest.trim_end_matches('/');
        let (host, port) = rest
            .rsplit_once(':')
            .and_then(|(h, p)| Some((h, p.parse::<u16>().ok()?)))
            .ok_or_else(|| "a peer url has a host and a port".to_owned())?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if token.len() < 32 {
            return Err("a peer credential is at least 32 characters".to_owned());
        }
        let tls = if secure {
            let config = tls.ok_or_else(|| "an https peer needs TLS set up".to_owned())?;
            let name = ServerName::try_from(host.to_owned())
                .map_err(|_| "the peer's host is not a valid name".to_owned())?;
            Some((TlsConnector::from(config), name))
        } else {
            if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
                return Err("a peer that is not on loopback must be https".to_owned());
            }
            None
        };
        Ok(PeerClient {
            host: host.to_owned(),
            port,
            tls,
            token: token.to_owned(),
        })
    }

    /// Makes one call for a tenant and returns the status and the JSON body, whatever the status.
    pub async fn call(
        &self,
        method: &str,
        path: &str,
        tenant: &TenantId,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value), PeerError> {
        let unreachable = |e: &dyn std::fmt::Display| PeerError::Unreachable(e.to_string());
        let work = async {
            let tcp = TcpStream::connect((self.host.as_str(), self.port))
                .await
                .map_err(|e| unreachable(&e))?;
            match &self.tls {
                Some((connector, name)) => {
                    let stream = connector
                        .connect(name.clone(), tcp)
                        .await
                        .map_err(|e| unreachable(&e))?;
                    self.send(TokioIo::new(stream), method, path, tenant, body)
                        .await
                }
                None => {
                    self.send(TokioIo::new(tcp), method, path, tenant, body)
                        .await
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(10), work)
            .await
            .map_err(|_| PeerError::Unreachable("timed out".to_owned()))?
    }

    async fn send<T>(
        &self,
        io: TokioIo<T>,
        method: &str,
        path: &str,
        tenant: &TenantId,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value), PeerError>
    where
        T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let unreachable = |e: &dyn std::fmt::Display| PeerError::Unreachable(e.to_string());
        let (mut sender, connection) = hyper::client::conn::http1::handshake(io)
            .await
            .map_err(|e| unreachable(&e))?;
        tokio::spawn(connection);
        let bytes = body.map(|b| b.to_string()).unwrap_or_default();
        let request = hyper::Request::builder()
            .method(method)
            .uri(path)
            .header("host", &self.host)
            .header("authorization", format!("Bearer {}", self.token))
            .header("x-ledger-tenant", tenant.as_str())
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(bytes)))
            .map_err(|e| unreachable(&e))?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|e| unreachable(&e))?;
        let status = response.status();
        let collected = response
            .into_body()
            .collect()
            .await
            .map_err(|e| unreachable(&e))?
            .to_bytes();
        Ok((
            status,
            serde_json::from_slice(&collected).unwrap_or(Value::Null),
        ))
    }
}
