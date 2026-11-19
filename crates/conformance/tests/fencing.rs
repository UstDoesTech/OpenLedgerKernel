//! R-02: fencing and quarantine on every node. The void rule of `ledger/node.py` is enforced in
//! SQL, so it holds however an entry or a fence arrives. Cells are real Postgres nodes.
//!
//! Run with `make up` (or `scripts/local_nodes.sh start`), then
//! `cargo test -p conformance --test fencing -- --ignored`.

use std::time::Duration;

use ledger_check::{Options, check_cell, compare_chains};
use ledger_core::{AccountId, Currency, EntryId, JournalId, Money, TenantId};
use ledger_repl::{Fence, apply, high_water_of, install_fence, pull_all};
use ledger_store::{Leg, LegError, LegWriter, Side, migrate};
use tokio_postgres::{Client, NoTls};

const TENANT: &str = "ten_acme";

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

/// A cell with accounts a and b (which may overdraw) at epoch 1, homed at `home`.
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
           ('{TENANT}', 'acc_a', 1, 'aws-lon-01'), ('{TENANT}', 'acc_b', 1, 'aws-lon-01')"
    ))
    .await
    .unwrap();
    c
}

/// Journal `n` moves `n` from acc_a to acc_b, written at `epoch`.
async fn journal(c: &Client, schema: &str, n: i64, epoch: i64, origin: &str) {
    let w = LegWriter::prepare(c, schema).await.unwrap();
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

fn fence(epoch: i64, home: &str, prev_epoch: i64, cutoff: i64) -> Fence {
    Fence {
        tenant: TENANT.to_owned(),
        account: "acc_a".to_owned(),
        epoch,
        home: home.to_owned(),
        prev_epoch,
        prev_cutoff_seq: cutoff,
    }
}

async fn seqs(c: &Client, schema: &str, account: &str) -> Vec<(i64, i64)> {
    c.query(
        &format!("select epoch, seq from {schema}.entry where account_id = $1 order by epoch, seq"),
        &[&account],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get(0), r.get(1)))
    .collect()
}

async fn balance(c: &Client, schema: &str, account: &str) -> i64 {
    c.query_one(
        &format!("select balance from {schema}.account where id = $1"),
        &[&account],
    )
    .await
    .unwrap()
    .get(0)
}

