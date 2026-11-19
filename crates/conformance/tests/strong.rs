//! R-03: the strong durability tier. A strong write returns only after a remote cell has applied
//! it; with no acknowledgement in time the caller gets `durability_not_met`, and a retry with the
//! same key completes it without double-posting. Cells are real Postgres nodes.
//!
//! Run with `make up` (or `scripts/local_nodes.sh start`), then
//! `cargo test -p conformance --test strong -- --ignored`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ledger_core::{AccountId, Currency, EntryId, JournalId, Money, TenantId};
use ledger_repl::strong::{StrongError, acknowledge};
use ledger_store::{Leg, LegWriter, Side, migrate};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_postgres::{Client, NoTls};

const TENANT: &str = "ten_acme";
const HOME: &str = "aws-lon-01";

async fn connect(port: u16) -> Client {
    let (client, conn) = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(port)
        .user("ledger")
        .dbname("postgres")
        .connect_timeout(Duration::from_secs(5))
        .connect(NoTls)
        .await
        .unwrap();
    tokio::spawn(conn);
    client
}

async fn cell(port: u16, schema: &str) -> Client {
    let mut c = connect(port).await;
    c.batch_execute(&format!("drop schema if exists {schema} cascade"))
        .await
        .unwrap();
    migrate(&mut c, schema).await.unwrap();
    c.batch_execute(&format!(
        "insert into {schema}.account (tenant_id, id, kind, currency, normal_side, allow_negative) values
           ('{TENANT}', 'acc_a', 'customer', 'GBP', 'credit', true),
           ('{TENANT}', 'acc_b', 'customer', 'GBP', 'credit', true);
         insert into {schema}.epoch_fence (tenant_id, account_id, epoch, home) values
           ('{TENANT}', 'acc_a', 1, '{HOME}'), ('{TENANT}', 'acc_b', 1, '{HOME}')"
    ))
    .await
    .unwrap();
    c
}

/// The write: a transfer of `minor` from a to b, keyed by the journal id. Posting it again with
/// the same key replays what was written. Returns the entry ids and whether it was a replay.
async fn post(c: &Client, schema: &str, key: &str, minor: i64) -> (Vec<String>, bool) {
    let w = LegWriter::prepare(c, schema).await.unwrap();
    let mut ids = Vec::new();
    let mut replayed = true;
    for (i, (acc, side)) in [("acc_a", Side::Debit), ("acc_b", Side::Credit)]
        .into_iter()
        .enumerate()
    {
        let p = w
            .post(&Leg {
                tenant: TENANT.parse::<TenantId>().unwrap(),
                journal: format!("jr_{key}").parse::<JournalId>().unwrap(),
                account: acc.parse::<AccountId>().unwrap(),
                leg: i as i16 + 1,
                side,
                amount: Money::new(minor, "GBP".parse::<Currency>().unwrap()),
                counterparty: None,
                origin: HOME.to_owned(),
                epoch: 1,
                entry: format!("ent_{key}x{i}").parse::<EntryId>().unwrap(),
            })
            .await
            .unwrap();
        replayed &= p.replayed;
        ids.push(p.entry.as_str().to_owned());
    }
    (ids, replayed)
}

async fn count(c: &Client, schema: &str) -> i64 {
    c.query_one(&format!("select count(*) from {schema}.entry"), &[])
        .await
        .unwrap()
        .get(0)
}

async fn balance(c: &Client, schema: &str, id: &str) -> i64 {
    c.query_one(
        &format!("select balance from {schema}.account where id = $1"),
        &[&id],
    )
    .await
    .unwrap()
    .get(0)
}

/// A TCP proxy whose traffic can be stalled, as a cut link to the remote cell is.
struct Link {
    port: u16,
    stalled: Arc<AtomicBool>,
}

impl Link {
    async fn start(target: u16) -> Link {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let stalled = Arc::new(AtomicBool::new(false));
        let flag = stalled.clone();
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let server = TcpStream::connect(("127.0.0.1", target)).await.unwrap();
                let (mut cr, mut cw) = client.into_split();
                let (mut sr, mut sw) = server.into_split();
                let (f1, f2) = (flag.clone(), flag.clone());
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 64 * 1024];
                    while let Ok(n) = cr.read(&mut buf).await {
                        while f1.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        if n == 0 || sw.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 64 * 1024];
                    while let Ok(n) = sr.read(&mut buf).await {
                        while f2.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        if n == 0 || cw.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        Link { port, stalled }
    }
}

