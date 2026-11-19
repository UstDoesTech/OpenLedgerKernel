//! K-04 against two real Neon cells: cross-cell journals through in-transit accounts, with the
//! payer's cell in one Neon project (`LEDGER_DSN_USE1`) and the payee's in another
//! (`LEDGER_DSN_USE2`). It is the persisted proof that the four acceptance lines hold over the
//! wide-area network. It runs only through the `neon-rust` workflow, uses a schema of its own,
//! `k04_neon`, and drops it afterwards. Without both connection strings it does nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ledger_core::{AccountId, Currency, JournalId, Money, TenantId};
use ledger_store::cross_cell::{self, Transfer};
use ledger_store::{LegError, migrate};
use native_tls::TlsConnector;
use postgres_native_tls::MakeTlsConnector;
use tokio_postgres::Client;

const S: &str = "k04_neon";
const TENANT: &str = "ten_acme";
const LON: &str = "neon-use1";
const CAR: &str = "neon-use2";

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

async fn setup(dsn: &str, customers: usize) -> Client {
    let mut c = connect(dsn).await;
    c.batch_execute(&format!("drop schema if exists {S} cascade"))
        .await
        .unwrap();
    migrate(&mut c, S).await.unwrap();
    let mut sql = String::new();
    for i in 1..=customers {
        for p in ["p", "q"] {
            sql += &format!(
                "insert into {S}.account (tenant_id, id, kind, currency, normal_side, allow_negative, overdraft_limit)
                 values ('{TENANT}', 'acc_{p}{i}', 'customer', 'GBP', 'credit', false, 1000000);
                 insert into {S}.epoch_fence (tenant_id, account_id, epoch, home) values ('{TENANT}', 'acc_{p}{i}', 1, '{LON}');"
            );
        }
    }
    c.batch_execute(&sql).await.unwrap();
    c.execute(
        &format!(
            "select {S}.register_in_transit($1,$2,$3,'GBP','acc_transitsend','acc_transitrecv')"
        ),
        &[&TENANT, &LON, &CAR],
    )
    .await
    .unwrap();
    c.batch_execute(&format!(
        "insert into {S}.epoch_fence (tenant_id, account_id, epoch, home) values
           ('{TENANT}', 'acc_transitsend', 1, '{LON}'), ('{TENANT}', 'acc_transitrecv', 1, '{CAR}')"
    ))
    .await
    .unwrap();
    c
}

fn transfer(journal: &str, payer: &str, payee: &str, minor: i64) -> Transfer {
    Transfer {
        tenant: TENANT.parse::<TenantId>().unwrap(),
        journal: journal.parse::<JournalId>().unwrap(),
        actor: "usr_ada".to_owned(),
        payer: payer.parse::<AccountId>().unwrap(),
        payee: payee.parse::<AccountId>().unwrap(),
        sending: "acc_transitsend".parse().unwrap(),
        receiving: "acc_transitrecv".parse().unwrap(),
        amount: Money::new(minor, "GBP".parse::<Currency>().unwrap()),
        payer_cell: LON.to_owned(),
        payee_cell: CAR.to_owned(),
        payer_epoch: 1,
        sending_epoch: 1,
        receiving_epoch: 1,
        payee_epoch: 1,
    }
}

async fn balance(c: &Client, id: &str) -> i64 {
    c.query_one(
        &format!("select balance from {S}.account where tenant_id = $1 and id = $2"),
        &[&TENANT, &id],
    )
    .await
    .unwrap()
    .get(0)
}

async fn count(c: &Client, table: &str) -> i64 {
    c.query_one(&format!("select count(*) from {S}.{table}"), &[])
        .await
        .unwrap()
        .get(0)
}

async fn trial_balance(c: &Client) -> (i64, i64) {
    let r = c
        .query_one(
            &format!(
                "select (select coalesce(sum(amount), 0)::bigint from {S}.entry),
                        (select coalesce(sum(balance), 0)::bigint from {S}.account)"
            ),
            &[],
        )
        .await
        .unwrap();
    (r.get(0), r.get(1))
}

async fn in_transit(a: &Client, b: &Client) -> i64 {
    balance(a, "acc_transitsend").await + balance(b, "acc_transitrecv").await
}

