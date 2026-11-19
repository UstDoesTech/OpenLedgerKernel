//! R-03 against two real Neon cells: a strong write at the home (`LEDGER_DSN_USE1`) is
//! acknowledged by a remote cell in another region (`LEDGER_DSN_USE2`). It runs only through the
//! `neon-rust` workflow, uses a schema of its own, `strong_neon`, and drops it afterwards. Without
//! both connection strings it does nothing. Each check prints a PASS line, the persisted proof.

use std::time::{Duration, Instant};

use ledger_core::{AccountId, Currency, EntryId, JournalId, Money, TenantId};
use ledger_repl::strong::{StrongError, acknowledge};
use ledger_store::{Leg, LegWriter, Side, migrate};
use native_tls::TlsConnector;
use postgres_native_tls::MakeTlsConnector;
use tokio_postgres::Client;

const S: &str = "strong_neon";
const TENANT: &str = "ten_acme";
const HOME: &str = "neon-use1";

async fn connect(dsn: &str) -> Client {
    let tls = MakeTlsConnector::new(TlsConnector::new().unwrap());
    let (client, conn) =
        tokio::time::timeout(Duration::from_secs(30), tokio_postgres::connect(dsn, tls))
            .await
            .expect("timed out connecting to Neon")
            .expect("could not connect to Neon");
    tokio::spawn(conn);
    client
}

async fn cell(dsn: &str) -> Client {
    let mut c = connect(dsn).await;
    c.batch_execute(&format!("drop schema if exists {S} cascade"))
        .await
        .unwrap();
    migrate(&mut c, S).await.unwrap();
    c.batch_execute(&format!(
        "insert into {S}.account (tenant_id, id, kind, currency, normal_side, allow_negative) values
           ('{TENANT}', 'acc_a', 'customer', 'GBP', 'credit', true),
           ('{TENANT}', 'acc_b', 'customer', 'GBP', 'credit', true);
         insert into {S}.epoch_fence (tenant_id, account_id, epoch, home) values
           ('{TENANT}', 'acc_a', 1, '{HOME}'), ('{TENANT}', 'acc_b', 1, '{HOME}')"
    ))
    .await
    .unwrap();
    c
}

async fn post(c: &Client, key: &str, minor: i64) -> (Vec<String>, bool) {
    let w = LegWriter::prepare(c, S).await.unwrap();
    let (mut ids, mut replayed) = (Vec::new(), true);
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

async fn count(c: &Client) -> i64 {
    c.query_one(&format!("select count(*) from {S}.entry"), &[])
        .await
        .unwrap()
        .get(0)
}

async fn balance(c: &Client, id: &str) -> i64 {
    c.query_one(
        &format!("select balance from {S}.account where id = $1"),
        &[&id],
    )
    .await
    .unwrap()
    .get(0)
}

#[tokio::test]
#[ignore = "runs through the neon-rust workflow, which holds the connection strings"]
async fn a_strong_write_is_acknowledged_across_two_neon_regions() {
    let (Ok(d1), Ok(d2)) = (
        std::env::var("LEDGER_DSN_USE1"),
        std::env::var("LEDGER_DSN_USE2"),
    ) else {
        eprintln!("LEDGER_DSN_USE1 and LEDGER_DSN_USE2 are not both set: nothing to do");
        return;
    };
    let (home, remote) = (cell(&d1).await, cell(&d2).await);

    let (ids, replayed) = post(&home, "pay1", 500).await;
    assert!(!replayed);
    assert_eq!(count(&remote).await, 0);
    let t = Instant::now();
    let ack = acknowledge(
        &home,
        &remote,
        S,
        TENANT,
        HOME,
        &ids,
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    println!(
        "acknowledged in {:?} after {} pull(s)",
        t.elapsed(),
        ack.pulls
    );
    assert_eq!(
        (
            count(&remote).await,
            balance(&remote, "acc_a").await,
            balance(&remote, "acc_b").await
        ),
        (2, -500, 500)
    );
    println!(
        "PASS acknowledged: the write returned only once the remote region held both entries, with equal balances"
    );

    // No acknowledgement in time: a zero timeout cannot be met over a real network.
    let (ids, _) = post(&home, "pay2", 700).await;
    let err = acknowledge(&home, &remote, S, TENANT, HOME, &ids, Duration::ZERO)
        .await
        .unwrap_err();
    assert!(matches!(err, StrongError::DurabilityNotMet { .. }), "{err}");
    assert_eq!(err.code(), "durability_not_met");
    assert_eq!(count(&home).await, 4, "committed at the home");
    println!(
        "PASS not_met: with no acknowledgement in time the caller got durability_not_met, and the write stayed committed at home"
    );

    // The retry with the same key replays, posts nothing new, and completes the acknowledgement.
    let (again, replayed) = post(&home, "pay2", 700).await;
    assert!(replayed && again == ids);
    acknowledge(
        &home,
        &remote,
        S,
        TENANT,
        HOME,
        &again,
        Duration::from_secs(30),
    )
    .await
    .unwrap();
    for c in [&home, &remote] {
        assert_eq!(
            (
                count(c).await,
                balance(c, "acc_a").await,
                balance(c, "acc_b").await
            ),
            (4, -1200, 1200)
        );
    }
    println!(
        "PASS retry: the same key replayed, posted nothing new, and completed the acknowledgement; both regions agree"
    );

    for c in [&home, &remote] {
        c.batch_execute(&format!("drop schema if exists {S} cascade"))
            .await
            .unwrap();
    }
    println!("PASS cleanup: schema dropped on both cells");
}
