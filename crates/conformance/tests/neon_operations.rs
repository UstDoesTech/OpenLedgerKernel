//! Every ledger operation of the kernel against a real Neon cell (`LEDGER_DSN_USE1`): posting
//! legs with the funds check, the epoch fence and idempotency (K-03), and holds, available
//! balance, conversion and expiry (K-05). Cross-cell journals (K-04) run in `neon_cross_cell`.
//!
//! It runs only through the `neon-rust` workflow, uses a schema of its own, `ops_neon`, and
//! drops it afterwards. Without the connection string it does nothing. Each check prints a
//! PASS line, which is what the workflow publishes as the proof.

use std::time::Duration;

use ledger_core::{AccountId, Currency, EntryId, HoldId, JournalId, Money, TenantId};
use ledger_store::holds::{self, Conversion, HoldState, NewHold};
use ledger_store::{Leg, LegError, LegWriter, Side, migrate};
use native_tls::TlsConnector;
use postgres_native_tls::MakeTlsConnector;
use tokio_postgres::Client;

const S: &str = "ops_neon";
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

fn gbp(minor: i64) -> Money {
    Money::new(minor, "GBP".parse::<Currency>().unwrap())
}

fn tenant() -> TenantId {
    TENANT.parse().unwrap()
}

fn account(id: &str) -> AccountId {
    id.parse().unwrap()
}

#[allow(clippy::too_many_arguments)]
async fn post(
    w: &LegWriter<'_>,
    acc: &str,
    side: Side,
    minor: i64,
    journal: &str,
    entry: &str,
    epoch: i64,
) -> Result<ledger_store::Posted, LegError> {
    w.post(&Leg {
        tenant: tenant(),
        journal: journal.parse::<JournalId>().unwrap(),
        account: account(acc),
        leg: 1,
        side,
        amount: gbp(minor),
        counterparty: None,
        origin: "neon-use1".to_owned(),
        epoch,
        entry: entry.parse::<EntryId>().unwrap(),
    })
    .await
}

fn hold(id: &str, acc: &str, minor: i64) -> NewHold {
    NewHold {
        tenant: tenant(),
        hold: id.parse::<HoldId>().unwrap(),
        account: account(acc),
        amount: gbp(minor),
        command: None,
        expires_at: None,
        epoch: 1,
    }
}

