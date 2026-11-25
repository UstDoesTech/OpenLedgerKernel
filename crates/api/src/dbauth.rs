//! How the service signs in to its cell's database when the database is managed (D-24).
//!
//! A managed cell has no password for the service. The cell's cloud, or Databricks, assigns the
//! database role to an identity, and the service asks that identity for a short-lived token each
//! time it connects. The token is used as the password for that one connection. A connection that
//! is open keeps working after its token has expired, because the database checks the token only
//! when the connection is made.
//!
//! Every source sits behind [`TokenSource`], so tests use [`SimulatedTokens`] and the real ones are
//! checked against recorded responses. Rules this module keeps:
//! - a token, a secret, a host, a user or a response body is never logged, returned in an error or
//!   put in a `Debug` output. Errors are one of three fixed words;
//! - plain HTTP goes only to a loopback or metadata address, never to anything else;
//! - the sources read only the machine's own identity. The one exception is Lakebase, which signs
//!   in as a Databricks service principal whose secret is read from a file each time.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use ring::{digest, hmac};
use serde_json::Value;
use tokio::net::TcpStream;

/// How the service signs in to its database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignIn {
    /// The password in `db.conn`. The default, and the only way for a self-hosted cell.
    Password,
    /// An AWS IAM role, through an RDS authentication token.
    AwsIam,
    /// A managed identity on Azure, through a Microsoft Entra token.
    AzureEntra,
    /// A service account on Google Cloud, through its access token.
    GcpIam,
    /// A Databricks service principal, through a Lakebase database credential.
    LakebaseOauth,
}

impl SignIn {
    /// Reads the setting. Anything but one of the five words is refused, so a typo stops the
    /// service at start and does not fall back to a password.
    pub fn parse(text: &str) -> Result<SignIn, String> {
        match text.trim() {
            "password" => Ok(SignIn::Password),
            "aws_iam" => Ok(SignIn::AwsIam),
            "azure_entra" => Ok(SignIn::AzureEntra),
            "gcp_iam" => Ok(SignIn::GcpIam),
            "lakebase_oauth" => Ok(SignIn::LakebaseOauth),
            _ => Err(
                "LEDGER_DB_SIGN_IN: must be password, aws_iam, azure_entra, gcp_iam or lakebase_oauth"
                    .to_owned(),
            ),
        }
    }

    /// Whether the database role belongs to an identity, so `db.conn` holds no password.
    pub fn by_identity(self) -> bool {
        self != SignIn::Password
    }
}

/// A token to use as the password for one connection. It prints as nothing.
pub struct Token {
    secret: String,
    valid_for: Duration,
}

impl Token {
    pub fn new(secret: String, valid_for: Duration) -> Self {
        Token { secret, valid_for }
    }

    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// How long the source says the token may be used to connect.
    pub fn valid_for(&self) -> Duration {
        self.valid_for
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(..)")
    }
}

/// Why a token could not be had. Never carries text from the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    /// The source did not answer, or answered with a server error.
    Unavailable,
    /// The source answered and said no: the identity is not allowed, or its secret is wrong.
    Refused,
    /// The source answered with something that is not a token.
    Malformed,
}

impl TokenError {
    pub fn text(self) -> &'static str {
        match self {
            TokenError::Unavailable => "the identity service did not answer",
            TokenError::Refused => "the identity service refused this machine's identity",
            TokenError::Malformed => "the identity service gave an answer that is not a token",
        }
    }
}

pub type TokenFuture<'a> = Pin<Box<dyn Future<Output = Result<Token, TokenError>> + Send + 'a>>;

/// Something that can give a token for a new connection.
pub trait TokenSource: Send + Sync {
    fn token(&self) -> TokenFuture<'_>;
}

/// A source for tests: it gives `<prefix>1`, `<prefix>2` and so on, and can be told to fail.
pub struct SimulatedTokens {
    prefix: String,
    issued: AtomicU64,
    failing: AtomicBool,
}

impl SimulatedTokens {
    pub fn new(prefix: &str) -> Self {
        SimulatedTokens {
            prefix: prefix.to_owned(),
            issued: AtomicU64::new(0),
            failing: AtomicBool::new(false),
        }
    }

    /// How many tokens have been given.
    pub fn issued(&self) -> u64 {
        self.issued.load(Ordering::SeqCst)
    }

