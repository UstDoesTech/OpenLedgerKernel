//! K-11: the service starts next to a cell, reads the cell's database and etcd, and answers.
//! This runs the real `ledger-api` binary. Needs Postgres on port 5431 and etcd on 2379
//! (`make up`): `cargo test -p api --test binary -- --ignored`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
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