#[tokio::test]
#[ignore = "runs through the neon-rust workflow, which holds the connection strings"]
async fn cross_cell_journals_hold_across_two_neon_cells() {
    let (Ok(d1), Ok(d2)) = (
        std::env::var("LEDGER_DSN_USE1"),
        std::env::var("LEDGER_DSN_USE2"),
    ) else {
        eprintln!("LEDGER_DSN_USE1 and LEDGER_DSN_USE2 are not both set: nothing to do");
        return;
    };
    let customers = 6usize;
    let (payer, payee) = (setup(&d1, customers).await, setup(&d2, customers).await);
    let tenant: TenantId = TENANT.parse().unwrap();
    let t0 = Instant::now();

    // 1. One pair of legs on each cell.
    let t = transfer("jr_one", "acc_p1", "acc_q1", 2_500);
    let started = Instant::now();
    assert!(cross_cell::transfer(&payer, &payee, S, &t).await.unwrap());
    println!(
        "one cross-cell transfer, three steps: {:?}",
        started.elapsed()
    );
    assert_eq!(balance(&payer, "acc_p1").await, -2_500);
    assert_eq!(balance(&payer, "acc_transitsend").await, 2_500);
    assert_eq!(balance(&payee, "acc_q1").await, 2_500);
    assert_eq!(balance(&payee, "acc_transitrecv").await, -2_500);
    assert_eq!(
        (count(&payer, "entry").await, count(&payee, "entry").await),
        (2, 2)
    );
    assert_eq!(in_transit(&payer, &payee).await, 0);
    println!(
        "PASS legs: payer-to-in-transit at the payer's cell, in-transit-to-payee at the payee's"
    );

    // 2. Declined at the first leg writes nothing.
    payer
        .batch_execute(&format!(
            "update {S}.account set overdraft_limit = 0 where id = 'acc_p2'"
        ))
        .await
        .unwrap();
    let before = (
        count(&payer, "entry").await,
        count(&payer, "journal").await,
        count(&payer, "outbox").await,
    );
    let err = cross_cell::send(&payer, S, &transfer("jr_poor", "acc_p2", "acc_q2", 100))
        .await
        .unwrap_err();
    assert!(matches!(err, LegError::InsufficientFunds(_)), "{err}");
    assert_eq!(
        before,
        (
            count(&payer, "entry").await,
            count(&payer, "journal").await,
            count(&payer, "outbox").await
        )
    );
    assert_eq!(count(&payee, "entry").await, 2);
    println!("PASS declined: a journal declined at the first leg wrote nothing");

    // 3. Crash between legs: recovered exactly once by concurrent recoveries, in-transit drains.
    for (j, p, q, n) in [
        ("jr_a", "acc_p3", "acc_q3", 100),
        ("jr_b", "acc_p4", "acc_q4", 250),
    ] {
        cross_cell::send(&payer, S, &transfer(j, p, q, n))
            .await
            .unwrap();
    }
    let d = transfer("jr_d", "acc_p3", "acc_q4", 40);
    cross_cell::send(&payer, S, &d).await.unwrap();
    cross_cell::receive(&payee, S, &d).await.unwrap();
    assert_eq!(trial_balance(&payer).await, (0, 0));
    assert_eq!(trial_balance(&payee).await, (0, 0));
    assert_eq!(in_transit(&payer, &payee).await, 350);
    let mut runs = Vec::new();
    for _ in 0..3 {
        let (a, b, tenant) = (connect(&d1).await, connect(&d2).await, tenant.clone());
        runs.push(tokio::spawn(async move {
            cross_cell::recover(&a, &b, S, &tenant).await.unwrap()
        }));
    }
    let mut settled = Vec::new();
    for r in runs {
        settled.extend(r.await.unwrap().settled);
    }
    settled.sort();
    assert_eq!(settled, ["jr_a", "jr_b", "jr_d"]);
    assert_eq!(
        (count(&payer, "entry").await, count(&payee, "entry").await),
        (8, 8)
    );
    assert_eq!(in_transit(&payer, &payee).await, 0);
    println!(
        "PASS recovery: {} journals completed exactly once, in-transit drained to zero",
        settled.len()
    );

    // 4. Trial balance zero on both cells while a concurrent workload runs.
    let stop = Arc::new(AtomicBool::new(false));
    let mut samplers = Vec::new();
    for dsn in [d1.clone(), d2.clone()] {
        let stop = stop.clone();
        samplers.push(tokio::spawn(async move {
            let c = connect(&dsn).await;
            let mut n = 0;
            while !stop.load(Ordering::SeqCst) {
                assert_eq!(trial_balance(&c).await, (0, 0));
                n += 1;
            }
            n
        }));
    }
    let (workers, per) = (6usize, 8usize);
    let mut hs = Vec::new();
    for w in 0..workers {
        let (d1, d2) = (d1.clone(), d2.clone());
        hs.push(tokio::spawn(async move {
            let (a, b) = (connect(&d1).await, connect(&d2).await);
            for n in 0..per {
                let p = format!("acc_p{}", (w + n) % customers + 1);
                let q = format!("acc_q{}", (w * 3 + n) % customers + 1);
                // p2 has no overdraft and no funds: keep it out of the workload.
                if p == "acc_p2" {
                    continue;
                }
                let t = transfer(&format!("jr_w{w}n{n}"), &p, &q, 1 + n as i64);
                cross_cell::transfer(&a, &b, S, &t).await.unwrap();
            }
        }));
    }
    for h in hs {
        h.await.unwrap();
    }
    stop.store(true, Ordering::SeqCst);
    let mut samples = 0;
    for s in samplers {
        samples += s.await.unwrap();
    }
    assert!(samples > 0);
    assert_eq!(trial_balance(&payer).await, (0, 0));
    assert_eq!(trial_balance(&payee).await, (0, 0));
    assert_eq!(in_transit(&payer, &payee).await, 0);
    println!("PASS trial balance: zero on both cells in {samples} samples taken mid-flight");
    println!("total {:?}", t0.elapsed());

    for c in [&payer, &payee] {
        c.batch_execute(&format!("drop schema if exists {S} cascade"))
            .await
            .unwrap();
    }
}
