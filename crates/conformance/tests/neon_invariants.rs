//! K-06 against two real Neon cells: `ledgerctl check` verifies the invariants over TLS on both
//! cells (`LEDGER_DSN_USE1`, `LEDGER_DSN_USE2`), exits zero when they hold, and exits non-zero
//! naming the cell and account when one is broken on purpose.
//!
//! It runs only through the `neon-rust` workflow, uses a schema of its own, `inv_neon`, and drops
//! it afterwards. Without both connection strings it does nothing. The connection strings go to
//! `ledgerctl` as environment variables, as in production, and the output must never contain them.

use std::process::Command;
use std::time::Duration;

use ledger_core::{AccountId, Currency, EntryId, JournalId, Money, TenantId};
use ledger_store::{Leg, LegWriter, Side, migrate};
use native_tls::TlsConnector;
use postgres_native_tls::MakeTlsConnector;
use tokio_postgres::Client;

const S: &str = "inv_neon";
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

/// The same two journals on a cell, with the same entry ids, so the cells hold the same log.
async fn setup(dsn: &str) -> Client {
    let mut c = connect(dsn).await;
    c.batch_execute(&format!("drop schema if exists {S} cascade"))
        .await
        .unwrap();
    migrate(&mut c, S).await.unwrap();
    c.batch_execute(&format!(
        "insert into {S}.account (tenant_id, id, kind, currency, normal_side, allow_negative, overdraft_limit) values
           ('{TENANT}', 'acc_a', 'customer', 'GBP', 'credit', false, 0),
           ('{TENANT}', 'acc_b', 'customer', 'GBP', 'credit', false, 0),
           ('{TENANT}', 'acc_cap', 'internal', 'GBP', 'credit', true, 0);
         insert into {S}.epoch_fence (tenant_id, account_id, epoch, home) values
           ('{TENANT}', 'acc_a', 1, 'neon'), ('{TENANT}', 'acc_b', 1, 'neon'), ('{TENANT}', 'acc_cap', 1, 'neon')"
    ))
    .await
    .unwrap();
    let w = LegWriter::prepare(&c, S).await.unwrap();
    for (j, tag, one, two, minor) in [
        (
            "jr_fund",
            "f",
            ("acc_cap", Side::Debit),
            ("acc_a", Side::Credit),
            500,
        ),
        (
            "jr_move",
            "m",
            ("acc_a", Side::Debit),
            ("acc_b", Side::Credit),
            100,
        ),
    ] {
        for (i, (acc, side)) in [one, two].into_iter().enumerate() {
            w.post(&Leg {
                tenant: TENANT.parse::<TenantId>().unwrap(),
                journal: j.parse::<JournalId>().unwrap(),
                account: acc.parse::<AccountId>().unwrap(),
                leg: i as i16 + 1,
                side,
                amount: Money::new(minor, "GBP".parse::<Currency>().unwrap()),
                counterparty: None,
                origin: "neon".to_owned(),
                epoch: 1,
                entry: format!("ent_{tag}{i}").parse::<EntryId>().unwrap(),
            })
            .await
            .unwrap();
        }
    }
    c
}

fn ledgerctl(extra: &[&str], d1: &str, d2: &str) -> (i32, String, String) {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let out = Command::new(cargo)
        .args(["run", "-q", "-p", "ledgerctl", "--", "check"])
        .args([
            "--cell",
            "use1=LEDGER_DSN_USE1",
            "--cell",
            "use2=LEDGER_DSN_USE2",
        ])
        .args(["--schema", S])
        .args(extra)
        .env("LEDGER_DSN_USE1", d1)
        .env("LEDGER_DSN_USE2", d2)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[tokio::test]
#[ignore = "runs through the neon-rust workflow, which holds the connection strings"]
async fn ledgerctl_check_holds_and_fails_on_two_neon_cells() {
    let (Ok(d1), Ok(d2)) = (
        std::env::var("LEDGER_DSN_USE1"),
        std::env::var("LEDGER_DSN_USE2"),
    ) else {
        eprintln!("LEDGER_DSN_USE1 and LEDGER_DSN_USE2 are not both set: nothing to do");
        return;
    };
    let (a, b) = (setup(&d1).await, setup(&d2).await);

    let (code, out, err) = ledgerctl(&["--compare-chains"], &d1, &d2);
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        out.contains("OK: every invariant holds on 2 cell(s)"),
        "{out}"
    );
    println!("PASS healthy: every invariant holds on both Neon cells, hash chains match");

    // Break the projection on the second cell only.
    b.batch_execute(&format!(
        "update {S}.account set balance = balance + 7 where id = 'acc_b'"
    ))
    .await
    .unwrap();
    let (code, out, err) = ledgerctl(&[], &d1, &d2);
    assert_eq!(code, 1, "{out}{err}");
    assert!(
        out.contains("FAIL projection cell=use2 account=acc_b"),
        "{out}"
    );
    assert!(
        !out.contains("use1 account"),
        "the healthy cell is not named: {out}"
    );
    for secret in [&d1, &d2] {
        assert!(!out.contains(secret.as_str()) && !err.contains(secret.as_str()));
    }
    println!("PASS failure: a broken projection exits 1 and names cell use2 and account acc_b");

    // A chain that differs between cells is named too.
    b.batch_execute(&format!(
        "update {S}.account set balance = balance - 7 where id = 'acc_b'"
    ))
    .await
    .unwrap();
    let w = LegWriter::prepare(&a, S).await.unwrap();
    for (i, (acc, side)) in [("acc_a", Side::Debit), ("acc_b", Side::Credit)]
        .into_iter()
        .enumerate()
    {
        w.post(&Leg {
            tenant: TENANT.parse().unwrap(),
            journal: "jr_extra".parse().unwrap(),
            account: acc.parse().unwrap(),
            leg: i as i16 + 1,
            side,
            amount: Money::new(10, "GBP".parse::<Currency>().unwrap()),
            counterparty: None,
            origin: "neon".to_owned(),
            epoch: 1,
            entry: format!("ent_x{i}").parse().unwrap(),
        })
        .await
        .unwrap();
    }
    let (code, out, _) = ledgerctl(&[], &d1, &d2);
    assert_eq!(code, 0, "each cell is healthy by itself: {out}");
    let (code, out, _) = ledgerctl(&["--compare-chains"], &d1, &d2);
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains("FAIL chain_divergence cell=use1,use2 account=acc_a"),
        "{out}"
    );
    println!("PASS chains: cells holding different logs exit 1 and name the account");

    for c in [&a, &b] {
        c.batch_execute(&format!("drop schema if exists {S} cascade"))
            .await
            .unwrap();
    }
    println!("PASS cleanup: schema dropped on both cells");
}
