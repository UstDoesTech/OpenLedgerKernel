//! K-17: certificates and credentials rotate while the service runs. This runs two real
//! `ledger-api` processes with real TLS, mutual TLS between them and cross-cell transfers, then
//! replaces every certificate and every credential while transfers are in flight.
//!
//! Needs Postgres on ports 5431 and 5432 and etcd on `LEDGER_ETCD` (default
//! `http://127.0.0.1:2379`): `cargo test -p api --test rotation -- --ignored`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use ledger_store::migrate;
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};
use rustls::RootCertStore;
use rustls::crypto::ring::default_provider;
use rustls_pki_types::ServerName;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_postgres::NoTls;

const TENANT_TOKEN: &str = "tttttttttttttttttttttttttttttttttttttttt";

struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Authority {
    cert: rcgen::Certificate,
    key: KeyPair,
}

fn authority() -> Authority {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec![]).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let cert = params.self_signed(&key).unwrap();
    Authority { cert, key }
}

/// A new certificate and key for `localhost`, signed by the authority.
fn leaf(ca: &Authority) -> (String, String) {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ServerAuth,
        ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let cert = params.signed_by(&key, &ca.cert, &ca.key).unwrap();
    (cert.pem(), key.serialize_pem())
}

fn token(n: u32) -> String {
    format!("{n:0>40}")
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Writes a file the way the machine does: a new file renamed over the old one.
fn put(dir: &Path, name: &str, text: &str) {
    let new = dir.join(format!("{name}.new"));
    std::fs::write(&new, text).unwrap();
    std::fs::rename(&new, dir.join(name)).unwrap();
}

struct Cell {
    name: &'static str,
    dir: PathBuf,
    public: u16,
    internal: u16,
    _process: Option<Running>,
}

impl Cell {
    fn new(name: &'static str, run: &Path) -> Cell {
        let dir = run.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        Cell {
            name,
            dir,
            public: free_port(),
            internal: free_port(),
            _process: None,
        }
    }

    fn certificate(&self, ca: &Authority) {
        let (crt, key) = leaf(ca);
        put(&self.dir, "server.crt", &crt);
        put(&self.dir, "server.key", &key);
    }

    fn start(&mut self, schema: &str, db_port: u16, prefix: &str) {
        let etcd = std::env::var("LEDGER_ETCD").unwrap_or_else(|_| "http://127.0.0.1:2379".into());
        let d = |f: &str| self.dir.join(f);
        self._process = Some(Running(
            Command::new(env!("CARGO_BIN_EXE_ledger-api"))
                .env("LEDGER_NODE", self.name)
                .env("LEDGER_DB_FILE", d("db.conn"))
                .env("LEDGER_DB_TLS", "off")
                .env("LEDGER_SCHEMA", schema)
                .env("LEDGER_LISTEN", format!("127.0.0.1:{}", self.public))
                .env(
                    "LEDGER_INTERNAL_LISTEN",
                    format!("127.0.0.1:{}", self.internal),
                )
                .env("LEDGER_TOKENS_FILE", d("tokens.json"))
                .env("LEDGER_INTERNAL_AUTH_FILE", d("internal-auth.json"))
                .env("LEDGER_CELLS_FILE", d("cells.json"))
                .env("LEDGER_TLS_CERT_FILE", d("server.crt"))
                .env("LEDGER_TLS_KEY_FILE", d("server.key"))
                .env("LEDGER_TLS_CA_FILE", d("ca.crt"))
                .env("LEDGER_ETCD_ENDPOINTS", etcd)
                .env("LEDGER_ETCD_PREFIX", prefix)
                .env("LEDGER_RELOAD_SECS", "1")
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        ));
        let _ = db_port;
    }
}

/// One HTTPS request to a service, checked against the authority. Returns the status, the body and
/// the certificate the service showed.
async fn https(
    ca: &Authority,
    port: u16,
    method: &str,
    path: &str,
    key: Option<&str>,
    body: Option<Value>,
) -> Result<(u16, Value, Vec<u8>), String> {
    https_as(ca, port, method, path, key, body, TENANT_TOKEN).await
}

/// The same, with the bearer token given.
async fn https_as(
    ca: &Authority,
    port: u16,
    method: &str,
    path: &str,
    key: Option<&str>,
    body: Option<Value>,
    bearer: &str,
) -> Result<(u16, Value, Vec<u8>), String> {
    let mut roots = RootCertStore::empty();
    roots.add(ca.cert.der().clone()).unwrap();
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|e| e.to_string())?;
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let mut tls = connector
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .map_err(|e| e.to_string())?;
    let shown = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|c| c.first())
        .map(|c| c.as_ref().to_vec())
        .unwrap_or_default();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\nauthorization: Bearer {bearer}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n",
        body.len()
    );
    if let Some(k) = key {
        request.push_str(&format!("idempotency-key: {k}\r\n"));
    }
    request.push_str(&format!("\r\n{body}"));
    tls.write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    let _ = tls.read_to_end(&mut out).await;
    let text = String::from_utf8_lossy(&out).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "no answer".to_owned())?;
    let json = text
        .split("\r\n\r\n")
        .nth(1)
        .and_then(|b| serde_json::from_str(b).ok())
        .unwrap_or(Value::Null);
    Ok((status, json, shown))
}