async fn drop_all(schema: &str) {
    for p in [5431, 5432] {
        connect(p)
            .await
            .batch_execute(&format!("drop schema if exists {schema} cascade"))
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn a_strong_write_returns_only_after_the_remote_cell_holds_it() {
    let s = "r03_ack";
    let (home, remote) = (cell(5431, s).await, cell(5432, s).await);
    let (ids, replayed) = post(&home, s, "pay1", 500).await;
    assert!(!replayed);
    // Committed at the home, and the remote knows nothing yet.
    assert_eq!(count(&remote, s).await, 0);

    let ack = acknowledge(
        &home,
        &remote,
        s,
        TENANT,
        HOME,
        &ids,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert!(ack.pulls >= 1);
    // On return, the remote holds exactly the write, with the same balances.
    assert_eq!(count(&remote, s).await, 2);
    assert_eq!(balance(&remote, s, "acc_a").await, -500);
    assert_eq!(balance(&remote, s, "acc_b").await, 500);
    // Acknowledging again is free: the remote already holds it.
    let again = acknowledge(
        &home,
        &remote,
        s,
        TENANT,
        HOME,
        &ids,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(again.pulls, 0);
    drop_all(s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn without_an_acknowledgement_the_caller_gets_durability_not_met_and_a_retry_completes_it() {
    let s = "r03_retry";
    let (home, remote_direct) = (cell(5431, s).await, cell(5432, s).await);
    let link = Link::start(5432).await;
    let remote = connect(link.port).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The link to the remote cell stalls. The write still commits at the home.
    link.stalled.store(true, Ordering::SeqCst);
    let (ids, _) = post(&home, s, "pay2", 700).await;
    let started = std::time::Instant::now();
    let err = acknowledge(
        &home,
        &remote,
        s,
        TENANT,
        HOME,
        &ids,
        Duration::from_millis(600),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, StrongError::DurabilityNotMet { .. }), "{err}");
    assert_eq!(err.code(), "durability_not_met");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "it gives up in time: {:?}",
        started.elapsed()
    );
    // Committed at home, not at the remote.
    assert_eq!(count(&home, s).await, 2);
    assert_eq!(count(&remote_direct, s).await, 0);

    // The link returns. The caller retries with the same key, on a fresh connection.
    link.stalled.store(false, Ordering::SeqCst);
    drop(remote);
    let remote = connect(link.port).await;
    let (again_ids, replayed) = post(&home, s, "pay2", 700).await;
    assert!(
        replayed,
        "the retry replays what was written and posts nothing new"
    );
    assert_eq!(again_ids, ids);
    acknowledge(
        &home,
        &remote,
        s,
        TENANT,
        HOME,
        &again_ids,
        Duration::from_secs(5),
    )
    .await
    .unwrap();

    // Never double-posted, on either cell.
    for c in [&home, &remote_direct] {
        assert_eq!(count(c, s).await, 2);
        assert_eq!(balance(c, s, "acc_a").await, -700);
        assert_eq!(balance(c, s, "acc_b").await, 700);
    }
    drop_all(s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn a_remote_that_quarantines_an_entry_has_not_made_it_durable() {
    let s = "r03_void";
    let (home, remote) = (cell(5431, s).await, cell(5432, s).await);
    // The remote already runs epoch 2 for acc_a and cut epoch 1 at seq 0: the home's write is void there.
    remote
        .batch_execute(&format!(
            "insert into {s}.epoch_fence (tenant_id, account_id, epoch, home, prev_epoch, prev_cutoff_seq)
             values ('{TENANT}', 'acc_a', 2, 'gcp-car-01', 1, 0)"
        ))
        .await
        .unwrap();
    let (ids, _) = post(&home, s, "pay3", 50).await;
    let err = acknowledge(
        &home,
        &remote,
        s,
        TENANT,
        HOME,
        &ids,
        Duration::from_millis(500),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, StrongError::DurabilityNotMet { .. }), "{err}");
    let quarantined: i64 = remote
        .query_one(&format!("select count(*) from {s}.quarantine"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        quarantined, 1,
        "the void entry sits in quarantine, which is not durability"
    );
    drop_all(s).await;
}
