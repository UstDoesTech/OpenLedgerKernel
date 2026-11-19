//! R-01: the outbox relay. Two cells are two real Postgres nodes, never a mock.
//!
//! Run with `make up` (or `scripts/local_nodes.sh start`), then
//! `cargo test -p conformance --test replication -- --ignored`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

use ledger_check::{Options, check_cell, compare_chains};
use ledger_core::{AccountId, Currency, EntryId, JournalId, Money, TenantId};
use ledger_repl::{
    Entry, apply, catch_up, cursor, lag, publish_watermark, pull, pull_all, watermark,
};
use ledger_store::{Leg, LegWriter, Side, migrate};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_postgres::{Client, NoTls};

const TENANT: &str = "ten_acme";
const SRC: &str = "aws-lon-01";

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

/// A cell with accounts a, b and c (customer accounts that may overdraw, so any journal posts)
/// and epoch 1 for each.
async fn cell(port: u16, schema: &str) -> Client {
    let mut c = connect(port).await;
    c.batch_execute(&format!("drop schema if exists {schema} cascade"))
        .await
        .unwrap();
    migrate(&mut c, schema).await.unwrap();
    c.batch_execute(&format!(
        "insert into {schema}.account (tenant_id, id, kind, currency, normal_side, allow_negative) values
           ('{TENANT}', 'acc_a', 'customer', 'GBP', 'credit', true),
           ('{TENANT}', 'acc_b', 'customer', 'GBP', 'credit', true),
           ('{TENANT}', 'acc_c', 'customer', 'GBP', 'credit', true);
         insert into {schema}.epoch_fence (tenant_id, account_id, epoch, home) values
           ('{TENANT}', 'acc_a', 1, '{SRC}'), ('{TENANT}', 'acc_b', 1, '{SRC}'), ('{TENANT}', 'acc_c', 1, '{SRC}')"
    ))
    .await
    .unwrap();
    c
}

async fn journal(c: &Client, schema: &str, id: &str, from: &str, to: &str, minor: i64) {
    let w = LegWriter::prepare(c, schema).await.unwrap();
    for (i, (acc, side)) in [(from, Side::Debit), (to, Side::Credit)]
        .into_iter()
        .enumerate()
    {
        w.post(&Leg {
            tenant: TENANT.parse::<TenantId>().unwrap(),
            journal: id.parse::<JournalId>().unwrap(),
            account: acc.parse::<AccountId>().unwrap(),
            leg: i as i16 + 1,
            side,
            amount: Money::new(minor, "GBP".parse::<Currency>().unwrap()),
            counterparty: None,
            origin: SRC.to_owned(),
            epoch: 1,
            entry: format!("ent_{}{i}", &id[3..]).parse::<EntryId>().unwrap(),
        })
        .await
        .unwrap();
    }
}

/// `n` balanced journals rotating over the three accounts.
async fn workload(c: &Client, schema: &str, n: usize) {
    let accounts = ["acc_a", "acc_b", "acc_c"];
    for i in 0..n {
        journal(
            c,
            schema,
            &format!("jr_w{i}"),
            accounts[i % 3],
            accounts[(i + 1) % 3],
            1 + i as i64,
        )
        .await;
    }
}

async fn log(c: &Client, schema: &str) -> Vec<(String, i64, i64, i64)> {
    c.query(
        &format!(
            "select id, epoch, seq, amount from {schema}.entry order by account_id, epoch, seq"
        ),
        &[],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
    .collect()
}

async fn balances(c: &Client, schema: &str) -> Vec<(String, i64)> {
    c.query(
        &format!("select id, balance from {schema}.account order by id"),
        &[],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get(0), r.get(1)))
    .collect()
}

async fn drop_both(schema: &str) {
    for p in [5431, 5432] {
        connect(p)
            .await
            .batch_execute(&format!("drop schema if exists {schema} cascade"))
            .await
            .unwrap();
    }
}

