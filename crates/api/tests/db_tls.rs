//! K-19a: the service trusts the authority that made its cell's database certificate, and says
//! why when it cannot connect. This starts a real Postgres node with TLS and password sign-in,
//! with a certificate from a throwaway authority, and connects to it the way the service does.
//!
//! Needs the Postgres server binaries (`apt install postgresql-16`, or `PG_BIN`):
//! `cargo test -p api --test db_tls -- --ignored`.

use std::path::PathBuf;
use std::process::Command;

use ledger_api::dbconn::{Cause, connect};
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair, KeyUsagePurpose};

const PASSWORD: &str = "pw-for-this-test-only-9f3a1c";

struct Authority {
    cert: rcgen::Certificate,
    key: KeyPair,
}

fn authority(name: &str) -> Authority {
    let mut params = CertificateParams::new(Vec::new()).unwrap();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, name);
    let key = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    Authority { cert, key }
}

fn bin(name: &str) -> PathBuf {
    let dir = std::env::var("PG_BIN").ok().or_else(|| {
        let mut dirs: Vec<_> = std::fs::read_dir("/usr/lib/postgresql")
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path().join("bin")))
            .collect();
        dirs.sort();
        dirs.pop().map(|p| p.display().to_string())
    });
    PathBuf::from(dir.expect("no Postgres binaries: set PG_BIN")).join(name)
}

fn is_root() -> bool {
    String::from_utf8(Command::new("id").arg("-u").output().unwrap().stdout)
        .unwrap()
        .trim()
        == "0"
}

/// Postgres refuses to run as root, so as root the commands run as the `postgres` user.
fn pg(args: &[String]) -> std::process::Output {
    let line = args.join(" ");
    let out = if is_root() {
        Command::new("su").args(["postgres", "-c", &line]).output()
    } else {
        Command::new("bash").args(["-c", &line]).output()
    };
    out.unwrap()
}

struct Node {
    dir: PathBuf,
    port: u16,
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = pg(&[
            bin("pg_ctl").display().to_string(),
            "-D".into(),
            self.dir.join("data").display().to_string(),
            "-m".into(),
            "immediate".into(),
            "stop".into(),
        ]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start(ca: &Authority) -> Node {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let dir = std::env::temp_dir().join(format!("ledger-dbtls-{}-{port}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let data = dir.join("data");

    // The server's certificate names `localhost` and nothing else.
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "localhost");
    let cert = params.signed_by(&key, &ca.cert, &ca.key).unwrap();
    std::fs::write(dir.join("server.crt"), cert.pem()).unwrap();
    std::fs::write(dir.join("server.key"), key.serialize_pem()).unwrap();
    std::fs::write(dir.join("pw"), PASSWORD).unwrap();
    std::fs::write(
        dir.join("hba.conf"),
        "hostssl all all 127.0.0.1/32 scram-sha-256\n",
    )
    .unwrap();
    if is_root() {
        let _ = Command::new("chown")
            .args(["-R", "postgres"])
            .arg(&dir)
            .status();
    }
    let _ = Command::new("chmod")
        .arg("600")
        .arg(dir.join("server.key"))
        .status();

    let init = pg(&[
        bin("initdb").display().to_string(),
        "-D".into(),
        data.display().to_string(),
        "-A".into(),
        "scram-sha-256".into(),
        "-U".into(),
        "ledger_admin".into(),
        format!("--pwfile={}", dir.join("pw").display()),
    ]);
    assert!(
        init.status.success(),
        "initdb failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    std::fs::copy(dir.join("hba.conf"), data.join("pg_hba.conf")).unwrap();
    if is_root() {
        let _ = Command::new("chown")
            .arg("postgres")
            .arg(data.join("pg_hba.conf"))
            .status();
    }

    let options = format!(
        "-p {port} -k {} -c listen_addresses=127.0.0.1 -c fsync=off -c ssl=on -c ssl_cert_file={} -c ssl_key_file={}",
        dir.display(),
        dir.join("server.crt").display(),
        dir.join("server.key").display()
    );
    let started = pg(&[
        bin("pg_ctl").display().to_string(),
        "-D".into(),
        data.display().to_string(),
        "-o".into(),
        format!("'{options}'"),
        "-l".into(),
        dir.join("log").display().to_string(),
        "-w".into(),
        "start".into(),
    ]);
    assert!(
        started.status.success(),
        "the test's Postgres did not start"
    );
    Node { dir, port }
}

fn conn(host: &str, port: u16, password: &str) -> String {
    format!(
        "host={host} port={port} user=ledger_admin password={password} dbname=postgres sslmode=require"
    )
}

fn said(cause: Cause) -> String {
    format!("cannot connect to the cell's database: {}", cause.text())
}

#[tokio::test]
#[ignore = "needs the Postgres server binaries"]
async fn the_service_trusts_the_authority_it_is_given_and_says_why_it_cannot() {
    let right = authority("The cell's authority");
    let other = authority("Some other authority");
    let node = start(&right);
    let right_pem = right.cert.pem();
    let other_pem = other.cert.pem();

    // The right authority: it connects, and the connection works.
    let client = connect(
        &conn("localhost", node.port, PASSWORD),
        true,
        Some(&right_pem),
    )
    .await
    .expect("the right authority must connect");
    let row = client.query_one("select 1::int", &[]).await.unwrap();
    assert_eq!(row.get::<_, i32>(0), 1);

    let mut messages = Vec::new();

    // No authority, and another authority: the certificate is not trusted.
    for ca in [None, Some(other_pem.as_str())] {
        let err = connect(&conn("localhost", node.port, PASSWORD), true, ca)
            .await
            .expect_err("must not connect");
        assert_eq!(err, said(Cause::Certificate));
        messages.push(err);
    }

    // A name the certificate does not carry.
    let err = connect(
        &conn("127.0.0.1", node.port, PASSWORD),
        true,
        Some(&right_pem),
    )
    .await
    .expect_err("must not connect");
    assert_eq!(err, said(Cause::Name));
    messages.push(err);

    // A wrong password.
    let err = connect(
        &conn("localhost", node.port, "wrong-password-xyz"),
        true,
        Some(&right_pem),
    )
    .await
    .expect_err("must not connect");
    assert_eq!(err, said(Cause::SignIn));
    messages.push(err);

    // Nothing listening.
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let err = connect(&conn("localhost", closed, PASSWORD), true, Some(&right_pem))
        .await
        .expect_err("must not connect");
    assert_eq!(err, said(Cause::Unreachable));
    messages.push(err);

    // None of those messages carried the password, the user or the host.
    for m in &messages {
        for secret in [
            PASSWORD,
            "wrong-password-xyz",
            "ledger_admin",
            "localhost",
            "127.0.0.1",
        ] {
            assert!(
                !m.contains(secret),
                "a message carried something it must not"
            );
        }
    }
}

#[tokio::test]
async fn a_file_that_is_not_a_certificate_is_refused_before_any_connection() {
    let err = connect(
        &conn("localhost", 1, PASSWORD),
        true,
        Some("not a certificate"),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(err, "db-ca.crt: not a certificate");
}