fn cells_json(url_b: Option<(u16, &str)>, url_a: Option<(u16, &str)>) -> String {
    let mut o = serde_json::Map::new();
    if let Some((port, t)) = url_b {
        o.insert(
            "cell-b".into(),
            json!({"url": format!("https://localhost:{port}"), "token": t}),
        );
    }
    if let Some((port, t)) = url_a {
        o.insert(
            "cell-a".into(),
            json!({"url": format!("https://localhost:{port}"), "token": t}),
        );
    }
    Value::Object(o).to_string()
}

fn accept(cell: &str, tokens: &[&str]) -> String {
    Value::Array(
        tokens
            .iter()
            .map(|t| json!({"token": t, "cell": cell, "tenants": ["ten_acme"]}))
            .collect(),
    )
    .to_string()
}

async fn wait_healthy(ca: &Authority, port: u16) {
    let start = Instant::now();
    loop {
        if let Ok((200, _, _)) = https(ca, port, "GET", "/healthz", None, None).await {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "the service did not start"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

struct Pair {
    ca: Arc<Authority>,
    a: Cell,
    b: Cell,
}

async fn pair(name: &str) -> Pair {
    let schema = format!("rot_{name}");
    for port in [5431u16, 5432] {
        let (mut db, conn) = tokio_postgres::Config::new()
            .host("127.0.0.1")
            .port(port)
            .user("ledger")
            .dbname("postgres")
            .connect(NoTls)
            .await
            .unwrap_or_else(|e| panic!("port {port}: {e}"));
        tokio::spawn(conn);
        db.batch_execute(&format!("drop schema if exists {schema} cascade"))
            .await
            .unwrap();
        migrate(&mut db, &schema).await.unwrap();
    }
    let run = std::env::temp_dir().join(format!("ledger-rotation-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&run);
    let ca = Arc::new(authority());
    let mut a = Cell::new("cell-a", &run);
    let mut b = Cell::new("cell-b", &run);
    for (c, port) in [(&a, 5431u16), (&b, 5432)] {
        put(
            &c.dir,
            "db.conn",
            &format!("host=127.0.0.1 port={port} user=ledger dbname=postgres"),
        );
        put(
            &c.dir,
            "tokens.json",
            &format!(r#"[{{"token":"{TENANT_TOKEN}","tenant":"ten_acme","principal":"usr_ada"}}]"#),
        );
        put(&c.dir, "ca.crt", &ca.cert.pem());
        c.certificate(&ca);
    }
    // Credential 1: a calls b with it and b lists it for cell-a, and the other way round.
    put(
        &a.dir,
        "internal-auth.json",
        &accept("cell-b", &[&token(2)]),
    );
    put(
        &b.dir,
        "internal-auth.json",
        &accept("cell-a", &[&token(1)]),
    );
    put(
        &a.dir,
        "cells.json",
        &cells_json(Some((b.internal, &token(1))), None),
    );
    put(
        &b.dir,
        "cells.json",
        &cells_json(None, Some((a.internal, &token(2)))),
    );
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let prefix = format!("/api-rotation-test/{nanos}");
    a.start(&schema, 5431, &prefix);
    b.start(&schema, 5432, &prefix);
    wait_healthy(&ca, a.public).await;
    wait_healthy(&ca, b.public).await;
    Pair { ca, a, b }
}

async fn open(p: &Pair, cell: &Cell, key: &str, overdraft: i64) -> String {
    let (status, body, _) = https(
        &p.ca,
        cell.public,
        "POST",
        "/v1/accounts",
        Some(key),
        Some(json!({"currency":"GBP","overdraft_limit":{"minor":overdraft,"currency":"GBP"}})),
    )
    .await
    .unwrap();
    assert_eq!(status, 201, "{body}");
    body["account_id"]
        .as_str()
        .or_else(|| body["id"].as_str())
        .unwrap()
        .to_owned()
}

/// Sends transfers from a to b, one after another, until told to stop. Counts how many were
/// answered `201` or `200` and how many were not.
fn transfers(
    p: &Pair,
    payer: String,
    payee: String,
    stop: Arc<AtomicBool>,
    ok: Arc<AtomicU32>,
    bad: Arc<AtomicU32>,
) -> tokio::task::JoinHandle<()> {
    let (ca, port) = (p.ca.clone(), p.a.public);
    tokio::spawn(async move {
        let mut n = 0u32;
        while !stop.load(Ordering::Relaxed) {
            n += 1;
            let sent = https(
                &ca,
                port,
                "POST",
                "/v1/transfers",
                Some(&format!("rot-{n}")),
                Some(json!({"payer_account_id": payer, "payee_account_id": payee,
                            "amount": {"minor": 1, "currency": "GBP"}})),
            )
            .await;
            match sent {
                Ok((200 | 201, _, _)) => ok.fetch_add(1, Ordering::Relaxed),
                _ => bad.fetch_add(1, Ordering::Relaxed),
            };
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
}

async fn settle() {
    // The service looks at its files every second.
    tokio::time::sleep(Duration::from_millis(3500)).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431 and 5432 and etcd"]
async fn rotating_every_certificate_and_credential_while_transfers_run_drops_none() {
    let p = pair("live").await;
    let payer = open(&p, &p.a, "open-payer", 100_000).await;
    let payee = open(&p, &p.b, "open-payee", 0).await;

    let (stop, ok, bad) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU32::new(0)),
        Arc::new(AtomicU32::new(0)),
    );
    let sender = transfers(&p, payer, payee, stop.clone(), ok.clone(), bad.clone());
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(ok.load(Ordering::Relaxed) > 0, "transfers did not start");

    let (_, _, before) = https(&p.ca, p.a.public, "GET", "/healthz", None, None)
        .await
        .unwrap();

    // 1. Everyone lists the new credentials as well as the old ones.
    put(
        &p.a.dir,
        "internal-auth.json",
        &accept("cell-b", &[&token(2), &token(4)]),
    );
    put(
        &p.b.dir,
        "internal-auth.json",
        &accept("cell-a", &[&token(1), &token(3)]),
    );
    settle().await;
    // 2. New certificates, and each cell starts using its new credential.
    p.a.certificate(&p.ca);
    p.b.certificate(&p.ca);
    put(
        &p.a.dir,
        "cells.json",
        &cells_json(Some((p.b.internal, &token(3))), None),
    );
    put(
        &p.b.dir,
        "cells.json",
        &cells_json(None, Some((p.a.internal, &token(4)))),
    );
    settle().await;
    // 3. The old credentials are no longer listed.
    put(
        &p.a.dir,
        "internal-auth.json",
        &accept("cell-b", &[&token(4)]),
    );
    put(
        &p.b.dir,
        "internal-auth.json",
        &accept("cell-a", &[&token(3)]),
    );
    settle().await;

    stop.store(true, Ordering::Relaxed);
    sender.await.unwrap();
    let (_, _, after) = https(&p.ca, p.a.public, "GET", "/healthz", None, None)
        .await
        .unwrap();

    assert!(
        ok.load(Ordering::Relaxed) > 50,
        "too few transfers ran to mean anything"
    );
    assert_eq!(
        bad.load(Ordering::Relaxed),
        0,
        "a request was dropped during the rotation"
    );
    assert_ne!(
        before, after,
        "the service is still showing its old certificate"
    );
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431 and 5432 and etcd"]
async fn a_credential_the_other_side_no_longer_lists_is_refused_and_a_removed_cell_stops_at_once() {
    let p = pair("removed").await;
    let payer = open(&p, &p.a, "open-payer", 100_000).await;
    let payee = open(&p, &p.b, "open-payee", 0).await;
    let send = |key: &'static str| {
        let (ca, port, payer, payee) = (p.ca.clone(), p.a.public, payer.clone(), payee.clone());
        async move {
            https(
                &ca,
                port,
                "POST",
                "/v1/transfers",
                Some(key),
                Some(json!({"payer_account_id": payer, "payee_account_id": payee,
                            "amount": {"minor": 1, "currency": "GBP"}})),
            )
            .await
            .map(|(s, _, _)| s)
        }
    };
    assert_eq!(send("first").await, Ok(201));

    // Cell b stops listing the credential cell a holds. Cell a is refused.
    put(
        &p.b.dir,
        "internal-auth.json",
        &accept("cell-a", &[&token(9)]),
    );
    settle().await;
    assert_ne!(send("second").await, Ok(201));

    // Listed again, it works again. Then cell a is removed from the list of cells that b accepts.
    put(
        &p.b.dir,
        "internal-auth.json",
        &accept("cell-a", &[&token(1)]),
    );
    settle().await;
    assert_eq!(send("third").await, Ok(201));
    put(&p.b.dir, "internal-auth.json", "[]");
    settle().await;
    assert_ne!(send("fourth").await, Ok(201));
}

/// K-19b: a token that a later run no longer lists is refused by the running service, and one it
/// lists is accepted, with no restart.
#[tokio::test]
#[ignore = "needs Postgres on ports 5431 and 5432 and etcd"]
async fn a_token_a_later_run_no_longer_lists_is_refused_and_a_new_one_is_accepted() {
    let p = pair("tokens").await;
    let open_as = |bearer: String, key: &'static str| {
        let (ca, port) = (p.ca.clone(), p.a.public);
        async move {
            https_as(
                &ca,
                port,
                "POST",
                "/v1/accounts",
                Some(key),
                Some(json!({"currency":"GBP","overdraft_limit":{"minor":0,"currency":"GBP"}})),
                &bearer,
            )
            .await
            .map(|(s, _, _)| s)
        }
    };
    assert_eq!(open_as(TENANT_TOKEN.into(), "before").await, Ok(201));

    // A run lists a new token and no longer lists the old one.
    let fresh = "f".repeat(64);
    put(
        &p.a.dir,
        "tokens.json",
        &format!(r#"[{{"token":"{fresh}","tenant":"ten_acme","principal":"usr_proving"}}]"#),
    );
    settle().await;
    assert_eq!(open_as(TENANT_TOKEN.into(), "old").await, Ok(401));
    assert_eq!(open_as(fresh.clone(), "new").await, Ok(201));
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431 and 5432 and etcd"]
async fn a_certificate_that_does_not_match_its_key_changes_nothing() {
    let p = pair("mismatch").await;
    let (_, _, before) = https(&p.ca, p.a.public, "GET", "/healthz", None, None)
        .await
        .unwrap();
    // A new certificate arrives before its key: the pair does not match.
    let (crt, _) = leaf(&p.ca);
    put(&p.a.dir, "server.crt", &crt);
    settle().await;
    let (status, _, shown) = https(&p.ca, p.a.public, "GET", "/healthz", None, None)
        .await
        .unwrap();
    assert_eq!(status, 200);
    assert_eq!(
        shown, before,
        "a mismatched pair must leave the old one in force"
    );
}
