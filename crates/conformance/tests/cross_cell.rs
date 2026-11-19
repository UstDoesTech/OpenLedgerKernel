//! K-04: journals across cells through in-transit accounts. Two cells are two real Postgres
//! nodes, never a mock.
//!
//! Run with `make up` (or `scripts/local_nodes.sh start`), then
//! `cargo test -p conformance --test cross_cell -- --ignored`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ledger_core::{AccountId, Currency, JournalId, Money, TenantId};
use ledger_store::cross_cell::{self, Transfer};
use ledger_store::{LegError, migrate};
use tokio_postgres::{Client, NoTls};

const PAYER_PORT: u16 = 5431;
const PAYEE_PORT: u16 = 5432;
const TENANT: &str = "ten_acme";
const LON: &str = "aws-lon-01";
const CAR: &str = "gcp-car-01";

async fn connect_to(port: u16) -> Client {
    let (client, conn) = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(port)
        .user("ledger")
        .dbname("postgres")
        .connect_timeout(Duration::from_secs(5))
        .connect(NoTls)
        .await
        .unwrap_or_else(|e| panic!("port {port}: {e}"));
    tokio::spawn(conn);
    client
}

/// Two cells with the ledger schema, both accounts of a pair of cells in each direction, and
/// the given customer accounts on both (accounts are known on every node).
/// Payers `acc_p1..n` live on the first cell and may overdraw up to a large limit, so the
/// workload tests never decline; payees `acc_q1..n` live on the second.
async fn cells(schema: &str, customers: usize) -> (Client, Client) {
    let mut cells = Vec::new();
    for port in [PAYER_PORT, PAYEE_PORT] {
        let mut c = connect_to(port).await;
        c.batch_execute(&format!("drop schema if exists {schema} cascade"))
            .await
            .unwrap();
        migrate(&mut c, schema).await.unwrap();
        for i in 1..=customers {
            for (prefix, limit) in [("p", 1_000_000), ("q", 1_000_000)] {
                c.batch_execute(&format!(
                    "insert into {schema}.account (tenant_id, id, kind, currency, normal_side, allow_negative, overdraft_limit)
                     values ('{TENANT}', 'acc_{prefix}{i}', 'customer', 'GBP', 'credit', false, {limit});
                     insert into {schema}.epoch_fence (tenant_id, account_id, epoch, home)
                     values ('{TENANT}', 'acc_{prefix}{i}', 1, '{LON}')"
                ))
                .await
                .unwrap();
            }
        }
        for (from, to, sending, receiving) in [
            (LON, CAR, "acc_transitlonsend", "acc_transitlonrecv"),
            (CAR, LON, "acc_transitcarsend", "acc_transitcarrecv"),
        ] {
            // Registering twice is safe.
            for _ in 0..2 {
                c.execute(
                    &format!("select {schema}.register_in_transit($1,$2,$3,'GBP',$4,$5)"),
                    &[&TENANT, &from, &to, &sending, &receiving],
                )
                .await
                .unwrap();
            }
            for a in [sending, receiving] {
                c.batch_execute(&format!(
                    "insert into {schema}.epoch_fence (tenant_id, account_id, epoch, home)
                     values ('{TENANT}', '{a}', 1, '{from}') on conflict do nothing"
                ))
                .await
                .unwrap();
            }
        }
        cells.push(c);
    }
    let payee = cells.pop().unwrap();
    let payer = cells.pop().unwrap();
    (payer, payee)
}

async fn drop_all(schema: &str) {
    for port in [PAYER_PORT, PAYEE_PORT] {
        connect_to(port)
            .await
            .batch_execute(&format!("drop schema if exists {schema} cascade"))
            .await
            .unwrap();
    }
}

fn transfer(journal: &str, payer: &str, payee: &str, minor: i64) -> Transfer {
    Transfer {
        tenant: TENANT.parse::<TenantId>().unwrap(),
        journal: journal.parse::<JournalId>().unwrap(),
        actor: "usr_ada".to_owned(),
        payer: payer.parse::<AccountId>().unwrap(),
        payee: payee.parse::<AccountId>().unwrap(),
        sending: "acc_transitlonsend".parse().unwrap(),
        receiving: "acc_transitlonrecv".parse().unwrap(),
        amount: Money::new(minor, "GBP".parse::<Currency>().unwrap()),
        payer_cell: LON.to_owned(),
        payee_cell: CAR.to_owned(),
        payer_epoch: 1,
        sending_epoch: 1,
        receiving_epoch: 1,
        payee_epoch: 1,
    }
}

async fn balance(c: &Client, schema: &str, id: &str) -> i64 {
    c.query_one(
        &format!("select balance from {schema}.account where tenant_id = $1 and id = $2"),
        &[&TENANT, &id],
    )
    .await
    .unwrap()
    .get(0)
}

