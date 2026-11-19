//! K-06: the invariant checker. Each invariant is broken on purpose against real Postgres and
//! the checker must name the cell and the account or journal. `ledgerctl check` must exit
//! non-zero for a failure and zero for a healthy ledger.
//!
//! Run with `make up` (or `scripts/local_nodes.sh start`), then
//! `cargo test -p conformance --test invariants -- --ignored`.

use std::process::Command;
use std::time::Duration;

use ledger_check::{Options, Violation, check_cell, compare_chains};
use ledger_core::{AccountId, Currency, EntryId, JournalId, Money, TenantId};
use ledger_store::cross_cell::{self, Transfer};
use ledger_store::{Leg, LegWriter, Side, migrate};
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

fn gbp(minor: i64) -> Money {
    Money::new(minor, "GBP".parse::<Currency>().unwrap())
}

/// A cell with accounts `acc_a` and `acc_b`, and a balanced journal moving 100 from a to b.
async fn cell(port: u16, schema: &str) -> Client {
    let mut c = connect(port).await;
    c.batch_execute(&format!("drop schema if exists {schema} cascade"))
        .await
        .unwrap();
    migrate(&mut c, schema).await.unwrap();
    c.batch_execute(&format!(
        "insert into {schema}.account (tenant_id, id, kind, currency, normal_side, allow_negative, overdraft_limit) values
           ('{TENANT}', 'acc_a', 'customer', 'GBP', 'credit', false, 0),
           ('{TENANT}', 'acc_b', 'customer', 'GBP', 'credit', false, 0),
           ('{TENANT}', 'acc_cap', 'internal', 'GBP', 'credit', true, 0);
         insert into {schema}.epoch_fence (tenant_id, account_id, epoch, home) values
           ('{TENANT}', 'acc_a', 1, 'aws-lon-01'), ('{TENANT}', 'acc_b', 1, 'aws-lon-01'), ('{TENANT}', 'acc_cap', 1, 'aws-lon-01')"
    ))
    .await
    .unwrap();
    // Fund a from the capital account, then move 100 from a to b: two balanced journals.
    two_legs(
        &c,
        schema,
        "jr_fund",
        ("acc_cap", Side::Debit),
        ("acc_a", Side::Credit),
        500,
        "f",
    )
    .await;
    two_legs(
        &c,
        schema,
        "jr_move",
        ("acc_a", Side::Debit),
        ("acc_b", Side::Credit),
        100,
        "m",
    )
    .await;
    c
}

async fn two_legs(
    c: &Client,
    schema: &str,
    journal: &str,
    one: (&str, Side),
    two: (&str, Side),
    minor: i64,
    tag: &str,
) {
    let w = LegWriter::prepare(c, schema).await.unwrap();
    for (i, (acc, side)) in [one, two].into_iter().enumerate() {
        w.post(&Leg {
            tenant: TENANT.parse::<TenantId>().unwrap(),
            journal: journal.parse::<JournalId>().unwrap(),
            account: acc.parse::<AccountId>().unwrap(),
            leg: i as i16 + 1,
            side,
            amount: gbp(minor),
            counterparty: None,
            origin: "aws-lon-01".to_owned(),
            epoch: 1,
            entry: format!("ent_{tag}{i}").parse::<EntryId>().unwrap(),
        })
        .await
        .unwrap();
    }
}

async fn check(c: &Client, schema: &str, cell: &str) -> Vec<Violation> {
    check_cell(c, schema, cell, &Options::default())
        .await
        .unwrap()
}

fn names(v: &[Violation], invariant: &str, subject: &str) -> bool {
    v.iter()
        .any(|x| x.invariant == invariant && x.subject == subject && x.cell == "use1")
}