    /// The token the source gives as its `n`th (counting from 1).
    pub fn nth(&self, n: u64) -> String {
        format!("{}{n}", self.prefix)
    }

    pub fn set_failing(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
    }
}

impl TokenSource for SimulatedTokens {
    fn token(&self) -> TokenFuture<'_> {
        Box::pin(async move {
            if self.failing.load(Ordering::SeqCst) {
                return Err(TokenError::Unavailable);
            }
            let n = self.issued.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(Token::new(self.nth(n), Duration::from_secs(900)))
        })
    }
}

// ---------------------------------------------------------------------------------------------
// A small HTTP client for the identity services.

struct Url {
    https: bool,
    host: String,
    port: u16,
    /// The `Host` header: the host, with the port when it is not the scheme's own.
    authority: String,
    path: String,
}

fn split(url: &str) -> Option<Url> {
    let (https, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else {
        (false, url.strip_prefix("http://")?)
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let given = authority
        .rsplit_once(':')
        .and_then(|(h, p)| p.parse::<u16>().ok().map(|p| (h, p)));
    let default = if https { 443 } else { 80 };
    let (host, port) = match given {
        Some((h, p)) => (h.to_owned(), p),
        None => (authority.to_owned(), default),
    };
    if host.is_empty() {
        return None;
    }
    let authority = if port == default {
        host.clone()
    } else {
        format!("{host}:{port}")
    };
    Some(Url {
        https,
        host,
        port,
        authority,
        path: path.to_owned(),
    })
}

/// The only hosts plain HTTP may go to: this machine, and the cloud metadata services.
fn plain_http_allowed(host: &str) -> bool {
    matches!(
        host,
        "127.0.0.1" | "localhost" | "[::1]" | "169.254.169.254" | "metadata.google.internal"
    )
}

struct Reply {
    status: u16,
    body: Vec<u8>,
}

async fn call(
    method: &str,
    url: &str,
    headers: &[(&str, String)],
    body: Option<(&str, Vec<u8>)>,
) -> Result<Reply, TokenError> {
    let u = split(url).ok_or(TokenError::Malformed)?;
    if !u.https && !plain_http_allowed(&u.host) {
        return Err(TokenError::Refused);
    }
    let work = async {
        let tcp = TcpStream::connect((u.host.trim_matches(['[', ']']), u.port))
            .await
            .map_err(|_| TokenError::Unavailable)?;
        if u.https {
            let native = native_tls::TlsConnector::new().map_err(|_| TokenError::Unavailable)?;
            let stream = tokio_native_tls::TlsConnector::from(native)
                .connect(&u.host, tcp)
                .await
                .map_err(|_| TokenError::Unavailable)?;
            send(TokioIo::new(stream), method, &u, headers, body).await
        } else {
            send(TokioIo::new(tcp), method, &u, headers, body).await
        }
    };
    tokio::time::timeout(Duration::from_secs(5), work)
        .await
        .map_err(|_| TokenError::Unavailable)?
}

async fn send<T>(
    io: TokioIo<T>,
    method: &str,
    u: &Url,
    headers: &[(&str, String)],
    body: Option<(&str, Vec<u8>)>,
) -> Result<Reply, TokenError>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|_| TokenError::Unavailable)?;
    tokio::spawn(connection);
    let mut request = hyper::Request::builder()
        .method(method)
        .uri(&u.path)
        .header("host", &u.authority);
    for (k, v) in headers {
        request = request.header(*k, v);
    }
    let bytes = match body {
        Some((content_type, bytes)) => {
            request = request.header("content-type", content_type);
            bytes
        }
        None => Vec::new(),
    };
    let request = request
        .body(Full::new(Bytes::from(bytes)))
        .map_err(|_| TokenError::Malformed)?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|_| TokenError::Unavailable)?;
    let status = response.status().as_u16();
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|_| TokenError::Unavailable)?
        .to_bytes()
        .to_vec();
    Ok(Reply { status, body })
}

/// A good answer, or the word for what went wrong.
fn ok(reply: Reply) -> Result<Vec<u8>, TokenError> {
    match reply.status {
        200..=299 => Ok(reply.body),
        400..=499 => Err(TokenError::Refused),
        _ => Err(TokenError::Unavailable),
    }
}

fn json(bytes: &[u8]) -> Result<Value, TokenError> {
    serde_json::from_slice(bytes).map_err(|_| TokenError::Malformed)
}