async fn count(c: &Client, schema: &str, table: &str) -> i64 {
    c.query_one(&format!("select count(*) from {schema}.{table}"), &[])
        .await
        .unwrap()
        .get(0)
}

/// The cell's trial balance: every entry and every balance on the cell sums to zero.
async fn trial_balance(c: &Client, schema: &str) -> (i64, i64) {
    let r = c
        .query_one(
            &format!(
                "select (select coalesce(sum(amount), 0)::bigint from {schema}.entry),
                        (select coalesce(sum(balance), 0)::bigint from {schema}.account)"
            ),
            &[],
        )
        .await
        .unwrap();
    (r.get(0), r.get(1))
}

/// What is still between legs: the sending account's balance at the payer's cell plus the
/// receiving account's at the payee's. Zero once every journal has finished.
async fn in_transit(payer: &Client, payee: &Client, schema: &str) -> i64 {
    balance(payer, schema, "acc_transitlonsend").await
        + balance(payee, schema, "acc_transitlonrecv").await
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn a_cross_cell_transfer_posts_one_pair_on_each_cell() {
    let s = "k04_pairs";
    let (payer, payee) = cells(s, 1).await;
    let t = transfer("jr_one", "acc_p1", "acc_q1", 2_500);

    assert!(cross_cell::transfer(&payer, &payee, s, &t).await.unwrap());

    // The payer's cell holds payer to in-transit, and nothing for the payee.
    assert_eq!(balance(&payer, s, "acc_p1").await, -2_500);
    assert_eq!(balance(&payer, s, "acc_transitlonsend").await, 2_500);
    assert_eq!(balance(&payer, s, "acc_q1").await, 0);
    assert_eq!(count(&payer, s, "entry").await, 2);
    // The payee's cell holds in-transit to payee, and nothing for the payer.
    assert_eq!(balance(&payee, s, "acc_q1").await, 2_500);
    assert_eq!(balance(&payee, s, "acc_transitlonrecv").await, -2_500);
    assert_eq!(balance(&payee, s, "acc_p1").await, 0);
    assert_eq!(count(&payee, s, "entry").await, 2);

    assert_eq!(trial_balance(&payer, s).await, (0, 0));
    assert_eq!(trial_balance(&payee, s).await, (0, 0));
    assert_eq!(in_transit(&payer, &payee, s).await, 0);
    let state: String = payer
        .query_one(
            &format!("select state from {s}.journal where id = 'jr_one'"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(state, "posted");

    // A repeat of the whole transfer changes nothing.
    assert!(!cross_cell::transfer(&payer, &payee, s, &t).await.unwrap());
    assert_eq!(count(&payer, s, "entry").await, 2);
    assert_eq!(count(&payee, s, "entry").await, 2);
    drop_all(s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn a_journal_declined_at_the_first_leg_writes_nothing() {
    let s = "k04_declined";
    let (payer, payee) = cells(s, 1).await;
    // No overdraft: the payer cannot afford it.
    payer
        .batch_execute(&format!(
            "update {s}.account set overdraft_limit = 0 where id = 'acc_p1'"
        ))
        .await
        .unwrap();
    let t = transfer("jr_poor", "acc_p1", "acc_q1", 100);

    let err = cross_cell::send(&payer, s, &t).await.unwrap_err();
    assert!(matches!(err, LegError::InsufficientFunds(_)), "{err}");

    for table in ["journal", "entry", "outbox", "settlement"] {
        assert_eq!(count(&payer, s, table).await, 0, "payer cell {table}");
        assert_eq!(count(&payee, s, table).await, 0, "payee cell {table}");
    }
    assert_eq!(balance(&payer, s, "acc_p1").await, 0);
    assert_eq!(balance(&payer, s, "acc_transitlonsend").await, 0);
    drop_all(s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn a_stale_epoch_at_the_first_leg_writes_nothing() {
    let s = "k04_fenced";
    let (payer, _payee) = cells(s, 1).await;
    let mut t = transfer("jr_stale", "acc_p1", "acc_q1", 100);
    t.sending_epoch = 7; // the payer leg would pass, the second half of the pair would not
    let err = cross_cell::send(&payer, s, &t).await.unwrap_err();
    assert!(matches!(err, LegError::Fenced(_)), "{err}");
    for table in ["journal", "entry", "outbox"] {
        assert_eq!(count(&payer, s, table).await, 0, "{table}");
    }
    assert_eq!(balance(&payer, s, "acc_p1").await, 0);
    drop_all(s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn a_crash_between_legs_is_completed_exactly_once_and_in_transit_drains() {
    let s = "k04_recover";
    let (payer, payee) = cells(s, 3).await;
    // Crash after the first half: three journals sent, none received.
    for (j, p, q, n) in [
        ("jr_a", "acc_p1", "acc_q1", 100),
        ("jr_b", "acc_p2", "acc_q2", 250),
        ("jr_c", "acc_p3", "acc_q3", 75),
    ] {
        cross_cell::send(&payer, s, &transfer(j, p, q, n))
            .await
            .unwrap();
    }
    // And one crash after the second half, before the marker.
    let d = transfer("jr_d", "acc_p1", "acc_q2", 40);
    cross_cell::send(&payer, s, &d).await.unwrap();
    cross_cell::receive(&payee, s, &d).await.unwrap();

    // Mid-flight: every cell balances, and the value in flight is what was sent and not yet received.
    assert_eq!(trial_balance(&payer, s).await, (0, 0));
    assert_eq!(trial_balance(&payee, s).await, (0, 0));
    assert_eq!(in_transit(&payer, &payee, s).await, 100 + 250 + 75);

    // Several recoveries at once, each on its own connections.
    let tenant: TenantId = TENANT.parse().unwrap();
    let mut runs = Vec::new();
    for _ in 0..4 {
        let (a, b, tenant) = (
            connect_to(PAYER_PORT).await,
            connect_to(PAYEE_PORT).await,
            tenant.clone(),
        );
        runs.push(tokio::spawn(async move {
            cross_cell::recover(&a, &b, s, &tenant).await.unwrap()
        }));
    }
    let mut settled: Vec<String> = Vec::new();
    for r in runs {
        settled.extend(r.await.unwrap().settled);
    }
    settled.sort();
    assert_eq!(
        settled,
        ["jr_a", "jr_b", "jr_c", "jr_d"],
        "each journal settled once in total"
    );

    // Applied exactly once: four entries per journal in all, two on each cell.
    assert_eq!(count(&payer, s, "entry").await, 8);
    assert_eq!(count(&payee, s, "entry").await, 8);
    assert_eq!(balance(&payee, s, "acc_q1").await, 100);
    assert_eq!(balance(&payee, s, "acc_q2").await, 250 + 40);
    assert_eq!(balance(&payee, s, "acc_q3").await, 75);
    assert_eq!(balance(&payer, s, "acc_p1").await, -140);
    assert_eq!(in_transit(&payer, &payee, s).await, 0);
    assert_eq!(trial_balance(&payer, s).await, (0, 0));
    assert_eq!(trial_balance(&payee, s).await, (0, 0));

    // Nothing is left to recover.
    let again = cross_cell::recover(&payer, &payee, s, &tenant)
        .await
        .unwrap();
    assert_eq!((again.found, again.settled.len()), (0, 0));
    drop_all(s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn every_cells_trial_balance_is_zero_when_sampled_mid_flight() {
    let s = "k04_workload";
    let customers = 8;
    let (payer, payee) = cells(s, customers).await;
    drop((payer, payee));

    let stop = Arc::new(AtomicBool::new(false));
    // Samplers: one per cell, each reading one statement at a time while the workload runs.
    let mut samplers = Vec::new();
    for port in [PAYER_PORT, PAYEE_PORT] {
        let stop = stop.clone();
        samplers.push(tokio::spawn(async move {
            let c = connect_to(port).await;
            let mut samples = 0;
            while !stop.load(Ordering::SeqCst) {
                assert_eq!(trial_balance(&c, s).await, (0, 0), "port {port}");
                samples += 1;
            }
            samples
        }));
    }

    // Workers: many transfers at once, all from the payer cell to the payee cell, each the
    // whole of its three steps. Customers are shared, so accounts are contended.
    let mut workers = Vec::new();
    for w in 0..16usize {
        workers.push(tokio::spawn(async move {
            let (a, b) = (connect_to(PAYER_PORT).await, connect_to(PAYEE_PORT).await);
            for n in 0..25usize {
                let p = format!("acc_p{}", (w + n) % customers + 1);
                let q = format!("acc_q{}", (w * 3 + n) % customers + 1);
                let t = transfer(&format!("jr_w{w}n{n}"), &p, &q, 1 + (n as i64 % 7));
                cross_cell::transfer(&a, &b, s, &t).await.unwrap();
            }
        }));
    }
    for w in workers {
        w.await.unwrap();
    }
    stop.store(true, Ordering::SeqCst);
    let mut taken = 0;
    for h in samplers {
        taken += h.await.unwrap();
    }
    assert!(
        taken > 20,
        "the samplers ran during the workload ({taken} samples)"
    );

    let (payer, payee) = (connect_to(PAYER_PORT).await, connect_to(PAYEE_PORT).await);
    assert_eq!(count(&payer, s, "entry").await, 16 * 25 * 2);
    assert_eq!(count(&payee, s, "entry").await, 16 * 25 * 2);
    assert_eq!(in_transit(&payer, &payee, s).await, 0);
    assert_eq!(count(&payer, s, "settlement").await, 16 * 25);
    drop_all(s).await;
}
