//! K-03 against a real Neon cell: posting a leg costs one round trip over the wide-area network.
//!
//! The Python harness measured about 15 sequential statements for each transfer, which is why a
//! transfer cost 600 to 970 ms from one remote client (docs/design.md, "Write path performance").
//! This test posts legs through the single SQL call and compares each with the cost of the
//! plainest possible statement over the same connection. One leg must cost about one round trip.
//!
//! It runs only through the `neon` workflow, which holds the connection strings
//! (`LEDGER_DSN_USE1`, `LEDGER_DSN_USE2`, `LEDGER_DSN_USW2`). It uses a schema of its own,
//! `k03_neon`, and drops it afterwards. Without the connection string it does nothing.

use std::time::{Duration, Instant};

use ledger_core::{AccountId, Currency, EntryId, JournalId, Money, TenantId};
use ledger_store::{Leg, LegError, LegWriter, Side, migrate};
use native_tls::TlsConnector;
use postgres_native_tls::MakeTlsConnector;
use tokio_postgres::Client;

const SCHEMA: &str = "k03_neon";
const SAMPLES: usize = 15;

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

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn leg(side: Side, minor: i64, journal: String, entry: String) -> Leg {
    Leg {
        tenant: "ten_acme".parse::<TenantId>().unwrap(),
        journal: journal.parse::<JournalId>().unwrap(),
        account: "acc_a".parse::<AccountId>().unwrap(),
        leg: 1,
        side,
        amount: Money::new(minor, "GBP".parse::<Currency>().unwrap()),
        counterparty: None,
        origin: "neon-use1".to_owned(),
        epoch: 1,
        entry: entry.parse::<EntryId>().unwrap(),
    }
}

#[tokio::test]
#[ignore = "runs through the neon workflow, which holds the connection strings"]
async fn one_leg_costs_about_one_round_trip_on_neon() {
    let Ok(dsn) = std::env::var("LEDGER_DSN_USE1") else {
        eprintln!("LEDGER_DSN_USE1 is not set: nothing to do");
        return;
    };
    let mut c = connect(&dsn).await;
    c.batch_execute(&format!("drop schema if exists {SCHEMA} cascade"))
        .await
        .unwrap();
    migrate(&mut c, SCHEMA).await.unwrap();
    c.batch_execute(&format!(
        "insert into {SCHEMA}.account (tenant_id, id, kind, currency, normal_side, allow_negative) \
         values ('ten_acme', 'acc_a', 'customer', 'GBP', 'credit', false);
         insert into {SCHEMA}.epoch_fence (tenant_id, account_id, epoch, home) \
         values ('ten_acme', 'acc_a', 1, 'neon-use1');"
    ))
    .await
    .unwrap();

    let writer = LegWriter::prepare(&c, SCHEMA).await.unwrap();
    // Warm up the connection, then time the plainest statement: one round trip and no work.
    for _ in 0..3 {
        c.batch_execute("select 1").await.unwrap();
    }
    let mut baseline = Vec::new();
    for _ in 0..SAMPLES {
        let t = Instant::now();
        c.batch_execute("select 1").await.unwrap();
        baseline.push(t.elapsed());
    }
    let rtt = median(baseline);

    let mut legs = Vec::new();
    for i in 0..SAMPLES {
        let t = Instant::now();
        let posted = writer
            .post(&leg(
                Side::Credit,
                100,
                format!("jr_n{i}"),
                format!("ent_n{i}"),
            ))
            .await
            .unwrap();
        legs.push(t.elapsed());
        assert_eq!(posted.seq, i as i64 + 1);
    }
    let leg_cost = median(legs);

    // The same debits against a balance that cannot cover them, to time a refusal too.
    let refused = writer
        .post(&leg(
            Side::Debit,
            1_000_000,
            "jr_big".into(),
            "ent_big".into(),
        ))
        .await
        .unwrap_err();
    assert!(matches!(refused, LegError::InsufficientFunds(_)));

    println!(
        "neon use1: round trip (select 1) p50 {:?}, post_leg p50 {:?}, ratio {:.2}",
        rtt,
        leg_cost,
        leg_cost.as_secs_f64() / rtt.as_secs_f64()
    );
    // About one round trip, plus the commit's disk write and a little work. The harness's 15
    // statements would cost about fifteen times the round trip, so 3x separates them clearly.
    assert!(
        leg_cost < rtt * 3 + Duration::from_millis(20),
        "a leg took {leg_cost:?}, more than three round trips of {rtt:?}"
    );

    c.batch_execute(&format!("drop schema if exists {SCHEMA} cascade"))
        .await
        .unwrap();
}
