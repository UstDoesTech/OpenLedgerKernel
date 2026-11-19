//! R-01 against two real Neon cells: the outbox relay pulls from the cell in `LEDGER_DSN_USE1` and
//! applies at the cell in `LEDGER_DSN_USE2`, across regions. It runs only through the `neon-rust`
//! workflow, uses a schema of its own, `repl_neon`, and drops it afterwards. Without both
//! connection strings it does nothing. Each check prints a PASS line, the persisted proof.

use std::time::{Duration, Instant};

use ledger_check::{Options, check_cell, compare_chains};
use ledger_core::{AccountId, Currency, EntryId, JournalId, Money, TenantId};
use ledger_repl::{Entry, apply, catch_up, lag, publish_watermark, pull_all};
use ledger_store::{Leg, LegWriter, Side, migrate};
use native_tls::TlsConnector;
use postgres_native_tls::MakeTlsConnector;
use tokio_postgres::Client;

const S: &str = "repl_neon";
const TENANT: &str = "ten_acme";
const SRC: &str = "neon-use1";

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
           ('{TENANT}', 'acc_b', 'customer', 'GBP', 'credit', true),
           ('{TENANT}', 'acc_c', 'customer', 'GBP', 'credit', true);
         insert into {S}.epoch_fence (tenant_id, account_id, epoch, home) values
           ('{TENANT}', 'acc_a', 1, '{SRC}'), ('{TENANT}', 'acc_b', 1, '{SRC}'), ('{TENANT}', 'acc_c', 1, '{SRC}')"
    ))
    .await
    .unwrap();
    c
}

async fn workload(c: &Client, n: usize) {
    let w = LegWriter::prepare(c, S).await.unwrap();
    let a = ["acc_a", "acc_b", "acc_c"];
    for i in 0..n {
        for (leg, (acc, side)) in [(a[i % 3], Side::Debit), (a[(i + 1) % 3], Side::Credit)]
            .into_iter()
            .enumerate()
        {
            w.post(&Leg {
                tenant: TENANT.parse::<TenantId>().unwrap(),
                journal: format!("jr_w{i}").parse::<JournalId>().unwrap(),
                account: acc.parse::<AccountId>().unwrap(),
                leg: leg as i16 + 1,
                side,
                amount: Money::new(1 + i as i64, "GBP".parse::<Currency>().unwrap()),
                counterparty: None,
                origin: SRC.to_owned(),
                epoch: 1,
                entry: format!("ent_w{i}x{leg}").parse::<EntryId>().unwrap(),
            })
            .await
            .unwrap();
        }
    }
}

async fn log(c: &Client) -> Vec<(String, i64, i64, i64)> {
    c.query(
        &format!("select id, epoch, seq, amount from {S}.entry order by account_id, epoch, seq"),
        &[],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
    .collect()
}

#[tokio::test]
#[ignore = "runs through the neon-rust workflow, which holds the connection strings"]
async fn the_relay_replicates_between_two_neon_cells() {
    let (Ok(d1), Ok(d2)) = (
        std::env::var("LEDGER_DSN_USE1"),
        std::env::var("LEDGER_DSN_USE2"),
    ) else {
        eprintln!("LEDGER_DSN_USE1 and LEDGER_DSN_USE2 are not both set: nothing to do");
        return;
    };
    let (src, dest) = (cell(&d1).await, cell(&d2).await);
    workload(&src, 30).await;

    // Idle-cell watermark and lag before anything is read.
    assert_eq!(publish_watermark(&src, S, TENANT, SRC).await.unwrap(), 60);
    assert_eq!(lag(&src, &dest, S, TENANT, SRC).await.unwrap(), Some(60));

    let t = Instant::now();
    let r = pull_all(&src, &dest, S, TENANT, SRC, 20).await.unwrap();
    println!(
        "pulled {} entries in {} batches' worth of calls in {:?}",
        r.read,
        r.read.div_ceil(20),
        t.elapsed()
    );
    assert_eq!((r.read, r.applied.applied), (60, 60));
    assert_eq!(log(&src).await, log(&dest).await);
    println!(
        "PASS order: 60 entries shipped across regions in batches, identical entries in identical order"
    );

    assert_eq!(lag(&src, &dest, S, TENANT, SRC).await.unwrap(), Some(0));
    println!("PASS watermark: the source published its position and the destination's lag is 0");

    dest.batch_execute(&format!("update {S}.outbox_cursor set last_position = 0"))
        .await
        .unwrap();
    let again = pull_all(&src, &dest, S, TENANT, SRC, 25).await.unwrap();
    assert_eq!((again.applied.applied, again.applied.dup), (0, 60));
    println!("PASS redelivery: pulling everything again applied nothing twice (60 duplicates)");

    assert_eq!(
        check_cell(&dest, S, "use2", &Options::default())
            .await
            .unwrap(),
        []
    );
    assert_eq!(
        compare_chains(&[("use1", &src), ("use2", &dest)], S)
            .await
            .unwrap(),
        []
    );
    println!(
        "PASS checker: ledger-check finds the destination healthy and its hash chains equal to the source's"
    );

    // A gap: later entries first, then the earlier ones.
    let e = |n: i64| Entry {
        id: format!("ent_g{n}"),
        journal: format!("jr_g{n}"),
        account: "acc_a".to_owned(),
        epoch: 1,
        seq: 100 + n,
        leg: 1,
        kind: "credit".to_owned(),
        amount: n,
        counterparty: None,
        origin: SRC.to_owned(),
        created_at: "2026-11-14T10:00:00Z".to_owned(),
    };
    let top: i64 = dest
        .query_one(
            &format!("select max(seq) from {S}.entry where account_id = 'acc_a'"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let mut late = e(2);
    late.seq = top + 2;
    let mut early = e(1);
    early.seq = top + 1;
    let r = apply(&dest, S, TENANT, None, &[late]).await.unwrap();
    assert_eq!((r.applied, r.buffered), (0, 1));
    let r = apply(&dest, S, TENANT, None, &[early]).await.unwrap();
    assert_eq!((r.applied, r.drained), (1, 1));
    println!(
        "PASS gap: an entry that arrived early was buffered, then applied in order behind its predecessor"
    );

    // Catch-up finds what the destination lacks, by account.
    src.batch_execute(&format!("select {S}.post_leg('{TENANT}','jr_tail','acc_b',1::smallint,'credit',7,'GBP',null,'{SRC}',1,'ent_tail')"))
        .await
        .unwrap();
    let c = catch_up(&src, &dest, S, TENANT, 50).await.unwrap();
    assert!(c.accounts >= 1);
    assert!(log(&dest).await.iter().any(|x| x.0 == "ent_tail"));
    println!("PASS catch-up: an entry the cursor had not carried was fetched by account");

    for c in [&src, &dest] {
        c.batch_execute(&format!("drop schema if exists {S} cascade"))
            .await
            .unwrap();
    }
    println!("PASS cleanup: schema dropped on both cells");
}
