//! K-11: the service starts next to a cell, reads the cell's database and etcd, and answers.
//! This runs the real `ledger-api` binary. Needs Postgres on port 5431 and etcd on 2379
//! (`make up`): `cargo test -p api --test binary -- --ignored`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ledger_store::migrate;
use tokio_postgres::NoTls;

const TOKEN: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn http(addr: &str, request: &str) -> Option<(u16, String)> {
    let mut s = TcpStream::connect(addr).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    s.write_all(request.as_bytes()).ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    let status = out.split_whitespace().nth(1)?.parse().ok()?;
    let body = out.split("\r\n\r\n").nth(1).unwrap_or("").to_owned();
    Some((status, body))
}

#[tokio::test]
#[ignore = "needs Postgres on port 5431 and etcd on 2379"]
async fn the_service_starts_reads_the_cell_and_answers() {
    let schema = "api_binary";
    let (mut db, conn) = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(5431)
        .user("ledger")
        .dbname("postgres")
        .connect(NoTls)
        .await
        .expect("Postgres is on port 5431");
    tokio::spawn(conn);
    db.batch_execute(&format!("drop schema if exists {schema} cascade"))
        .await
        .unwrap();
    migrate(&mut db, schema).await.unwrap();

    let dir = std::env::temp_dir().join(format!("ledger-api-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_file = dir.join("db");
    std::fs::write(
        &db_file,
        "host=127.0.0.1 port=5431 user=ledger dbname=postgres",
    )
    .unwrap();
    let tokens = dir.join("tokens.json");
    std::fs::write(
        &tokens,
        format!(r#"[{{"token":"{TOKEN}","tenant":"ten_acme","principal":"usr_ada"}}]"#),
    )
    .unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let etcd = std::env::var("LEDGER_ETCD").unwrap_or_else(|_| "http://127.0.0.1:2379".into());

    let _service = Running(
        Command::new(env!("CARGO_BIN_EXE_ledger-api"))
            .env("LEDGER_NODE", "cell-a")
            .env("LEDGER_DB_FILE", &db_file)
            .env("LEDGER_DB_TLS", "off")
            .env("LEDGER_SCHEMA", schema)
            .env("LEDGER_LISTEN", &addr)
            .env("LEDGER_TOKENS_FILE", &tokens)
            .env("LEDGER_ETCD_ENDPOINTS", etcd)
            .env("LEDGER_ETCD_PREFIX", format!("/api-binary-test/{nanos}"))
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );

    let started = Instant::now();
    let health = loop {
        if let Some(r) = http(
            &addr,
            "GET /healthz HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n",
        ) {
            break r;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the service did not start"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(health.0, 200, "{}", health.1);
    assert!(health.1.contains(r#""status":"ok""#) && health.1.contains(r#""cell":"cell-a""#));

    // It opens an account through the real control plane and the real cell.
    let body = r#"{"currency":"GBP"}"#;
    let request = format!(
        "POST /v1/accounts HTTP/1.1\r\nhost: x\r\nconnection: close\r\nauthorization: Bearer {TOKEN}\r\nidempotency-key: bin-0001\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    let (status, opened) = http(&addr, &request).unwrap();
    assert_eq!(status, 201, "{opened}");
    assert!(opened.contains(r#""cell":"cell-a""#) && opened.contains(r#""epoch":1"#));
    let (status, health) = http(
        &addr,
        "GET /healthz HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n",
    )
    .unwrap();
    assert!(
        status == 200 && health.contains(r#""accounts_homed":1"#),
        "{health}"
    );

    // Without a token it refuses.
    let (status, refused) = http(
        &addr,
        "GET /v1/accounts/acc_x HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n",
    )
    .unwrap();
    assert!(status == 401 && refused.contains("unauthenticated"));

    db.batch_execute(&format!("drop schema if exists {schema} cascade"))
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn it_refuses_to_start_without_its_configuration() {
    let out = Command::new(env!("CARGO_BIN_EXE_ledger-api"))
        .env_clear()
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("LEDGER_NODE is not set"), "{said}");
}

/// Runs the binary with a complete, valid configuration plus the given extras, and returns what it
/// said. None of these reach the database: the checks come first.
fn refused(extra: &[(&str, &str)]) -> (Option<i32>, String) {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ledger-api-refuse-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let tokens = dir.join("tokens.json");
    std::fs::write(
        &tokens,
        format!(r#"[{{"token":"{TOKEN}","tenant":"ten_acme","principal":"usr_ada"}}]"#),
    )
    .unwrap();
    let db = dir.join("db");
    std::fs::write(&db, "host=127.0.0.1 port=1 user=x dbname=x").unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ledger-api"));
    cmd.env_clear()
        .env("LEDGER_NODE", "cell-a")
        .env("LEDGER_DB_FILE", &db)
        .env("LEDGER_DB_TLS", "off")
        .env("LEDGER_TOKENS_FILE", &tokens)
        .env("LEDGER_ETCD_ENDPOINTS", "http://127.0.0.1:1");
    for (k, v) in extra {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let _ = std::fs::remove_dir_all(dir);
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn it_refuses_a_public_listener_off_loopback_without_tls() {
    let (code, said) = refused(&[("LEDGER_LISTEN", "0.0.0.0:8080")]);
    assert_eq!(code, Some(2));
    assert!(said.contains("not loopback"), "{said}");
}

#[test]
fn it_refuses_an_internal_listener_off_loopback_without_mutual_tls() {
    let (code, said) = refused(&[("LEDGER_INTERNAL_LISTEN", "10.1.2.3:9090")]);
    assert_eq!(code, Some(2));
    assert!(
        said.contains("LEDGER_INTERNAL_LISTEN") && said.contains("not loopback"),
        "{said}"
    );
}

#[test]
fn it_refuses_the_retired_peer_database_setting() {
    let (code, said) = refused(&[("LEDGER_PEERS_FILE", "/nonexistent")]);
    assert_eq!(code, Some(2));
    assert!(said.contains("LEDGER_PEERS_FILE is retired"), "{said}");
}

#[test]
fn it_refuses_an_https_etcd_endpoint_without_client_files() {
    let (code, said) = refused(&[("LEDGER_ETCD_ENDPOINTS", "https://10.0.0.1:2379")]);
    assert_eq!(code, Some(2));
    assert!(
        said.contains("https") && said.contains("LEDGER_ETCD_CA_FILE"),
        "{said}"
    );
}

#[test]
fn it_refuses_some_but_not_all_of_the_etcd_files() {
    let (code, said) = refused(&[
        ("LEDGER_ETCD_ENDPOINTS", "https://10.0.0.1:2379"),
        ("LEDGER_ETCD_CA_FILE", "/nonexistent/ca.crt"),
    ]);
    assert_eq!(code, Some(2));
    assert!(said.contains("all three or none"), "{said}");
}

#[test]
fn it_refuses_etcd_files_with_a_plain_http_endpoint_off_loopback() {
    let (code, said) = refused(&[
        ("LEDGER_ETCD_ENDPOINTS", "http://10.0.0.1:2379"),
        ("LEDGER_ETCD_CA_FILE", "/nonexistent/ca.crt"),
        ("LEDGER_ETCD_CERT_FILE", "/nonexistent/client.crt"),
        ("LEDGER_ETCD_KEY_FILE", "/nonexistent/client.key"),
    ]);
    assert_eq!(code, Some(2));
    assert!(said.contains("plain http"), "{said}");
}

/// One request to a TLS listener, checking its certificate against `ca`.
async fn https(port: u16, ca_pem: &str, request: &str) -> Option<(u16, String)> {
    use rustls::crypto::ring::default_provider;
    use rustls_pki_types::{CertificateDer, ServerName, pem::PemObject};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut roots = rustls::RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(ca_pem.as_bytes()) {
        roots.add(c.ok()?).ok()?;
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(default_provider()))
        .with_safe_default_protocol_versions()
        .ok()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .ok()?;
    let mut tls = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("localhost").ok()?, tcp)
        .await
        .ok()?;
    tls.write_all(request.as_bytes()).await.ok()?;
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), tls.read_to_end(&mut out)).await;
    let out = String::from_utf8_lossy(&out).into_owned();
    let status = out.split_whitespace().nth(1)?.parse().ok()?;
    Some((
        status,
        out.split("\r\n\r\n").nth(1).unwrap_or("").to_owned(),
    ))
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn post_request(path: &str, key: &str, body: &str) -> String {
    format!(
        "POST {path} HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\nauthorization: Bearer {TOKEN}\r\nidempotency-key: {key}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
}

fn field(json: &str, name: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    v[name].as_str().unwrap().to_owned()
}

/// Two real services, two real cells and real etcd, calling each other over mutual TLS.
#[tokio::test]
#[ignore = "needs Postgres on ports 5431 and 5432 and etcd on 2379"]
async fn two_cells_move_value_through_the_real_binaries_over_mutual_tls() {
    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair,
        KeyUsagePurpose,
    };
    let schema = "api_two_cells";
    for port in [5431u16, 5432] {
        let (mut db, conn) = tokio_postgres::Config::new()
            .host("127.0.0.1")
            .port(port)
            .user("ledger")
            .dbname("postgres")
            .connect(NoTls)
            .await
            .expect("Postgres is up");
        tokio::spawn(conn);
        db.batch_execute(&format!("drop schema if exists {schema} cascade"))
            .await
            .unwrap();
        migrate(&mut db, schema).await.unwrap();
    }

    let dir = std::env::temp_dir().join(format!("ledger-api-two-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // One authority, and one certificate that both cells use as server and client.
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(vec![]).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ServerAuth,
        ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let cert = params.signed_by(&key, &ca, &ca_key).unwrap();
    let write = |name: &str, text: String| {
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    };
    let (crt, key_file, ca_file) = (
        write("crt", cert.pem()),
        write("key", key.serialize_pem()),
        write("ca", ca.pem()),
    );
    let tokens = write(
        "tokens.json",
        format!(r#"[{{"token":"{TOKEN}","tenant":"ten_acme","principal":"usr_ada"}}]"#),
    );
    let cell_a = "c".repeat(40);
    let cell_b = "d".repeat(40);
    let auth = write(
        "auth.json",
        format!(
            r#"[{{"token":"{cell_a}","cell":"cell-a","tenants":["ten_acme"]}},{{"token":"{cell_b}","cell":"cell-b","tenants":["ten_acme"]}}]"#
        ),
    );
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let etcd = std::env::var("LEDGER_ETCD").unwrap_or_else(|_| "http://127.0.0.1:2379".into());

    let mut services = Vec::new();
    let mut ports = Vec::new();
    for (name, db_port) in [("cell-a", 5431u16), ("cell-b", 5432)] {
        let (public, internal) = (free_port(), free_port());
        ports.push((public, internal));
        services.push((name, db_port, public, internal));
    }
    let mut running = Vec::new();
    for (i, (name, db_port, public, internal)) in services.iter().enumerate() {
        let other = ports[1 - i].1;
        let (other_name, my_token_for_other) = if i == 0 {
            ("cell-b", &cell_a)
        } else {
            ("cell-a", &cell_b)
        };
        let db_file = write(
            &format!("db-{name}"),
            format!("host=127.0.0.1 port={db_port} user=ledger dbname=postgres"),
        );
        let cells = write(
            &format!("cells-{name}.json"),
            format!(
                r#"{{"{other_name}":{{"url":"https://localhost:{other}","token":"{my_token_for_other}"}}}}"#
            ),
        );
        running.push(Running(
            Command::new(env!("CARGO_BIN_EXE_ledger-api"))
                .env("LEDGER_NODE", name)
                .env("LEDGER_DB_FILE", db_file)
                .env("LEDGER_DB_TLS", "off")
                .env("LEDGER_SCHEMA", schema)
                .env("LEDGER_LISTEN", format!("127.0.0.1:{public}"))
                .env("LEDGER_INTERNAL_LISTEN", format!("127.0.0.1:{internal}"))
                .env("LEDGER_INTERNAL_AUTH_FILE", &auth)
                .env("LEDGER_CELLS_FILE", cells)
                .env("LEDGER_TLS_CERT_FILE", &crt)
                .env("LEDGER_TLS_KEY_FILE", &key_file)
                .env("LEDGER_TLS_CA_FILE", &ca_file)
                .env("LEDGER_TOKENS_FILE", &tokens)
                .env("LEDGER_ETCD_ENDPOINTS", &etcd)
                .env("LEDGER_ETCD_PREFIX", format!("/api-two-cells/{nanos}"))
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        ));
    }
    let ca_pem = ca.pem();
    let port = |i: usize| ports[i].0;
    for i in 0..2 {
        let started = Instant::now();
        loop {
            if https(
                port(i),
                &ca_pem,
                "GET /healthz HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n",
            )
            .await
            .is_some()
            {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "cell {i} did not start"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    // The public listener serves TLS too, and refuses plain HTTP.
    assert!(
        http(
            &format!("127.0.0.1:{}", port(0)),
            "GET /healthz HTTP/1.1\r\nhost: x\r\nconnection: close\r\n\r\n"
        )
        .is_none()
    );

    let (status, payer) = https(
        port(0),
        &ca_pem,
        &post_request(
            "/v1/accounts",
            "p-1",
            r#"{"currency":"GBP","overdraft_limit":{"minor":1000,"currency":"GBP"}}"#,
        ),
    )
    .await
    .unwrap();
    assert_eq!(status, 201, "{payer}");
    let (status, payee) = https(
        port(1),
        &ca_pem,
        &post_request("/v1/accounts", "p-2", r#"{"currency":"GBP"}"#),
    )
    .await
    .unwrap();
    assert_eq!(status, 201, "{payee}");
    let (payer, payee) = (field(&payer, "account_id"), field(&payee, "account_id"));

    let body = format!(
        r#"{{"payer_account_id":"{payer}","payee_account_id":"{payee}","amount":{{"minor":40,"currency":"GBP"}}}}"#
    );
    let (status, done) = https(
        port(0),
        &ca_pem,
        &post_request("/v1/transfers", "t-1", &body),
    )
    .await
    .unwrap();
    assert_eq!(status, 201, "{done}");
    assert!(
        done.contains(r#""replayed":false"#) && done.contains("cell-b"),
        "{done}"
    );
    let (status, again) = https(
        port(0),
        &ca_pem,
        &post_request("/v1/transfers", "t-1", &body),
    )
    .await
    .unwrap();
    assert_eq!(status, 201);
    assert!(again.contains(r#""replayed":true"#), "{again}");

    // The payee's own cell shows the credit.
    let (status, balances) = https(
        port(1),
        &ca_pem,
        &format!("GET /v1/accounts/{payee}/balances HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\nauthorization: Bearer {TOKEN}\r\n\r\n"),
    )
    .await
    .unwrap();
    assert_eq!(status, 200, "{balances}");
    assert!(balances.contains(r#""posted":{"minor":40"#), "{balances}");

    drop(running);
    for port in [5431u16, 5432] {
        let (db, conn) = tokio_postgres::Config::new()
            .host("127.0.0.1")
            .port(port)
            .user("ledger")
            .dbname("postgres")
            .connect(NoTls)
            .await
            .unwrap();
        tokio::spawn(conn);
        db.batch_execute(&format!("drop schema if exists {schema} cascade"))
            .await
            .unwrap();
    }
    let _ = std::fs::remove_dir_all(dir);
}