fn entry(id: &str, account: &str, epoch: i64, seq: i64, minor: i64) -> Entry {
    Entry {
        id: id.to_owned(),
        journal: format!("jr_{}", &id[4..]),
        account: account.to_owned(),
        epoch,
        seq,
        leg: 1,
        kind: "credit".to_owned(),
        amount: minor,
        counterparty: None,
        origin: SRC.to_owned(),
        created_at: "2026-11-14T10:00:00Z".to_owned(),
    }
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn entries_ship_in_order_and_redelivery_applies_nothing_twice() {
    let s = "r01_order";
    let (src, dest) = (cell(5431, s).await, cell(5432, s).await);
    workload(&src, s, 30).await;

    let r = pull_all(&src, &dest, s, TENANT, SRC, 7).await.unwrap();
    assert_eq!((r.read, r.applied.applied, r.applied.buffered), (60, 60, 0));
    assert_eq!(
        log(&src, s).await,
        log(&dest, s).await,
        "same entries, same order"
    );
    assert_eq!(balances(&src, s).await, balances(&dest, s).await);
    assert_eq!(cursor(&dest, s, TENANT, SRC).await.unwrap(), r.cursor);
    // The two cells agree by the checker, and the destination is healthy.
    assert_eq!(
        compare_chains(&[("src", &src), ("dest", &dest)], s)
            .await
            .unwrap(),
        []
    );
    assert_eq!(
        check_cell(&dest, s, "dest", &Options::default())
            .await
            .unwrap(),
        []
    );

    // Redelivery: rewind the cursor and pull everything again. Nothing is applied twice.
    let before = (log(&dest, s).await, balances(&dest, s).await);
    dest.batch_execute(&format!("update {s}.outbox_cursor set last_position = 0"))
        .await
        .unwrap();
    let again = pull_all(&src, &dest, s, TENANT, SRC, 11).await.unwrap();
    assert_eq!((again.applied.applied, again.applied.dup), (0, 60));
    assert_eq!((log(&dest, s).await, balances(&dest, s).await), before);
    // Nothing more to pull.
    assert_eq!(pull(&src, &dest, s, TENANT, SRC, 10).await.unwrap().read, 0);
    drop_both(s).await;
}

/// A TCP proxy that counts round trips, as in K-03.
struct CountingProxy {
    port: u16,
    turns: Arc<AtomicUsize>,
}

impl CountingProxy {
    async fn start(target: u16) -> CountingProxy {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let turns = Arc::new(AtomicUsize::new(0));
        let counted = turns.clone();
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let server = TcpStream::connect(("127.0.0.1", target)).await.unwrap();
                let (mut cr, mut cw) = client.into_split();
                let (mut sr, mut sw) = server.into_split();
                let last = Arc::new(AtomicU8::new(0));
                let (turns_up, last_up) = (counted.clone(), last.clone());
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 64 * 1024];
                    while let Ok(n) = cr.read(&mut buf).await {
                        if n == 0 || sw.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                        if last_up.swap(1, Ordering::SeqCst) != 1 {
                            turns_up.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                });
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 64 * 1024];
                    while let Ok(n) = sr.read(&mut buf).await {
                        if n == 0 || cw.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                        last.store(2, Ordering::SeqCst);
                    }
                });
            }
        });
        CountingProxy { port, turns }
    }
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn a_batch_costs_the_same_round_trips_whatever_its_size() {
    let s = "r01_trips";
    let src = cell(5431, s).await;
    let dest_direct = cell(5432, s).await;
    workload(&src, s, 30).await;
    let entries: Vec<Entry> = {
        let rows = src
            .query(
                &format!("select id, journal_id, account_id, epoch, seq, leg, kind, amount, counterparty, origin, created_at::text from {s}.entry order by account_id, epoch, seq"),
                &[],
            )
            .await
            .unwrap();
        rows.iter()
            .map(|r| Entry {
                id: r.get(0),
                journal: r.get(1),
                account: r.get(2),
                epoch: r.get(3),
                seq: r.get(4),
                leg: r.get(5),
                kind: r.get(6),
                amount: r.get(7),
                counterparty: r.get(8),
                origin: r.get(9),
                created_at: r.get(10),
            })
            .collect()
    };
    assert_eq!(entries.len(), 60);

    let proxy = CountingProxy::start(5432).await;
    let dest = connect(proxy.port).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut trips = Vec::new();
    for chunk in [&entries[..5], &entries[5..60]] {
        let before = proxy.turns.load(Ordering::SeqCst);
        let r = apply(&dest, s, TENANT, None, chunk).await.unwrap();
        assert_eq!(r.applied + r.drained, chunk.len());
        trips.push(proxy.turns.load(Ordering::SeqCst) - before);
    }
    assert_eq!(
        trips[0], trips[1],
        "5 entries and 55 entries cost the same: {trips:?}"
    );
    assert!(
        trips[0] <= 2,
        "a batch is at most a prepare and an execute: {trips:?}"
    );
    assert_eq!(balances(&src, s).await, balances(&dest_direct, s).await);
    drop_both(s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn a_gap_is_buffered_and_never_skipped() {
    let s = "r01_gap";
    let dest = cell(5432, s).await;
    let a = |n: i64| entry(&format!("ent_a{n}"), "acc_a", 1, n, 10 * n);

    // Seq 3 and 4 arrive before 1 and 2: held, not applied and not dropped.
    let r = apply(&dest, s, TENANT, None, &[a(3), a(4)]).await.unwrap();
    assert_eq!((r.applied, r.buffered), (0, 2));
    assert_eq!(log(&dest, s).await, []);
    let held: i64 = dest
        .query_one(&format!("select count(*) from {s}.repl_buffer"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(held, 2);

    // Seq 2 alone does not close the gap (1 is missing): all three are held.
    let r = apply(&dest, s, TENANT, None, &[a(2)]).await.unwrap();
    assert_eq!((r.applied, r.buffered, r.drained), (0, 1, 0));
    assert_eq!(log(&dest, s).await, []);

    // Seq 1 arrives: it applies, and the buffer drains in order behind it.
    let r = apply(&dest, s, TENANT, None, &[a(1)]).await.unwrap();
    assert_eq!((r.applied, r.drained), (1, 3));
    let seqs: Vec<i64> = log(&dest, s).await.iter().map(|e| e.2).collect();
    assert_eq!(seqs, [1, 2, 3, 4]);
    assert_eq!(
        balances(&dest, s).await[0],
        ("acc_a".to_owned(), 10 + 20 + 30 + 40)
    );
    let held: i64 = dest
        .query_one(&format!("select count(*) from {s}.repl_buffer"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(held, 0);
    // A repeat of any of them applies nothing.
    let r = apply(&dest, s, TENANT, None, &[a(2), a(4)]).await.unwrap();
    assert_eq!((r.applied, r.dup), (0, 2));
    drop_both(s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn a_gap_hidden_behind_a_passed_position_is_re_fetched() {
    let s = "r01_refetch";
    let (src, dest) = (cell(5431, s).await, cell(5432, s).await);

    // A transaction on the source takes an outbox position for acc_c, then stays open while a
    // later one for acc_a commits: the cursor will pass the first position before it is visible.
    let held = connect(5431).await;
    held.batch_execute("begin").await.unwrap();
    held.query(
        &format!("select * from {s}.post_leg('{TENANT}','jr_held','acc_c',1::smallint,'credit',5,'GBP',null,'{SRC}',1,'ent_held')"),
        &[],
    )
    .await
    .unwrap();
    journal(&src, s, "jr_later", "acc_a", "acc_b", 9).await;

    let r = pull_all(&src, &dest, s, TENANT, SRC, 10).await.unwrap();
    assert_eq!(
        r.applied.applied, 2,
        "only the committed journal is visible"
    );
    held.batch_execute("commit").await.unwrap();
    journal(&src, s, "jr_after", "acc_c", "acc_b", 3).await; // acc_c now has seq 1 and 2

    // The cursor is past the held position, so a pull ships only the newest entries. Seq 2 of
    // acc_c arrives before seq 1: a gap, buffered.
    let r = pull_all(&src, &dest, s, TENANT, SRC, 10).await.unwrap();
    assert!(r.applied.buffered >= 1, "{r:?}");
    assert!(log(&dest, s).await.iter().all(|e| e.0 != "ent_held"));

    // Catch-up fetches the missing entry by account, and the buffer drains behind it.
    let c = catch_up(&src, &dest, s, TENANT, 10).await.unwrap();
    assert!(c.accounts >= 1);
    assert_eq!(log(&src, s).await, log(&dest, s).await);
    assert_eq!(balances(&src, s).await, balances(&dest, s).await);
    let held_n: i64 = dest
        .query_one(&format!("select count(*) from {s}.repl_buffer"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(held_n, 0);
    assert_eq!(
        compare_chains(&[("src", &src), ("dest", &dest)], s)
            .await
            .unwrap(),
        []
    );
    drop_both(s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn an_idle_cell_still_publishes_a_watermark_and_lag_is_honest() {
    let s = "r01_watermark";
    let (src, dest) = (cell(5431, s).await, cell(5432, s).await);
    assert_eq!(
        lag(&src, &dest, s, TENANT, SRC).await.unwrap(),
        None,
        "nothing published yet"
    );

    // Idle: nothing written, and the watermark is still published, at zero.
    assert_eq!(publish_watermark(&src, s, TENANT, SRC).await.unwrap(), 0);
    assert_eq!(lag(&src, &dest, s, TENANT, SRC).await.unwrap(), Some(0));
    tokio::time::sleep(Duration::from_millis(2_200)).await;
    let (_, age) = watermark(&src, s, TENANT, SRC).await.unwrap().unwrap();
    assert!(age >= 2, "a stale watermark shows its age: {age}");
    publish_watermark(&src, s, TENANT, SRC).await.unwrap();
    let (pos, age) = watermark(&src, s, TENANT, SRC).await.unwrap().unwrap();
    assert_eq!(
        (pos, age <= 1),
        (0, true),
        "publishing while idle refreshes it"
    );

    // Writes move it, and the destination's lag is what it has not read.
    workload(&src, s, 3).await;
    assert_eq!(publish_watermark(&src, s, TENANT, SRC).await.unwrap(), 6);
    assert_eq!(lag(&src, &dest, s, TENANT, SRC).await.unwrap(), Some(6));
    pull_all(&src, &dest, s, TENANT, SRC, 4).await.unwrap();
    assert_eq!(lag(&src, &dest, s, TENANT, SRC).await.unwrap(), Some(0));
    // The published position never moves back.
    assert_eq!(publish_watermark(&src, s, TENANT, SRC).await.unwrap(), 6);
    drop_both(s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn superseded_epochs_are_quarantined_and_unknown_epochs_wait_for_their_fence() {
    let s = "r01_epochs";
    let dest = cell(5432, s).await;
    // acc_a moved to epoch 2 on this cell, which kept entries of epoch 1 only up to seq 2.
    dest.batch_execute(&format!(
        "insert into {s}.epoch_fence (tenant_id, account_id, epoch, home, prev_epoch, prev_cutoff_seq)
           values ('{TENANT}', 'acc_a', 2, 'gcp-car-01', 1, 2)"
    ))
    .await
    .unwrap();
    let r = apply(
        &dest,
        s,
        TENANT,
        None,
        &[
            entry("ent_a1", "acc_a", 1, 1, 10),
            entry("ent_a2", "acc_a", 1, 2, 20),
            entry("ent_a3", "acc_a", 1, 3, 30), // past the cutoff: void
            entry("ent_b9", "acc_b", 3, 1, 5),  // epoch 3 is not known here
        ],
    )
    .await
    .unwrap();
    assert_eq!((r.applied, r.quarantined, r.buffered), (2, 1, 1));
    let q: Vec<(String, String)> = dest
        .query(&format!("select id, reason from {s}.quarantine"), &[])
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    assert_eq!(q, [("ent_a3".to_owned(), "void_on_arrival".to_owned())]);
    // The unknown epoch waits. When its fence arrives, it applies from the buffer.
    dest.batch_execute(&format!(
        "insert into {s}.epoch_fence (tenant_id, account_id, epoch, home, prev_epoch, prev_cutoff_seq)
           values ('{TENANT}', 'acc_b', 3, 'gcp-car-01', 1, 0)"
    ))
    .await
    .unwrap();
    // An unrelated empty batch drains the buffer.
    let r = apply(&dest, s, TENANT, None, &[]).await.unwrap();
    assert_eq!(r.drained, 1);
    // An unknown account is an error that rolls the batch back.
    let err = apply(
        &dest,
        s,
        TENANT,
        None,
        &[entry("ent_z1", "acc_zzz", 1, 1, 1)],
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("db error"), "{err}");
    drop_both(s).await;
}
