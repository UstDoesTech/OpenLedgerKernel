//! The smallest HTTP client the cloud secret stores need, behind a trait so their logic can be
//! tested without a network.

use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl Request {
    pub fn new(method: &'static str, url: impl Into<String>) -> Self {
        Request {
            method,
            url: url.into(),
            headers: vec![],
            body: None,
        }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn header_value(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct HttpError(pub String);

/// Sends one request and returns whatever status came back. A 4xx or 5xx is a response, not an
/// error: only a failure to get any answer is an `HttpError`.
pub trait Http: Send + Sync {
    fn send(&self, request: &Request) -> Result<Response, HttpError>;
}

/// The real client. HTTPS uses the system's OpenSSL and the system's trusted certificates.
pub struct UreqHttp {
    agent: ureq::Agent,
}

impl UreqHttp {
    pub fn new() -> Self {
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .timeout_global(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .new_agent();
        UreqHttp { agent }
    }
}

impl Default for UreqHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl Http for UreqHttp {
    fn send(&self, request: &Request) -> Result<Response, HttpError> {
        let mut builder = ureq::http::Request::builder()
            .method(request.method)
            .uri(&request.url);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        let built = builder
            .body(request.body.clone().unwrap_or_default())
            .map_err(|e| HttpError(format!("bad request: {e}")))?;
        let mut response = self
            .agent
            .run(built)
            .map_err(|e| HttpError(format!("{} {}: {e}", request.method, host_of(&request.url))))?;
        let status = response.status().as_u16();
        let body = response.body_mut().read_to_vec().map_err(|e| {
            HttpError(format!(
                "reading the answer from {}: {e}",
                host_of(&request.url)
            ))
        })?;
        Ok(Response { status, body })
    }
}

/// The host part of a URL, for error messages. The path and query are left out, since they can
/// name a secret.
pub fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?']).next().unwrap_or(rest)
}