async fn avail(c: &Client, acc: &str) -> i64 {
    holds::available(c, S, &tenant(), &account(acc))
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "runs through the neon-rust workflow, which holds the connection strings"]
async fn every_ledger_operation_holds_on_neon() {
    let Ok(dsn) = std::env::var("LEDGER_DSN_USE1") else {
        eprintln!("LEDGER_DSN_USE1 is not set: nothing to do");
        return;
    };
    let mut c = connect(&dsn).await;
    c.batch_execute(&format!("drop schema if exists {S} cascade"))
        .await
        .unwrap();
    migrate(&mut c, S).await.unwrap();
    println!(
        "PASS migrate: {} migrations applied",
        ledger_store::MIGRATIONS.len()
    );
    c.batch_execute(&format!(
        "insert into {S}.account (tenant_id, id, kind, currency, normal_side, allow_negative, overdraft_limit) values
           ('{TENANT}', 'acc_a', 'customer', 'GBP', 'credit', false, 0),
           ('{TENANT}', 'acc_b', 'customer', 'GBP', 'credit', false, 200);
         insert into {S}.epoch_fence (tenant_id, account_id, epoch, home) values
           ('{TENANT}', 'acc_a', 1, 'neon-use1'), ('{TENANT}', 'acc_b', 1, 'neon-use1');"
    ))
    .await
    .unwrap();
    let w = LegWriter::prepare(&c, S).await.unwrap();

    // Legs: credit, debit, funds, fence, idempotency.
    let credit = post(&w, "acc_a", Side::Credit, 1_000, "jr_fund", "ent_fund", 1)
        .await
        .unwrap();
    assert_eq!(
        (credit.balance, credit.seq, credit.replayed),
        (1_000, 1, false)
    );
    let debit = post(&w, "acc_a", Side::Debit, 300, "jr_d1", "ent_d1", 1)
        .await
        .unwrap();
    assert_eq!((debit.balance, debit.seq), (700, 2));
    assert!(debit.watermark > credit.watermark);
    println!("PASS legs: a credit and a debit post in sequence and advance the watermark");
    let err = post(&w, "acc_a", Side::Debit, 701, "jr_d2", "ent_d2", 1)
        .await
        .unwrap_err();
    assert!(matches!(err, LegError::InsufficientFunds(_)), "{err}");
    println!("PASS funds: a debit above the available balance is refused");
    let err = post(&w, "acc_a", Side::Debit, 1, "jr_d3", "ent_d3", 9)
        .await
        .unwrap_err();
    assert!(matches!(err, LegError::Fenced(_)), "{err}");
    println!("PASS fence: a stale epoch is refused");
    let again = post(&w, "acc_a", Side::Debit, 300, "jr_d1", "ent_other", 1)
        .await
        .unwrap();
    assert!(again.replayed && again.entry == debit.entry);
    let err = post(&w, "acc_a", Side::Debit, 301, "jr_d1", "ent_other", 1)
        .await
        .unwrap_err();
    assert!(matches!(err, LegError::IdempotencyConflict(_)), "{err}");
    println!("PASS idempotency: a repeat returns the first entry and a changed repeat is refused");
    let n: i64 = c
        .query_one(&format!("select count(*) from {S}.entry"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 2, "refused legs wrote nothing");
    let err = c
        .batch_execute(&format!("update {S}.entry set amount = 1"))
        .await
        .unwrap_err();
    let message = err
        .as_db_error()
        .map(|d| d.message().to_owned())
        .unwrap_or_default();
    assert!(message.contains("append-only"), "{err:?}");
    println!("PASS append-only: an entry cannot be updated");

    // Holds: available balance, place, release, convert, expire.
    assert_eq!(avail(&c, "acc_b").await, 200);
    holds::place(&c, S, &hold("hld_a", "acc_a", 400))
        .await
        .unwrap();
    assert_eq!(avail(&c, "acc_a").await, 300);
    let err = post(&w, "acc_a", Side::Debit, 301, "jr_d4", "ent_d4", 1)
        .await
        .unwrap_err();
    assert!(matches!(err, LegError::InsufficientFunds(_)), "{err}");
    assert!(
        holds::place(&c, S, &hold("hld_a", "acc_a", 400))
            .await
            .unwrap()
            .replayed
    );
    println!(
        "PASS available: posted minus active holds plus overdraft, and debits authorise against it"
    );

    let id: HoldId = "hld_a".parse().unwrap();
    assert_eq!(
        holds::release(&c, S, &tenant(), &id, 1)
            .await
            .unwrap()
            .state,
        HoldState::Released
    );
    assert!(
        holds::release(&c, S, &tenant(), &id, 1)
            .await
            .unwrap()
            .replayed
    );
    assert_eq!(avail(&c, "acc_a").await, 700);
    println!("PASS release: idempotent, and it frees what it reserved");

    holds::place(&c, S, &hold("hld_b", "acc_a", 700))
        .await
        .unwrap();
    let conv = Conversion {
        tenant: tenant(),
        hold: "hld_b".parse().unwrap(),
        journal: "jr_card".parse().unwrap(),
        amount: None,
        counterparty: None,
        origin: "neon-use1".to_owned(),
        epoch: 1,
        entry: "ent_card".parse().unwrap(),
    };
    let (state, first) = holds::convert(&c, S, &conv).await.unwrap();
    assert_eq!((state, first.balance), (HoldState::Converted, 0));
    let (_, repeat) = holds::convert(&c, S, &conv).await.unwrap();
    assert!(repeat.replayed);
    println!("PASS convert: the hold becomes its debit once, and never blocks itself");

    let mut lapsing = hold("hld_c", "acc_b", 100);
    lapsing.expires_at = Some("2026-01-01T00:00:00Z".to_owned());
    holds::place(&c, S, &lapsing).await.unwrap();
    holds::place(&c, S, &hold("hld_d", "acc_b", 50))
        .await
        .unwrap();
    let swept = holds::expire(&c, S, &tenant(), "2026-06-01T00:00:00Z")
        .await
        .unwrap();
    assert_eq!(swept, ["hld_c"]);
    assert!(
        holds::expire(&c, S, &tenant(), "2026-06-01T00:00:00Z")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(avail(&c, "acc_b").await, 150);
    println!("PASS expire: only the lapsed hold ended, and a repeat sweep does nothing");

    let (e, b): (i64, i64) = {
        let r = c
            .query_one(
                &format!("select coalesce(sum(amount),0)::bigint, (select coalesce(sum(balance),0)::bigint from {S}.account) from {S}.entry"),
                &[],
            )
            .await
            .unwrap();
        (r.get(0), r.get(1))
    };
    assert_eq!(e, b, "balances are the sum of entries");
    println!("PASS projection: every balance equals the sum of its entries");

    c.batch_execute(&format!("drop schema if exists {S} cascade"))
        .await
        .unwrap();
}
