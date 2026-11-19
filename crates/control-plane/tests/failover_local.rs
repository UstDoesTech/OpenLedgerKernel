#![cfg(feature = "etcd")]
//! C-03: the orchestrator against three real Postgres nodes and a real etcd. A promotion
//! must quarantine the old home's stale tail, leave a level standby untouched, and finish well
//! inside the 15 second budget.
//!
//! Run with `scripts/local_nodes.sh start` and an etcd on 2379, then
//! `cargo test -p control-plane --test failover_local -- --ignored`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use control_plane::{
    ControlPlane, Epoch, EtcdConfig, EtcdControlPlane, NodeId, Orchestrator, PgCell,
};
use ledger_core::{AccountId, Currency, EntryId, JournalId, Money, TenantId};
use ledger_store::{Leg, LegWriter, Side, migrate};
use tokio_postgres::{Client, NoTls};

const TENANT: &str = "ten_acme";
const SCHEMA: &str = "c03_failover";
const BUDGET: Duration = Duration::from_secs(15);

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

async fn journal(c: &Client, n: i64) {
    let w = LegWriter::prepare(c, SCHEMA).await.unwrap();
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
            origin: "aws-lon-01".to_owned(),
            epoch: 1,
            entry: format!("ent_n{n}x{i}").parse::<EntryId>().unwrap(),
        })
        .await
        .unwrap();
    }
}

fn node(s: &str) -> NodeId {
    s.parse().unwrap()
}

async fn seqs(c: &Client, account: &str) -> Vec<i64> {
    c.query(
        &format!("select seq from {SCHEMA}.entry where account_id = $1 order by seq"),
        &[&account],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| r.get(0))
    .collect()
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433 and etcd on 2379"]
async fn promotion_quarantines_the_stale_tail_and_a_level_standby_loses_nothing() {
    let etcd = std::env::var("LEDGER_ETCD").unwrap_or_else(|_| "http://127.0.0.1:2379".into());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let cp = EtcdControlPlane::connect(EtcdConfig::new(
        vec![etcd],
        format!("/control-plane-test/c03-{nanos}-{}", std::process::id()),
        Duration::from_secs(2),
    ))
    .await
    .unwrap();

    let names = ["aws-lon-01", "azure-lon-01", "gcp-lon-01"];
    let mut clients = Vec::new();
    for port in [5431, 5432, 5433] {
        let mut c = connect(port).await;
        c.batch_execute(&format!("drop schema if exists {SCHEMA} cascade"))
            .await
            .unwrap();
        migrate(&mut c, SCHEMA).await.unwrap();
        c.batch_execute(&format!(
            "insert into {SCHEMA}.account (tenant_id, id, kind, currency, normal_side, allow_negative) values
               ('{TENANT}', 'acc_a', 'customer', 'GBP', 'credit', true),
               ('{TENANT}', 'acc_b', 'customer', 'GBP', 'credit', true)"
        ))
        .await
        .unwrap();
        clients.push(Arc::new(c));
    }
    let cells: BTreeMap<NodeId, PgCell> = names
        .iter()
        .zip(&clients)
        .map(|(n, c)| (node(n), PgCell::new(c.clone(), SCHEMA, TENANT)))
        .collect();

    // Both accounts are homed on aws at epoch 1 and every cell has the registration fences.
    let (a, b): (AccountId, AccountId) = ("acc_a".parse().unwrap(), "acc_b".parse().unwrap());
    cp.register(&a, &node("aws-lon-01")).await.unwrap();
    cp.register(&b, &node("aws-lon-01")).await.unwrap();
    for f in cp.fences().await.unwrap() {
        for cell in cells.values() {
            control_plane::Cell::install_fence(cell, &f).await.unwrap();
        }
    }

    // aws wrote five journals. azure has the first three, gcp has all five.
    for n in 1..=5 {
        journal(&clients[0], n).await;
        journal(&clients[2], n).await;
    }
    for n in 1..=3 {
        journal(&clients[1], n).await;
    }

    let orchestrator = Orchestrator::new(cp, cells, Duration::from_secs(5));

    // acc_a: azure is behind by two, so the cutoff is 3 and aws's entries 4 and 5 are void.
    let started = Instant::now();
    let done = orchestrator
        .promote(&a, &node("azure-lon-01"))
        .await
        .unwrap();
    assert!(started.elapsed() < BUDGET, "took {:?}", started.elapsed());
    assert_eq!(done.fence.epoch, Epoch(2));
    assert_eq!(done.fence.prev_cutoff_seq, 3);
    assert_eq!(done.installed.len(), 3);
    assert_eq!(
        seqs(&clients[0], "acc_a").await,
        vec![1, 2, 3],
        "aws stale tail"
    );
    assert_eq!(
        seqs(&clients[2], "acc_a").await,
        vec![1, 2, 3],
        "gcp stale tail"
    );
    assert_eq!(
        seqs(&clients[1], "acc_a").await,
        vec![1, 2, 3],
        "azure unchanged"
    );
    let quarantined: i64 = clients[0]
        .query_one(
            &format!("select count(*) from {SCHEMA}.quarantine where account_id = 'acc_a'"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(quarantined, 2);

    // acc_b: promote gcp, which is level with aws, so nothing is lost and it is quick.
    let started = Instant::now();
    let done = orchestrator.promote(&b, &node("gcp-lon-01")).await.unwrap();
    assert!(started.elapsed() < BUDGET, "took {:?}", started.elapsed());
    assert_eq!(done.fence.prev_cutoff_seq, 5);
    assert_eq!(seqs(&clients[0], "acc_b").await, vec![1, 2, 3, 4, 5]);
    assert_eq!(seqs(&clients[2], "acc_b").await, vec![1, 2, 3, 4, 5]);

    // The old home is refused at epoch 1 now (LE002), and the control plane agrees.
    let (home, epoch) = orchestrator.control().home(&a).await.unwrap();
    assert_eq!((home, epoch), (node("azure-lon-01"), Epoch(2)));
    let stale = LegWriter::prepare(&clients[0], SCHEMA).await.unwrap();
    let err = stale
        .post(&Leg {
            tenant: TENANT.parse::<TenantId>().unwrap(),
            journal: "jr_late".parse::<JournalId>().unwrap(),
            account: a.clone(),
            leg: 1,
            side: Side::Debit,
            amount: Money::new(1, "GBP".parse::<Currency>().unwrap()),
            counterparty: None,
            origin: "aws-lon-01".to_owned(),
            epoch: 1,
            entry: "ent_late".parse::<EntryId>().unwrap(),
        })
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("LE002") || format!("{err:?}").contains("Fenced"),
        "{err:?}"
    );

    for c in &clients {
        c.batch_execute(&format!("drop schema if exists {SCHEMA} cascade"))
            .await
            .unwrap();
    }
}
