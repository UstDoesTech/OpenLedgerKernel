//! The local environment: three Postgres nodes reachable directly and through Toxiproxy,
//! a healthy etcd, and a proxy that can inject faults.
//!
//! Run with `make up` first, then `cargo test -p conformance -- --ignored`.

use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, MutexGuard};
use tokio_postgres::NoTls;

const DIRECT: [u16; 3] = [5431, 5432, 5433];
const PROXIED: [u16; 3] = [15431, 15432, 15433];
const TOXIPROXY: &str = "127.0.0.1:8474";
const ETCD: &str = "127.0.0.1:2379";

/// Toxiproxy holds one set of faults for everyone, so tests that use it take turns. Without
/// this, one test's reset removes another test's fault part-way through.
static ENVIRONMENT: Mutex<()> = Mutex::const_new(());

async fn exclusive() -> MutexGuard<'static, ()> {
    let guard = ENVIRONMENT.lock().await;
    reset_faults().await;
    guard
}

/// One HTTP request over a plain socket, so the tests need no HTTP client. Returns the
/// status code and body.
async fn http(addr: &str, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(addr).await.expect("connect");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.expect("write");
    let mut raw = String::new();
    s.read_to_string(&mut raw).await.expect("read");
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .expect("status line");
    let body = raw
        .split_once("\r\n\r\n")
        .map(|x| x.1)
        .unwrap_or("")
        .to_owned();
    (status, body)
}

async fn reset_faults() {
    let (status, _) = http(TOXIPROXY, "POST", "/reset", "").await;
    assert_eq!(status, 204, "toxiproxy reset");
}

/// Connect, run `select 1`, and return how long the whole thing took.
async fn query_one(port: u16) -> Result<Duration, tokio_postgres::Error> {
    let started = Instant::now();
    let (client, conn) = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(port)
        .user("ledger")
        .dbname("postgres")
        .connect_timeout(Duration::from_secs(5))
        .connect(NoTls)
        .await?;
    tokio::spawn(conn);
    let row = client.query_one("select 1::int", &[]).await?;
    assert_eq!(row.get::<_, i32>(0), 1);
    Ok(started.elapsed())
}

#[tokio::test]
#[ignore = "needs `make up`"]
async fn every_node_answers_directly_and_through_toxiproxy() {
    let _env = exclusive().await;
    for port in DIRECT.into_iter().chain(PROXIED) {
        query_one(port)
            .await
            .unwrap_or_else(|e| panic!("port {port}: {e}"));
    }
}

#[tokio::test]
#[ignore = "needs `make up`"]
async fn proxies_lead_to_the_matching_node() {
    let _env = exclusive().await;
    for (direct, proxied) in DIRECT.into_iter().zip(PROXIED) {
        let name = |port| async move {
            let (client, conn) = tokio_postgres::Config::new()
                .host("127.0.0.1")
                .port(port)
                .user("ledger")
                .dbname("postgres")
                .connect(NoTls)
                .await
                .expect("connect");
            tokio::spawn(conn);
            // The container host name differs per node.
            client
                .query_one("select inet_server_addr()::text", &[])
                .await
                .expect("query")
                .get::<_, String>(0)
        };
        assert_eq!(
            name(direct).await,
            name(proxied).await,
            "port {proxied} is not the proxy for {direct}"
        );
    }
}

#[tokio::test]
#[ignore = "needs `make up`"]
async fn etcd_is_healthy() {
    let _env = exclusive().await;
    let (status, body) = http(ETCD, "GET", "/health", "").await;
    assert_eq!(status, 200);
    assert!(body.contains("\"health\":\"true\""), "{body}");
}

#[tokio::test]
#[ignore = "needs `make up`"]
async fn latency_added_through_the_proxy_does_not_touch_the_direct_path() {
    let _env = exclusive().await;
    let baseline = query_one(PROXIED[0]).await.expect("baseline");

    let toxic =
        r#"{"name":"slow","type":"latency","stream":"downstream","attributes":{"latency":300}}"#;
    let (status, body) = http(TOXIPROXY, "POST", "/proxies/pg1/toxics", toxic).await;
    assert_eq!(status, 200, "{body}");

    let slow = query_one(PROXIED[0]).await.expect("slow path");
    let direct = query_one(DIRECT[0]).await.expect("direct path");
    reset_faults().await;

    assert!(
        slow >= Duration::from_millis(300),
        "slow path took {slow:?}"
    );
    assert!(
        slow > baseline + Duration::from_millis(200),
        "baseline {baseline:?}, slow {slow:?}"
    );
    assert!(
        direct < Duration::from_millis(300),
        "direct path took {direct:?}"
    );
}

#[tokio::test]
#[ignore = "needs `make up`"]
async fn a_cut_link_refuses_connections_through_the_proxy_only() {
    let _env = exclusive().await;
    let (status, _) = http(TOXIPROXY, "POST", "/proxies/pg2", r#"{"enabled":false}"#).await;
    assert_eq!(status, 200);

    let cut = query_one(PROXIED[1]).await;
    let direct = query_one(DIRECT[1]).await;

    reset_faults().await;
    assert!(cut.is_err(), "the cut link still answered");
    assert!(direct.is_ok(), "the node itself should be unaffected");

    // The link comes back after a reset. The listener restarts a moment later.
    let mut ok = false;
    for _ in 0..20 {
        if query_one(PROXIED[1]).await.is_ok() {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(ok, "link did not come back after reset");
}