fn text_of<'a>(v: &'a Value, key: &str) -> Result<&'a str, TokenError> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(TokenError::Malformed)
}

/// Seconds, given as a number or as a string of digits, which is how Azure sends them.
fn seconds(v: &Value, key: &str) -> Duration {
    let n = match v.get(key) {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::String(s)) => s.parse().ok(),
        _ => None,
    };
    Duration::from_secs(n.unwrap_or(300))
}

fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------------------------
// Azure: a managed identity, through the instance metadata service.

const AZURE_METADATA: &str = "http://169.254.169.254";

/// A managed identity on Azure. A user-assigned identity is chosen by its client id, which names
/// the identity and is not a secret.
pub struct AzureEntra {
    base: String,
    client_id: Option<String>,
}

impl AzureEntra {
    pub fn new(client_id: Option<String>) -> Self {
        Self::at(AZURE_METADATA, client_id)
    }

    /// For tests: the metadata service at another address.
    pub fn at(base: &str, client_id: Option<String>) -> Self {
        AzureEntra {
            base: base.trim_end_matches('/').to_owned(),
            client_id,
        }
    }
}

impl TokenSource for AzureEntra {
    fn token(&self) -> TokenFuture<'_> {
        Box::pin(async move {
            let mut url = format!(
                "{}/metadata/identity/oauth2/token?api-version=2018-02-01&resource={}",
                self.base,
                encode("https://ossrdbms-aad.database.windows.net")
            );
            if let Some(id) = &self.client_id {
                url.push_str(&format!("&client_id={}", encode(id)));
            }
            let reply = call("GET", &url, &[("metadata", "true".to_owned())], None).await?;
            let v = json(&ok(reply)?)?;
            Ok(Token::new(
                text_of(&v, "access_token")?.to_owned(),
                seconds(&v, "expires_in"),
            ))
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Google Cloud: the instance's service account, through the metadata server.

const GCP_METADATA: &str = "http://metadata.google.internal";

/// The service account of the instance, with the scope Cloud SQL asks for.
pub struct GcpIam {
    base: String,
}

impl GcpIam {
    pub fn new() -> Self {
        Self::at(GCP_METADATA)
    }

    /// For tests: the metadata server at another address.
    pub fn at(base: &str) -> Self {
        GcpIam {
            base: base.trim_end_matches('/').to_owned(),
        }
    }
}

impl Default for GcpIam {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenSource for GcpIam {
    fn token(&self) -> TokenFuture<'_> {
        Box::pin(async move {
            let url = format!(
                "{}/computeMetadata/v1/instance/service-accounts/default/token?scopes={}",
                self.base,
                encode("https://www.googleapis.com/auth/sqlservice.login")
            );
            let reply = call(
                "GET",
                &url,
                &[("metadata-flavor", "Google".to_owned())],
                None,
            )
            .await?;
            let v = json(&ok(reply)?)?;
            Ok(Token::new(
                text_of(&v, "access_token")?.to_owned(),
                seconds(&v, "expires_in"),
            ))
        })
    }
}

// ---------------------------------------------------------------------------------------------
// AWS: the instance's role, through the metadata service, signing an RDS token.

const AWS_METADATA: &str = "http://169.254.169.254";

/// Short-lived credentials of the machine's role.
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: String,
}

/// The RDS authentication token for a role, which is a signed request that is never sent: the
/// database checks the signature. It is valid for 15 minutes, to connect with.
pub fn rds_token(
    host: &str,
    port: u16,
    user: &str,
    region: &str,
    creds: &AwsCredentials,
    unix_secs: u64,
) -> String {
    let (date, stamp) = civil(unix_secs);
    let scope = format!("{date}/{region}/rds-db/aws4_request");
    let mut params = vec![
        ("Action", "connect".to_owned()),
        ("DBUser", user.to_owned()),
        ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_owned()),
        (
            "X-Amz-Credential",
            format!("{}/{scope}", creds.access_key_id),
        ),
        ("X-Amz-Date", stamp.clone()),
        ("X-Amz-Expires", "900".to_owned()),
        ("X-Amz-SignedHeaders", "host".to_owned()),
    ];
    if !creds.session_token.is_empty() {
        params.push(("X-Amz-Security-Token", creds.session_token.clone()));
    }
    params.sort();
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let empty = hex(digest::digest(&digest::SHA256, b"").as_ref());
    let canonical = format!("GET\n/\n{query}\nhost:{host}:{port}\n\nhost\n{empty}");
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}",
        hex(digest::digest(&digest::SHA256, canonical.as_bytes()).as_ref())
    );
    let mut key = hmac::sign(
        &hmac::Key::new(
            hmac::HMAC_SHA256,
            format!("AWS4{}", creds.secret_access_key).as_bytes(),
        ),
        date.as_bytes(),
    );
    for part in [region, "rds-db", "aws4_request"] {
        key = hmac::sign(
            &hmac::Key::new(hmac::HMAC_SHA256, key.as_ref()),
            part.as_bytes(),
        );
    }
    let signature = hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA256, key.as_ref()),
        to_sign.as_bytes(),
    );
    format!(
        "{host}:{port}/?{query}&X-Amz-Signature={}",
        hex(signature.as_ref())
    )
}