async fn drop_schema(c: &Client, schema: &str) {
    c.batch_execute(&format!("drop schema if exists {schema} cascade"))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn a_healthy_ledger_has_no_violations() {
    let s = "k06_healthy";
    let c = cell(5431, s).await;
    assert_eq!(check(&c, s, "use1").await, []);
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn each_broken_invariant_names_the_cell_and_the_account_or_journal() {
    let s = "k06_broken";
    let c = cell(5431, s).await;

    // Projection: the balance no longer matches the entries.
    c.batch_execute(&format!(
        "update {s}.account set balance = balance + 7 where id = 'acc_b'"
    ))
    .await
    .unwrap();
    let v = check(&c, s, "use1").await;
    assert!(names(&v, "projection", "account=acc_b"), "{v:?}");
    assert!(
        v.iter()
            .any(|x| x.invariant == "trial_balance" && x.detail.contains("balances")),
        "{v:?}"
    );
    c.batch_execute(&format!(
        "update {s}.account set balance = balance - 7 where id = 'acc_b'"
    ))
    .await
    .unwrap();
    assert_eq!(check(&c, s, "use1").await, []);

    // A sequence hole, and a journal that does not net to zero (a lone entry, balance kept in step).
    c.batch_execute(&format!(
        "insert into {s}.entry (tenant_id, id, journal_id, account_id, epoch, seq, leg, kind, amount, origin)
           values ('{TENANT}', 'ent_hole', 'jr_lone', 'acc_b', 1, 9, 1, 'credit', 25, 'aws-lon-01');
         update {s}.account set balance = balance + 25 where id = 'acc_b'"
    ))
    .await
    .unwrap();
    let v = check(&c, s, "use1").await;
    assert!(names(&v, "sequence_gap", "account=acc_b"), "{v:?}");
    assert!(names(&v, "journal_unbalanced", "journal=jr_lone"), "{v:?}");
    assert!(
        v.iter().any(|x| x.invariant == "trial_balance"
            && x.subject == "tenant=ten_acme"
            && x.detail.contains("entries")),
        "{v:?}"
    );
    assert!(
        v.iter()
            .any(|x| x.invariant == "trial_balance" && x.detail.contains("balances")),
        "{v:?}"
    );
    drop_schema(&c, s).await;

    // Overdrawn: an account without an allowance below zero.
    let c = cell(5431, s).await;
    c.batch_execute(&format!(
        "update {s}.account set balance = -1 where id = 'acc_b'"
    ))
    .await
    .unwrap();
    assert!(names(
        &check(&c, s, "use1").await,
        "overdrawn",
        "account=acc_b"
    ));
    drop_schema(&c, s).await;

    // Quarantine: unresolved is a failure unless the limit allows its age.
    let c = cell(5431, s).await;
    c.batch_execute(&format!(
        "insert into {s}.quarantine (tenant_id, id, journal_id, account_id, epoch, seq, kind, amount, origin, created_at, reason)
           values ('{TENANT}', 'ent_q', 'jr_q', 'acc_a', 1, 99, 'credit', 5, 'aws-lon-01', now(), 'void_after_fence')"
    ))
    .await
    .unwrap();
    assert!(names(
        &check(&c, s, "use1").await,
        "quarantine",
        "account=acc_a"
    ));
    let tolerant = Options {
        quarantine_max_age_secs: 3600,
        ..Options::default()
    };
    assert_eq!(check_cell(&c, s, "use1", &tolerant).await.unwrap(), []);
    c.batch_execute(&format!("update {s}.quarantine set resolved = true"))
        .await
        .unwrap();
    assert_eq!(check(&c, s, "use1").await, []);
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn trial_balance_holds_across_cells_and_in_transit_age_names_the_stuck_journal() {
    let s = "k06_transit";
    let (a, b) = (cell(5431, s).await, cell(5432, s).await);
    for c in [&a, &b] {
        c.batch_execute(&format!("insert into {s}.account (tenant_id, id, kind, currency, normal_side, allow_negative, overdraft_limit)
            values ('{TENANT}', 'acc_p', 'customer', 'GBP', 'credit', false, 1000), ('{TENANT}', 'acc_q', 'customer', 'GBP', 'credit', false, 1000);
            insert into {s}.epoch_fence (tenant_id, account_id, epoch, home) values ('{TENANT}', 'acc_p', 1, 'aws-lon-01'), ('{TENANT}', 'acc_q', 1, 'aws-lon-01')"))
            .await
            .unwrap();
        c.execute(&format!("select {s}.register_in_transit($1,'aws-lon-01','gcp-car-01','GBP','acc_tsend','acc_trecv')"), &[&TENANT]).await.unwrap();
        c.batch_execute(&format!("insert into {s}.epoch_fence (tenant_id, account_id, epoch, home) values ('{TENANT}', 'acc_tsend', 1, 'aws-lon-01'), ('{TENANT}', 'acc_trecv', 1, 'gcp-car-01')")).await.unwrap();
    }
    let t = |j: &str| Transfer {
        tenant: TENANT.parse().unwrap(),
        journal: j.parse().unwrap(),
        actor: "usr_ada".to_owned(),
        payer: "acc_p".parse().unwrap(),
        payee: "acc_q".parse().unwrap(),
        sending: "acc_tsend".parse().unwrap(),
        receiving: "acc_trecv".parse().unwrap(),
        amount: gbp(40),
        payer_cell: "aws-lon-01".to_owned(),
        payee_cell: "gcp-car-01".to_owned(),
        payer_epoch: 1,
        sending_epoch: 1,
        receiving_epoch: 1,
        payee_epoch: 1,
    };
    cross_cell::transfer(&a, &b, s, &t("jr_done"))
        .await
        .unwrap();
    // Mid-flight: the first half only. Both cells still balance, and it is not yet too old.
    cross_cell::send(&a, s, &t("jr_stuck")).await.unwrap();
    assert_eq!(check(&a, s, "use1").await, []);
    assert_eq!(check(&b, s, "use2").await, []);
    // With no allowance for age, the stuck journal is named, at the payer's cell.
    let strict = Options {
        in_transit_max_age_secs: 0,
        ..Options::default()
    };
    let v = check_cell(&a, s, "use1", &strict).await.unwrap();
    assert!(names(&v, "in_transit_age", "journal=jr_stuck"), "{v:?}");
    assert!(!v.iter().any(|x| x.subject == "journal=jr_done"), "{v:?}");
    // Recovery drains it, and the check passes again.
    cross_cell::recover(&a, &b, s, &TENANT.parse().unwrap())
        .await
        .unwrap();
    assert_eq!(check_cell(&a, s, "use1", &strict).await.unwrap(), []);
    drop_schema(&a, s).await;
    drop_schema(&b, s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn hash_chains_that_differ_between_cells_name_the_account() {
    let s = "k06_chains";
    let (a, b) = (cell(5431, s).await, cell(5432, s).await);
    // Both cells ran the same two journals with the same entry ids, so the chains match.
    let cells = [("use1", &a), ("use2", &b)];
    assert_eq!(compare_chains(&cells, s).await.unwrap(), []);
    // One cell gains an entry the other lacks.
    two_legs(
        &a,
        s,
        "jr_more",
        ("acc_a", Side::Debit),
        ("acc_b", Side::Credit),
        10,
        "x",
    )
    .await;
    let v = compare_chains(&cells, s).await.unwrap();
    let accounts: Vec<&str> = v.iter().map(|x| x.subject.as_str()).collect();
    assert_eq!(accounts, ["account=acc_a", "account=acc_b"], "{v:?}");
    assert!(
        v.iter()
            .all(|x| x.invariant == "chain_divergence" && x.cell == "use1,use2")
    );
    drop_schema(&a, s).await;
    drop_schema(&b, s).await;
}

fn ledgerctl(args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let mut cmd = Command::new(cargo);
    cmd.args(["run", "-q", "-p", "ledgerctl", "--", "check"])
        .args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.env_remove("LEDGER_CLI_UNSET");
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn ledgerctl_check_exits_non_zero_and_names_the_account_or_cell() {
    let s = "k06_cli";
    let c = cell(5431, s).await;
    let dsn = "postgres://ledger@127.0.0.1:5431/postgres?sslmode=disable";
    let env = [("LEDGER_CLI_DSN", dsn)];
    let base = ["--cell", "use1=LEDGER_CLI_DSN", "--schema", s];

    let (code, out, _) = ledgerctl(&base, &env);
    assert_eq!(
        (code, out.trim()),
        (0, "OK: every invariant holds on 1 cell(s)")
    );

    c.batch_execute(&format!(
        "update {s}.account set balance = balance + 7 where id = 'acc_b'"
    ))
    .await
    .unwrap();
    let (code, out, _) = ledgerctl(&base, &env);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("FAIL projection cell=use1 account=acc_b"),
        "{out}"
    );
    assert!(out.contains("FAILED:"), "{out}");
    assert!(
        !out.contains("127.0.0.1"),
        "the connection string must not be printed"
    );

    // A check that could not run is not a pass: a missing variable and a bad cell are exit 2.
    let (code, _, err) = ledgerctl(&["--cell", "use1=LEDGER_CLI_UNSET", "--schema", s], &[]);
    assert_eq!(code, 2);
    assert!(
        err.contains("use1") && err.contains("LEDGER_CLI_UNSET"),
        "{err}"
    );
    let (code, _, err) = ledgerctl(&["--cell", &format!("use1={dsn}")], &[]);
    assert_eq!(code, 2);
    assert!(!err.contains("127.0.0.1"), "{err}");
    drop_schema(&c, s).await;
}
