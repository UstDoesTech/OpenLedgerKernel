//! C-03 against two real Neon cells: the failover orchestrator promotes the standby
//! (`LEDGER_DSN_USE2`) over the old home (`LEDGER_DSN_USE1`) and the old home's tail past the
//! standby's high-water mark is quarantined. The control plane is the in-memory one: the
//! etcd control plane has its own tests, and Neon is here to prove the cells. It runs only
//! through the `neon-rust` workflow, uses a schema of its own, `failover_neon`, and drops it
//! afterwards. Without both connection strings it does nothing. Each check prints a PASS line.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use control_plane::{
    Cell, ControlPlane, Epoch, InMemoryControlPlane, NodeId, Orchestrator, PgCell, SystemClock,
};
use ledger_core::{AccountId, Currency, EntryId, JournalId, Money, TenantId};
use ledger_store::{Leg, LegWriter, Side, migrate};
use native_tls::TlsConnector;
use postgres_native_tls::MakeTlsConnector;
use tokio_postgres::Client;

const S: &str = "failover_neon";
const TENANT: &str = "ten_acme";
const BUDGET: Duration = Duration::from_secs(15);

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
           ('{TENANT}', 'acc_b', 'customer', 'GBP', 'credit', true)"
    ))
    .await
    .unwrap();
    c
}

async fn journal(c: &Client, n: i64) {
    let w = LegWriter::prepare(c, S).await.unwrap();
    for (i, (acc, side)) in [("acc_a", Side::Debit), ("acc_b", Side::Credit)]
        .into_iter()
        .enumerate()
    {
        w.post(&Leg {
            tenant: TENANT.parse::<TenantId>().unwrap(),
            journal: format!("jr_n{n}").parse::<JournalId>().unwrap(),
            account: acc.parse::<AccountId>().unwrap(),
            leg: i as i16 + 1,
            side,
            amount: Money::new(n, "GBP".parse::<Currency>().unwrap()),
            counterparty: None,
            origin: "neon-use1".to_owned(),
            epoch: 1,
            entry: format!("ent_n{n}x{i}").parse::<EntryId>().unwrap(),
        })
        .await
        .unwrap();
    }
}

async fn seqs(c: &Client, account: &str) -> Vec<i64> {
    c.query(
        &format!("select seq from {S}.entry where account_id = $1 order by seq"),
        &[&account],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| r.get(0))
    .collect()
}

#[tokio::test]
#[ignore = "needs LEDGER_DSN_USE1 and LEDGER_DSN_USE2; runs in the neon-rust workflow"]
async fn the_orchestrator_promotes_across_two_neon_cells() {
    let (Ok(dsn1), Ok(dsn2)) = (
        std::env::var("LEDGER_DSN_USE1"),
        std::env::var("LEDGER_DSN_USE2"),
    ) else {
        eprintln!("LEDGER_DSN_USE1 and LEDGER_DSN_USE2 are not both set: nothing to do");
        return;
    };
    let (use1, use2) = (Arc::new(cell(&dsn1).await), Arc::new(cell(&dsn2).await));
    let (n1, n2): (NodeId, NodeId) = ("neon-use1".parse().unwrap(), "neon-use2".parse().unwrap());
    let cells: BTreeMap<NodeId, PgCell> = [
        (n1.clone(), PgCell::new(use1.clone(), S, TENANT)),
        (n2.clone(), PgCell::new(use2.clone(), S, TENANT)),
    ]
    .into();

    let control = InMemoryControlPlane::new(Duration::from_secs(30), Arc::new(SystemClock::new()));
    let (a, b): (AccountId, AccountId) = ("acc_a".parse().unwrap(), "acc_b".parse().unwrap());
    control.register(&a, &n1).await.unwrap();
    control.register(&b, &n1).await.unwrap();
    for f in control.fences().await.unwrap() {
        for c in cells.values() {
            c.install_fence(&f).await.unwrap();
        }
    }

    // The old home wrote five journals. The standby has the first three.
    for n in 1..=5 {
        journal(&use1, n).await;
    }
    for n in 1..=3 {
        journal(&use2, n).await;
    }
    let o = Orchestrator::new(control, cells, Duration::from_secs(20));

    let started = Instant::now();
    let done = o.promote(&a, &n2).await.unwrap();
    let took = started.elapsed();
    assert_eq!(done.fence.epoch, Epoch(2));
    assert_eq!(done.fence.prev_cutoff_seq, 3);
    assert_eq!(done.standby_high_water, (Epoch(1), 3));
    println!(
        "PASS promotion: the standby's own high-water mark (epoch 1, seq 3) became the cutoff for epoch 1"
    );
    assert!(done.not_installed.is_empty() && done.installed.len() == 2);
    assert_eq!(done.installed[0], n2, "the new home is fenced first");
    println!(
        "PASS fence order: the new home took the fence first, then the old home, both reached"
    );
    assert!(took < BUDGET, "took {took:?}");
    println!("PASS time: promotion across two Neon regions took {took:?}, inside {BUDGET:?}");

    assert_eq!(seqs(&use1, "acc_a").await, vec![1, 2, 3]);
    assert_eq!(seqs(&use2, "acc_a").await, vec![1, 2, 3]);
    let quarantined: i64 = use1
        .query_one(
            &format!("select count(*) from {S}.quarantine where account_id = 'acc_a'"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(quarantined, 2);
    println!("PASS swept: the old home's entries 4 and 5 on acc_a were quarantined by the fence");

    // Draining is a marker and moves nothing.
    o.drain(&n1).await.unwrap();
    assert_eq!(o.control().draining().await.unwrap(), [n1.clone()].into());
    assert_eq!(o.control().home(&b).await.unwrap(), (n1.clone(), Epoch(1)));
    o.undrain(&n1).await.unwrap();
    println!("PASS drain: draining the old home moved no account");

    for c in [&use1, &use2] {
        c.batch_execute(&format!("drop schema if exists {S} cascade"))
            .await
            .unwrap();
    }
    println!("PASS cleanup: schema {S} dropped on both cells");
}