/// `YYYYMMDD` and `YYYYMMDDTHHMMSSZ` for a time in seconds since 1970, in UTC.
fn civil(unix_secs: u64) -> (String, String) {
    let days = (unix_secs / 86_400) as i64;
    let rest = unix_secs % 86_400;
    // Days since 1970 to a calendar date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let date = format!("{y:04}{m:02}{d:02}");
    let stamp = format!(
        "{date}T{:02}{:02}{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    );
    (date, stamp)
}

/// The region a hostname carries, such as `eu-west-2` in an RDS address.
pub fn region_of_host(host: &str) -> Option<String> {
    host.split('.')
        .find(|label| {
            let parts: Vec<&str> = label.split('-').collect();
            parts.len() == 3
                && parts[0].len() == 2
                && parts[0].bytes().all(|b| b.is_ascii_lowercase())
                && parts[1].bytes().all(|b| b.is_ascii_lowercase())
                && parts[2].len() == 1
                && parts[2].bytes().all(|b| b.is_ascii_digit())
        })
        .map(str::to_owned)
}

/// The role of the instance, through the metadata service with its session token (IMDSv2).
pub struct AwsIam {
    base: String,
    host: String,
    port: u16,
    user: String,
    region: String,
}

impl AwsIam {
    pub fn new(host: &str, port: u16, user: &str, region: &str) -> Self {
        Self::at(AWS_METADATA, host, port, user, region)
    }

    /// For tests: the metadata service at another address.
    pub fn at(base: &str, host: &str, port: u16, user: &str, region: &str) -> Self {
        AwsIam {
            base: base.trim_end_matches('/').to_owned(),
            host: host.to_owned(),
            port,
            user: user.to_owned(),
            region: region.to_owned(),
        }
    }

    async fn credentials(&self) -> Result<AwsCredentials, TokenError> {
        let session = call(
            "PUT",
            &format!("{}/latest/api/token", self.base),
            &[("x-aws-ec2-metadata-token-ttl-seconds", "60".to_owned())],
            None,
        )
        .await?;
        let session = String::from_utf8(ok(session)?).map_err(|_| TokenError::Malformed)?;
        let with = [("x-aws-ec2-metadata-token", session)];
        let roles = call(
            "GET",
            &format!("{}/latest/meta-data/iam/security-credentials/", self.base),
            &with,
            None,
        )
        .await?;
        let roles = String::from_utf8(ok(roles)?).map_err(|_| TokenError::Malformed)?;
        let role = roles
            .lines()
            .next()
            .map(str::trim)
            .filter(|r| !r.is_empty());
        let role = role.ok_or(TokenError::Malformed)?;
        let reply = call(
            "GET",
            &format!(
                "{}/latest/meta-data/iam/security-credentials/{}",
                self.base,
                encode(role)
            ),
            &with,
            None,
        )
        .await?;
        let v = json(&ok(reply)?)?;
        Ok(AwsCredentials {
            access_key_id: text_of(&v, "AccessKeyId")?.to_owned(),
            secret_access_key: text_of(&v, "SecretAccessKey")?.to_owned(),
            session_token: text_of(&v, "Token")?.to_owned(),
        })
    }
}

impl TokenSource for AwsIam {
    fn token(&self) -> TokenFuture<'_> {
        Box::pin(async move {
            let creds = self.credentials().await?;
            let secret = rds_token(
                &self.host,
                self.port,
                &self.user,
                &self.region,
                &creds,
                now_secs(),
            );
            Ok(Token::new(secret, Duration::from_secs(900)))
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Lakebase: a Databricks service principal, in two steps.

/// A Databricks service principal. The file holds JSON with `workspace` (an https address),
/// `client_id`, `client_secret` and `endpoint` (the Lakebase endpoint's resource name). It is read
/// each time, so a new secret is used as soon as it is written.
pub struct LakebaseOauth {
    file: PathBuf,
}

impl LakebaseOauth {
    pub fn new(file: PathBuf) -> Self {
        LakebaseOauth { file }
    }
}

impl TokenSource for LakebaseOauth {
    fn token(&self) -> TokenFuture<'_> {
        Box::pin(async move {
            let text = std::fs::read_to_string(&self.file).map_err(|_| TokenError::Unavailable)?;
            let v = json(text.as_bytes())?;
            let workspace = text_of(&v, "workspace")?.trim_end_matches('/');
            let id = text_of(&v, "client_id")?;
            let secret = text_of(&v, "client_secret")?;
            let endpoint = text_of(&v, "endpoint")?;
            let basic = base64(format!("{id}:{secret}").as_bytes());
            let first = call(
                "POST",
                &format!("{workspace}/oidc/v1/token"),
                &[("authorization", format!("Basic {basic}"))],
                Some((
                    "application/x-www-form-urlencoded",
                    b"grant_type=client_credentials&scope=all-apis".to_vec(),
                )),
            )
            .await?;
            let workspace_token = text_of(&json(&ok(first)?)?, "access_token")?.to_owned();
            let body = serde_json::json!({ "endpoint": endpoint }).to_string();
            let second = call(
                "POST",
                &format!("{workspace}/api/2.0/postgres/credentials"),
                &[("authorization", format!("Bearer {workspace_token}"))],
                Some(("application/json", body.into_bytes())),
            )
            .await?;
            let v = json(&ok(second)?)?;
            Ok(Token::new(
                text_of(&v, "token")?.to_owned(),
                Duration::from_secs(300),
            ))
        })
    }
}

// ---------------------------------------------------------------------------------------------

/// Where the database is and who the service is there.
pub struct Target {
    pub host: String,
    pub port: u16,
    pub user: String,
}

/// The settings the sources need beyond `db.conn`.
#[derive(Default)]
pub struct Settings {
    /// `LEDGER_DB_REGION`: the AWS region, when the host does not carry it.
    pub region: Option<String>,
    /// `LEDGER_DB_IDENTITY_CLIENT_ID`: picks a user-assigned managed identity on Azure.
    pub identity_client_id: Option<String>,
    /// `LEDGER_DB_OAUTH_FILE`: the service principal's file, for Lakebase.
    pub oauth_file: Option<PathBuf>,
}

/// The source for a sign-in setting. A setting that a source needs and does not have stops the
/// service at start.
pub fn source_for(
    sign_in: SignIn,
    target: &Target,
    settings: &Settings,
) -> Result<Arc<dyn TokenSource>, String> {
    Ok(match sign_in {
        SignIn::Password => return Err("password sign-in has no token source".to_owned()),
        SignIn::AwsIam => {
            let region = settings
                .region
                .clone()
                .or_else(|| region_of_host(&target.host))
                .ok_or("aws_iam: set LEDGER_DB_REGION, because the host does not carry a region")?;
            Arc::new(AwsIam::new(
                &target.host,
                target.port,
                &target.user,
                &region,
            ))
        }
        SignIn::AzureEntra => Arc::new(AzureEntra::new(settings.identity_client_id.clone())),
        SignIn::GcpIam => Arc::new(GcpIam::new()),
        SignIn::LakebaseOauth => {
            let file = settings
                .oauth_file
                .clone()
                .ok_or("lakebase_oauth: set LEDGER_DB_OAUTH_FILE")?;
            Arc::new(LakebaseOauth::new(file))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_five_words_are_a_sign_in_setting() {
        assert_eq!(SignIn::parse("password"), Ok(SignIn::Password));
        assert_eq!(SignIn::parse("aws_iam"), Ok(SignIn::AwsIam));
        assert_eq!(SignIn::parse("azure_entra"), Ok(SignIn::AzureEntra));
        assert_eq!(SignIn::parse("gcp_iam"), Ok(SignIn::GcpIam));
        assert_eq!(SignIn::parse("lakebase_oauth"), Ok(SignIn::LakebaseOauth));
        for bad in ["", "Password", "aws", "iam", "passwords", "none"] {
            assert!(SignIn::parse(bad).is_err(), "{bad:?}");
        }
        assert!(!SignIn::Password.by_identity());
        assert!(SignIn::GcpIam.by_identity());
    }

    #[test]
    fn a_token_prints_as_nothing() {
        let t = Token::new("sensitive-value".to_owned(), Duration::from_secs(1));
        assert!(!format!("{t:?}").contains("sensitive"));
    }

    #[test]
    fn the_time_is_written_the_way_signing_wants_it() {
        assert_eq!(
            civil(0),
            ("19700101".to_owned(), "19700101T000000Z".to_owned())
        );
        assert_eq!(
            civil(1_800_000_000),
            ("20270115".to_owned(), "20270115T080000Z".to_owned())
        );
        // A leap day.
        assert_eq!(civil(1_709_210_096).0, "20240229");
    }

    #[test]
    fn base64_matches_the_known_forms() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn a_region_is_read_from_an_rds_address() {
        assert_eq!(
            region_of_host("db1.abc123.eu-west-2.rds.amazonaws.com").as_deref(),
            Some("eu-west-2")
        );
        assert_eq!(region_of_host("db.internal.example"), None);
    }

    #[test]
    fn plain_http_goes_only_to_this_machine_and_the_metadata_services() {
        assert!(plain_http_allowed("169.254.169.254"));
        assert!(plain_http_allowed("127.0.0.1"));
        assert!(!plain_http_allowed("example.com"));
        assert!(!plain_http_allowed("10.0.0.5"));
    }

    #[test]
    fn urls_are_split_into_host_port_and_path() {
        let u = split("https://adb.example.net/oidc/v1/token").unwrap();
        assert!(
            u.https && u.port == 443 && u.host == "adb.example.net" && u.path == "/oidc/v1/token"
        );
        let u = split("http://127.0.0.1:8081/x?y=1").unwrap();
        assert!(!u.https && u.port == 8081 && u.path == "/x?y=1");
        assert!(split("ftp://x").is_none());
        assert!(split("http://").is_none());
    }

    /// The token for fixed inputs, which `tests/test_database_identity_sign_in.py` recomputes with
    /// a second implementation. Values are derived at run time so none is written as a literal.
    #[test]
    fn the_rds_token_for_fixed_inputs_is_the_one_a_second_implementation_makes() {
        let h = |label: &str| hex(digest::digest(&digest::SHA256, label.as_bytes()).as_ref());
        let creds = AwsCredentials {
            access_key_id: format!("AKIA{}", h("akid")[..16].to_uppercase()),
            secret_access_key: h("secret")[..40].to_owned(),
            session_token: h("session"),
        };
        let got = rds_token(
            "db1.example.eu-west-2.rds.amazonaws.com",
            5432,
            "ledger_api",
            "eu-west-2",
            &creds,
            1_800_000_000,
        );
        let expected = concat!(
            "db1.example.eu-west-2.rds.amazonaws.com:5432/?Action=connect&DBUser=ledger_api",
            "&X-Amz-Algorithm=AWS4-HMAC-SHA256",
            "&X-Amz-Credential=AKIA10EE280719AE6827%2F20270115%2Feu-west-2%2Frds-db%2Faws4_request",
            "&X-Amz-Date=20270115T080000Z&X-Amz-Expires=900",
            "&X-Amz-Security-Token=3f3af1ecebbd1410ab417ec0d27bbfcb5d340e177ae159b59fc8626c2dfd9175",
            "&X-Amz-SignedHeaders=host",
            "&X-Amz-Signature=3aa41eeed0530951fd0301b49696701a0d7ad577005a9a04874762b71989d817"
        );
        assert_eq!(got, expected);
    }

    #[tokio::test]
    async fn the_simulated_source_counts_and_can_fail() {
        let s = SimulatedTokens::new("t");
        assert_eq!(s.token().await.unwrap().secret(), "t1");
        assert_eq!(s.token().await.unwrap().secret(), "t2");
        s.set_failing(true);
        assert_eq!(s.token().await.unwrap_err(), TokenError::Unavailable);
        assert_eq!(s.issued(), 2);
    }
}