async fn quarantined(c: &Client, schema: &str, account: &str) -> Vec<(i64, String)> {
    c.query(
        &format!("select seq, reason from {schema}.quarantine where account_id = $1 order by seq"),
        &[&account],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get(0), r.get(1)))
    .collect()
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
async fn installing_a_fence_quarantines_local_entries_past_the_cutoff_and_reverses_the_projection()
{
    let s = "r02_local";
    let c = cell(5431, s).await;
    for n in 1..=5 {
        journal(&c, s, n, 1, "aws-lon-01").await;
    }
    assert_eq!(high_water_of(&c, s, TENANT, "acc_a").await.unwrap(), (1, 5));
    assert_eq!(balance(&c, s, "acc_a").await, -15);

    // The standby that takes over had seen up to seq 3: its cutoff for epoch 1.
    assert!(
        install_fence(&c, s, &fence(2, "gcp-car-01", 1, 3))
            .await
            .unwrap()
    );
    assert_eq!(seqs(&c, s, "acc_a").await, [(1, 1), (1, 2), (1, 3)]);
    assert_eq!(
        quarantined(&c, s, "acc_a").await,
        [
            (4, "void_after_fence".to_owned()),
            (5, "void_after_fence".to_owned())
        ]
    );
    assert_eq!(
        balance(&c, s, "acc_a").await,
        -(1 + 2 + 3),
        "the projection is reversed"
    );
    // acc_b has no fence for epoch 2, so it keeps everything.
    assert_eq!(seqs(&c, s, "acc_b").await.len(), 5);

    // Repeating the fence changes nothing, and a different fence for the same epoch is refused.
    assert!(
        !install_fence(&c, s, &fence(2, "gcp-car-01", 1, 3))
            .await
            .unwrap()
    );
    assert_eq!(quarantined(&c, s, "acc_a").await.len(), 2);
    assert!(
        install_fence(&c, s, &fence(2, "gcp-car-01", 1, 4))
            .await
            .is_err()
    );

    // The sweep keeps the books right: the checker finds only the unresolved quarantine.
    let v = check_cell(
        &c,
        s,
        "use1",
        &Options {
            quarantine_max_age_secs: 3600,
            ..Options::default()
        },
    )
    .await
    .unwrap();
    assert!(
        v.iter()
            .all(|x| x.invariant == "journal_unbalanced" || x.invariant == "trial_balance"),
        "{v:?}"
    );
    drop_all(s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn the_database_refuses_void_and_quarantined_entries_however_they_arrive() {
    let s = "r02_admit";
    let c = cell(5431, s).await;
    for n in 1..=3 {
        journal(&c, s, n, 1, "aws-lon-01").await;
    }
    install_fence(&c, s, &fence(3, "gcp-car-01", 1, 2))
        .await
        .unwrap(); // epoch 2 skipped, epoch 1 cut at 2
    let insert = |epoch: i64, seq: i64, id: &str| {
        format!("insert into {s}.entry (tenant_id, id, journal_id, account_id, epoch, seq, kind, amount, origin)
                 values ('{TENANT}', '{id}', 'jr_direct', 'acc_a', {epoch}, {seq}, 'credit', 1, 'aws-lon-01')")
    };
    let code =
        |e: tokio_postgres::Error| e.as_db_error().map(|d| d.code().code().to_owned()).unwrap();
    // Past the cutoff, in the superseded epoch: void.
    assert_eq!(
        code(c.batch_execute(&insert(1, 3, "ent_v1")).await.unwrap_err()),
        "LE011"
    );
    // In the epoch that was skipped entirely: void.
    assert_eq!(
        code(c.batch_execute(&insert(2, 1, "ent_v2")).await.unwrap_err()),
        "LE011"
    );
    // An entry already in quarantine cannot be put back in the log, even in a valid position.
    c.batch_execute(&format!("insert into {s}.epoch_fence (tenant_id, account_id, epoch, home) values ('{TENANT}', 'acc_b', 2, 'x')")).await.unwrap();
    let q_id: String = c
        .query_one(
            &format!("select id from {s}.quarantine where seq = 3 and account_id = 'acc_a'"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let attempt = format!("insert into {s}.entry (tenant_id, id, journal_id, account_id, epoch, seq, kind, amount, origin)
        values ('{TENANT}', '{q_id}', 'jr_back', 'acc_b', 2, 1, 'credit', 1, 'aws-lon-01')");
    assert_eq!(code(c.batch_execute(&attempt).await.unwrap_err()), "LE012");
    // The relay path agrees: the same quarantined entry arrives again and stays quarantined.
    let before = (
        seqs(&c, s, "acc_a").await,
        quarantined(&c, s, "acc_a").await.len(),
        balance(&c, s, "acc_a").await,
    );
    let row = c.query_one(&format!("select journal_id, account_id, epoch, seq, leg, kind, amount, origin, created_at::text from {s}.quarantine where id = '{q_id}'"), &[]).await.unwrap();
    let again = ledger_repl::Entry {
        id: q_id.clone(),
        journal: row.get(0),
        account: row.get(1),
        epoch: row.get(2),
        seq: row.get(3),
        leg: row.get(4),
        kind: row.get(5),
        amount: row.get(6),
        counterparty: None,
        origin: row.get(7),
        created_at: row.get(8),
    };
    let r = apply(&c, s, TENANT, None, &[again]).await.unwrap();
    assert_eq!((r.applied, r.quarantined), (0, 1));
    assert_eq!(
        (
            seqs(&c, s, "acc_a").await,
            quarantined(&c, s, "acc_a").await.len(),
            balance(&c, s, "acc_a").await
        ),
        before
    );
    // The local write path is fenced too: the old epoch is refused outright.
    let w = LegWriter::prepare(&c, s).await.unwrap();
    let stale = w
        .post(&Leg {
            tenant: TENANT.parse().unwrap(),
            journal: "jr_late".parse().unwrap(),
            account: "acc_a".parse().unwrap(),
            leg: 1,
            side: Side::Credit,
            amount: Money::new(1, "GBP".parse().unwrap()),
            counterparty: None,
            origin: "aws-lon-01".to_owned(),
            epoch: 1,
            entry: "ent_late".parse().unwrap(),
        })
        .await;
    assert!(matches!(stale, Err(LegError::Fenced(_))), "{stale:?}");
    drop_all(s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn a_returning_stale_home_is_quarantined_on_both_cells_and_the_logs_converge() {
    let s = "r02_failover";
    let (old, new) = (cell(5431, s).await, cell(5432, s).await);
    for n in 1..=5 {
        journal(&old, s, n, 1, "aws-lon-01").await;
    }
    // The standby had replicated up to seq 3 of the old home's log when it was promoted.
    let first3 = old
        .query(
            &format!("select id, journal_id, account_id, epoch, seq, leg, kind, amount, counterparty, origin, created_at::text
                        from {s}.entry where seq <= 3 order by account_id, epoch, seq"),
            &[],
        )
        .await
        .unwrap();
    let entries: Vec<ledger_repl::Entry> = first3
        .iter()
        .map(|r| ledger_repl::Entry {
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
    apply(&new, s, TENANT, None, &entries).await.unwrap();
    let cutoff = high_water_of(&new, s, TENANT, "acc_a").await.unwrap();
    assert_eq!(cutoff, (1, 3));

    // The control plane issues epoch 2 with that cutoff, and the new home starts writing.
    let f = fence(2, "gcp-car-01", cutoff.0, cutoff.1);
    install_fence(&new, s, &f).await.unwrap();
    install_fence(
        &new,
        s,
        &Fence {
            account: "acc_b".to_owned(),
            ..f.clone()
        },
    )
    .await
    .unwrap();
    for n in 6..=7 {
        journal(&new, s, n, 2, "gcp-car-01").await;
    }

    // The old home returns and ships its tail. The new home quarantines it on arrival.
    let r = pull_all(&old, &new, s, TENANT, "aws-lon-01", 50)
        .await
        .unwrap();
    assert_eq!(
        r.applied.quarantined, 4,
        "seq 4 and 5 of both accounts: {r:?}"
    );
    assert_eq!(
        quarantined(&new, s, "acc_a").await,
        [
            (4, "void_on_arrival".to_owned()),
            (5, "void_on_arrival".to_owned())
        ]
    );
    assert_eq!(
        seqs(&new, s, "acc_a").await,
        [(1, 1), (1, 2), (1, 3), (2, 1), (2, 2)]
    );

    // The fence reaches the old home, which sweeps its own tail, then takes the new epoch's entries.
    install_fence(&old, s, &f).await.unwrap();
    install_fence(
        &old,
        s,
        &Fence {
            account: "acc_b".to_owned(),
            ..f.clone()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        quarantined(&old, s, "acc_a").await,
        [
            (4, "void_after_fence".to_owned()),
            (5, "void_after_fence".to_owned())
        ]
    );
    pull_all(&new, &old, s, TENANT, "gcp-car-01", 50)
        .await
        .unwrap();

    // Both cells hold the same log and the same balances, and the quarantine holds the same four.
    assert_eq!(seqs(&old, s, "acc_a").await, seqs(&new, s, "acc_a").await);
    for a in ["acc_a", "acc_b"] {
        assert_eq!(balance(&old, s, a).await, balance(&new, s, a).await, "{a}");
    }
    assert_eq!(
        compare_chains(&[("old", &old), ("new", &new)], s)
            .await
            .unwrap(),
        []
    );
    let tolerant = Options {
        quarantine_max_age_secs: 3600,
        ..Options::default()
    };
    for (name, c) in [("old", &old), ("new", &new)] {
        let v = check_cell(c, s, name, &tolerant).await.unwrap();
        assert!(
            v.iter().all(|x| x.invariant == "journal_unbalanced"),
            "{name}: {v:?}"
        );
    }
    // Strictly, the checker names the quarantined entries that nobody has resolved yet.
    let strict = check_cell(&new, s, "new", &Options::default())
        .await
        .unwrap();
    assert_eq!(
        strict
            .iter()
            .filter(|x| x.invariant == "quarantine")
            .count(),
        4
    );
    drop_all(s).await;
}
