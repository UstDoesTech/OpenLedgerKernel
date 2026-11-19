//! R-02 against two real Neon cells: a failover in which the old home (`LEDGER_DSN_USE1`) returns
//! after the standby (`LEDGER_DSN_USE2`) was promoted. It runs only through the `neon-rust`
//! workflow, uses a schema of its own, `fence_neon`, and drops it afterwards. Without both
//! connection strings it does nothing. Each check prints a PASS line, the persisted proof.

use std::time::Duration;

use ledger_check::{Options, check_cell, compare_chains};
use ledger_core::{AccountId, Currency, EntryId, JournalId, Money, TenantId};
use ledger_repl::{Entry, Fence, apply, high_water_of, install_fence, pull_all};
use ledger_store::{Leg, LegWriter, Side, migrate};
use native_tls::TlsConnector;
use postgres_native_tls::MakeTlsConnector;
use tokio_postgres::Client;

const S: &str = "fence_neon";
const TENANT: &str = "ten_acme";

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
           ('{TENANT}', 'acc_a', 1, 'neon-use1'), ('{TENANT}', 'acc_b', 1, 'neon-use1')"
    ))
    .await
    .unwrap();
    c
}

async fn journal(c: &Client, n: i64, epoch: i64, origin: &str) {
    let w = LegWriter::prepare(c, S).await.unwrap();
    for (i, (acc, side)) in [("acc_a", Side::Debit), ("acc_b", Side::Credit)]
        .into_iter()
        .enumerate()
    {
        w.post(&Leg {
            tenant: TENANT.parse::<TenantId>().unwrap(),
            journal: format!("jr_e{epoch}n{n}").parse::<JournalId>().unwrap(),
            account: acc.parse::<AccountId>().unwrap(),
            leg: i as i16 + 1,
            side,
            amount: Money::new(n, "GBP".parse::<Currency>().unwrap()),
            counterparty: None,
            origin: origin.to_owned(),
            epoch,
            entry: format!("ent_e{epoch}n{n}x{i}").parse::<EntryId>().unwrap(),
        })
        .await
        .unwrap();
    }
}

async fn seqs(c: &Client, account: &str) -> Vec<(i64, i64)> {
    c.query(
        &format!("select epoch, seq from {S}.entry where account_id = $1 order by epoch, seq"),
        &[&account],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get(0), r.get(1)))
    .collect()
}

async fn balance(c: &Client, account: &str) -> i64 {
    c.query_one(
        &format!("select balance from {S}.account where id = $1"),
        &[&account],
    )
    .await
    .unwrap()
    .get(0)
}

#[tokio::test]
#[ignore = "runs through the neon-rust workflow, which holds the connection strings"]
async fn a_returning_stale_home_is_fenced_across_two_neon_cells() {
    let (Ok(d1), Ok(d2)) = (
        std::env::var("LEDGER_DSN_USE1"),
        std::env::var("LEDGER_DSN_USE2"),
    ) else {
        eprintln!("LEDGER_DSN_USE1 and LEDGER_DSN_USE2 are not both set: nothing to do");
        return;
    };
    let (old, new) = (cell(&d1).await, cell(&d2).await);
    for n in 1..=5 {
        journal(&old, n, 1, "neon-use1").await;
    }
    // The standby replicated up to seq 3 before the old home went quiet.
    let rows = old
        .query(
            &format!("select id, journal_id, account_id, epoch, seq, leg, kind, amount, counterparty, origin, created_at::text from {S}.entry where seq <= 3 order by account_id, epoch, seq"),
            &[],
        )
        .await
        .unwrap();
    let head: Vec<Entry> = rows
        .iter()
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
        .collect();
    apply(&new, S, TENANT, None, &head).await.unwrap();
    let cutoff = high_water_of(&new, S, TENANT, "acc_a").await.unwrap();
    assert_eq!(cutoff, (1, 3));
    println!("PASS promotion: the standby reports its high-water mark {cutoff:?} as the cutoff");

    let f = |account: &str| Fence {
        tenant: TENANT.to_owned(),
        account: account.to_owned(),
        epoch: 2,
        home: "neon-use2".to_owned(),
        prev_epoch: cutoff.0,
        prev_cutoff_seq: cutoff.1,
    };
    for a in ["acc_a", "acc_b"] {
        assert!(install_fence(&new, S, &f(a)).await.unwrap());
        assert!(!install_fence(&new, S, &f(a)).await.unwrap());
    }
    for n in 6..=7 {
        journal(&new, n, 2, "neon-use2").await;
    }
    println!(
        "PASS fence: epoch 2 installed on the new home, repeating it changes nothing, and it writes at epoch 2"
    );

    let r = pull_all(&old, &new, S, TENANT, "neon-use1", 50)
        .await
        .unwrap();
    assert_eq!(r.applied.quarantined, 4, "{r:?}");
    println!(
        "PASS received: the stale home's tail (seq 4 and 5 of both accounts) was quarantined on arrival across regions"
    );

    for a in ["acc_a", "acc_b"] {
        install_fence(&old, S, &f(a)).await.unwrap();
    }
    let swept: i64 = old
        .query_one(
            &format!("select count(*) from {S}.quarantine where reason = 'void_after_fence'"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(swept, 4);
    println!(
        "PASS swept: installing the fence on the old home quarantined its own tail and reversed the projection"
    );

    pull_all(&new, &old, S, TENANT, "neon-use2", 50)
        .await
        .unwrap();
    assert_eq!(seqs(&old, "acc_a").await, seqs(&new, "acc_a").await);
    for a in ["acc_a", "acc_b"] {
        assert_eq!(balance(&old, a).await, balance(&new, a).await);
    }
    assert_eq!(
        compare_chains(&[("use1", &old), ("use2", &new)], S)
            .await
            .unwrap(),
        []
    );
    let tolerant = Options {
        quarantine_max_age_secs: 3600,
        ..Options::default()
    };
    for (name, c) in [("use1", &old), ("use2", &new)] {
        let v = check_cell(c, S, name, &tolerant).await.unwrap();
        assert!(
            v.iter().all(|x| x.invariant == "journal_unbalanced"),
            "{name}: {v:?}"
        );
    }
    println!(
        "PASS converged: both cells hold the same log, balances and hash chains; the checker finds only the quarantine"
    );

    for c in [&old, &new] {
        c.batch_execute(&format!("drop schema if exists {S} cascade"))
            .await
            .unwrap();
    }
    println!("PASS cleanup: schema dropped on both cells");
}
