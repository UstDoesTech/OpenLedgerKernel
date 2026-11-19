//! K-05: holds and available balance. Everything runs against real Postgres, never a mock.
//!
//! Run with `make up` (or `scripts/local_nodes.sh start`), then
//! `cargo test -p conformance --test holds -- --ignored`.

use std::time::Duration;

use ledger_core::{AccountId, CommandId, Currency, EntryId, HoldId, JournalId, Money, TenantId};
use ledger_store::holds::{self, Conversion, HoldState, NewHold};
use ledger_store::{Leg, LegError, LegWriter, Side, migrate};
use tokio_postgres::{Client, NoTls};

const TENANT: &str = "ten_acme";
const ACC: &str = "acc_card";

async fn connect() -> Client {
    let (client, conn) = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(5431)
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

fn tenant() -> TenantId {
    TENANT.parse().unwrap()
}

fn acc() -> AccountId {
    ACC.parse().unwrap()
}

/// A migrated schema with one account that has `funds` posted and an overdraft of `overdraft`.
async fn account_with(schema: &str, funds: i64, overdraft: i64) -> Client {
    let mut c = connect().await;
    c.batch_execute(&format!("drop schema if exists {schema} cascade"))
        .await
        .unwrap();
    migrate(&mut c, schema).await.unwrap();
    c.batch_execute(&format!(
        "insert into {schema}.account (tenant_id, id, kind, currency, normal_side, allow_negative, overdraft_limit)
           values ('{TENANT}', '{ACC}', 'customer', 'GBP', 'credit', false, {overdraft});
         insert into {schema}.epoch_fence (tenant_id, account_id, epoch, home) values ('{TENANT}', '{ACC}', 1, 'aws-lon-01')"
    ))
    .await
    .unwrap();
    if funds > 0 {
        post(&c, schema, Side::Credit, funds, "jr_fund", "ent_fund")
            .await
            .unwrap();
    }
    c
}

async fn post(
    c: &Client,
    schema: &str,
    side: Side,
    minor: i64,
    journal: &str,
    entry: &str,
) -> Result<(), LegError> {
    let w = LegWriter::prepare(c, schema).await?;
    w.post(&Leg {
        tenant: tenant(),
        journal: journal.parse::<JournalId>().unwrap(),
        account: acc(),
        leg: 1,
        side,
        amount: gbp(minor),
        counterparty: None,
        origin: "aws-lon-01".to_owned(),
        epoch: 1,
        entry: entry.parse::<EntryId>().unwrap(),
    })
    .await
    .map(|_| ())
}

fn hold(id: &str, minor: i64) -> NewHold {
    NewHold {
        tenant: tenant(),
        hold: id.parse::<HoldId>().unwrap(),
        account: acc(),
        amount: gbp(minor),
        command: None,
        expires_at: None,
        epoch: 1,
    }
}

async fn posted(c: &Client, s: &str) -> i64 {
    c.query_one(
        &format!("select balance from {s}.account where id = '{ACC}'"),
        &[],
    )
    .await
    .unwrap()
    .get(0)
}

async fn available(c: &Client, s: &str) -> i64 {
    holds::available(c, s, &tenant(), &acc()).await.unwrap()
}

async fn drop_schema(c: &Client, s: &str) {
    c.batch_execute(&format!("drop schema if exists {s} cascade"))
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn available_balance_is_posted_minus_active_holds_plus_overdraft() {
    let s = "k05_available";
    let c = account_with(s, 1_000, 200).await;
    assert_eq!(available(&c, s).await, 1_200);

    holds::place(&c, s, &hold("hld_a", 300)).await.unwrap();
    holds::place(&c, s, &hold("hld_b", 150)).await.unwrap();
    assert_eq!(
        posted(&c, s).await,
        1_000,
        "a hold does not move the posted balance"
    );
    assert_eq!(available(&c, s).await, 1_200 - 450);

    // A hold the available balance cannot cover is declined and leaves nothing behind.
    let err = holds::place(&c, s, &hold("hld_big", 751))
        .await
        .unwrap_err();
    assert!(matches!(err, LegError::InsufficientFunds(_)), "{err}");
    let n: i64 = c
        .query_one(&format!("select count(*) from {s}.hold"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 2);
    // Exactly what is left can be held.
    holds::place(&c, s, &hold("hld_edge", 750)).await.unwrap();
    assert_eq!(available(&c, s).await, 0);
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn every_debit_authorises_against_available_balance() {
    let s = "k05_debit";
    let c = account_with(s, 1_000, 200).await;
    holds::place(&c, s, &hold("hld_a", 900)).await.unwrap();
    // Posted is 1000 and overdraft 200, but 900 is held: 300 is available.
    let err = post(&c, s, Side::Debit, 301, "jr_over", "ent_over")
        .await
        .unwrap_err();
    assert!(matches!(err, LegError::InsufficientFunds(_)), "{err}");
    post(&c, s, Side::Debit, 300, "jr_ok", "ent_ok")
        .await
        .unwrap();
    assert_eq!(posted(&c, s).await, 700);
    assert_eq!(available(&c, s).await, 0);
    // Releasing the hold frees what it reserved.
    holds::release(&c, s, &tenant(), &"hld_a".parse().unwrap(), 1)
        .await
        .unwrap();
    assert_eq!(available(&c, s).await, 900);
    post(&c, s, Side::Debit, 900, "jr_more", "ent_more")
        .await
        .unwrap();
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn placing_a_hold_is_idempotent_by_id_and_by_command() {
    let s = "k05_place";
    let c = account_with(s, 1_000, 0).await;
    let mut h = hold("hld_a", 400);
    h.command = Some("cmd_one".parse::<CommandId>().unwrap());
    let first = holds::place(&c, s, &h).await.unwrap();
    assert!(!first.replayed);
    let again = holds::place(&c, s, &h).await.unwrap();
    assert!(again.replayed);
    assert_eq!(
        available(&c, s).await,
        600,
        "a repeat does not reserve twice"
    );

    // The same command under another hold id is a conflict, not a second hold.
    let mut other = hold("hld_b", 400);
    other.command = h.command.clone();
    assert!(matches!(
        holds::place(&c, s, &other).await.unwrap_err(),
        LegError::IdempotencyConflict(_)
    ));
    // The same hold with another amount is a conflict too.
    assert!(matches!(
        holds::place(&c, s, &hold("hld_a", 401)).await.unwrap_err(),
        LegError::IdempotencyConflict(_)
    ));
    // A stale epoch cannot place a hold.
    let mut stale = hold("hld_c", 1);
    stale.epoch = 9;
    assert!(matches!(
        holds::place(&c, s, &stale).await.unwrap_err(),
        LegError::Fenced(_)
    ));
    assert_eq!(available(&c, s).await, 600);
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn releasing_is_idempotent_and_refused_once_the_hold_ended_another_way() {
    let s = "k05_release";
    let c = account_with(s, 1_000, 0).await;
    holds::place(&c, s, &hold("hld_a", 400)).await.unwrap();
    let id: HoldId = "hld_a".parse().unwrap();
    let first = holds::release(&c, s, &tenant(), &id, 1).await.unwrap();
    assert_eq!((first.state, first.replayed), (HoldState::Released, false));
    let again = holds::release(&c, s, &tenant(), &id, 1).await.unwrap();
    assert_eq!((again.state, again.replayed), (HoldState::Released, true));
    assert_eq!(available(&c, s).await, 1_000);

    assert!(matches!(
        holds::release(&c, s, &tenant(), &"hld_nope".parse().unwrap(), 1)
            .await
            .unwrap_err(),
        LegError::UnknownHold(_)
    ));
    // A released hold cannot then be converted.
    let conv = Conversion {
        tenant: tenant(),
        hold: id,
        journal: "jr_x".parse().unwrap(),
        amount: None,
        counterparty: None,
        origin: "aws-lon-01".to_owned(),
        epoch: 1,
        entry: "ent_x".parse().unwrap(),
    };
    assert!(matches!(
        holds::convert(&c, s, &conv).await.unwrap_err(),
        LegError::HoldState(_)
    ));
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn converting_a_hold_posts_its_debit_once_and_never_blocks_itself() {
    let s = "k05_convert";
    // Exactly the held amount is posted: the debit must not count its own hold against itself.
    let c = account_with(s, 500, 0).await;
    holds::place(&c, s, &hold("hld_a", 500)).await.unwrap();
    let conv = Conversion {
        tenant: tenant(),
        hold: "hld_a".parse().unwrap(),
        journal: "jr_pay".parse().unwrap(),
        amount: None,
        counterparty: Some("acc_shop".parse().unwrap()),
        origin: "aws-lon-01".to_owned(),
        epoch: 1,
        entry: "ent_pay".parse().unwrap(),
    };
    let (state, posted_leg) = holds::convert(&c, s, &conv).await.unwrap();
    assert_eq!(
        (state, posted_leg.replayed, posted_leg.balance),
        (HoldState::Converted, false, 0)
    );
    assert_eq!(available(&c, s).await, 0);

    // Repeating it writes nothing more.
    let (state, again) = holds::convert(&c, s, &conv).await.unwrap();
    assert_eq!(
        (state, again.replayed, again.entry.clone()),
        (HoldState::Converted, true, posted_leg.entry.clone())
    );
    let n: i64 = c
        .query_one(
            &format!("select count(*) from {s}.entry where journal_id = 'jr_pay'"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
    let journal: String = c
        .query_one(
            &format!("select journal_id from {s}.hold where id = 'hld_a'"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(journal, "jr_pay");

    // Another journal cannot take a converted hold, and it cannot be released.
    let mut other = conv.clone();
    other.journal = "jr_other".parse().unwrap();
    assert!(matches!(
        holds::convert(&c, s, &other).await.unwrap_err(),
        LegError::HoldState(_)
    ));
    assert!(matches!(
        holds::release(&c, s, &tenant(), &"hld_a".parse().unwrap(), 1)
            .await
            .unwrap_err(),
        LegError::HoldState(_)
    ));
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn a_refused_conversion_leaves_the_hold_active() {
    let s = "k05_refused";
    let c = account_with(s, 500, 0).await;
    holds::place(&c, s, &hold("hld_a", 500)).await.unwrap();
    let conv = Conversion {
        tenant: tenant(),
        hold: "hld_a".parse().unwrap(),
        journal: "jr_big".parse().unwrap(),
        amount: Some(501), // clearing for more than the account holds
        counterparty: None,
        origin: "aws-lon-01".to_owned(),
        epoch: 1,
        entry: "ent_big".parse().unwrap(),
    };
    assert!(matches!(
        holds::convert(&c, s, &conv).await.unwrap_err(),
        LegError::InsufficientFunds(_)
    ));
    let state: String = c
        .query_one(
            &format!("select state from {s}.hold where id = 'hld_a'"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(state, "active");
    assert_eq!(available(&c, s).await, 0);
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn expiry_ends_only_lapsed_active_holds_and_is_idempotent() {
    let s = "k05_expire";
    let c = account_with(s, 1_000, 0).await;
    let mut lapsed = hold("hld_old", 100);
    lapsed.expires_at = Some("2026-11-14T09:00:00Z".to_owned());
    let mut later = hold("hld_new", 200);
    later.expires_at = Some("2026-11-14T18:00:00Z".to_owned());
    holds::place(&c, s, &lapsed).await.unwrap();
    holds::place(&c, s, &later).await.unwrap();
    holds::place(&c, s, &hold("hld_never", 300)).await.unwrap();
    // One lapsed hold is released before the sweep, so the sweep must leave it alone.
    let mut gone = hold("hld_gone", 50);
    gone.expires_at = Some("2026-11-14T08:00:00Z".to_owned());
    holds::place(&c, s, &gone).await.unwrap();
    holds::release(&c, s, &tenant(), &"hld_gone".parse().unwrap(), 1)
        .await
        .unwrap();
    assert_eq!(available(&c, s).await, 1_000 - 600);

    let first = holds::expire(&c, s, &tenant(), "2026-11-14T12:00:00Z")
        .await
        .unwrap();
    assert_eq!(first, ["hld_old"]);
    assert_eq!(available(&c, s).await, 1_000 - 500);
    assert!(
        holds::expire(&c, s, &tenant(), "2026-11-14T12:00:00Z")
            .await
            .unwrap()
            .is_empty()
    );
    let states: Vec<(String, String)> = c
        .query(&format!("select id, state from {s}.hold order by id"), &[])
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect();
    let expect = [
        ("hld_gone", "released"),
        ("hld_never", "active"),
        ("hld_new", "active"),
        ("hld_old", "expired"),
    ];
    assert_eq!(states, expect.map(|(a, b)| (a.to_owned(), b.to_owned())));
    // An expired hold cannot be converted or released.
    assert!(matches!(
        holds::release(&c, s, &tenant(), &"hld_old".parse().unwrap(), 1)
            .await
            .unwrap_err(),
        LegError::HoldState(_)
    ));
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn concurrent_holds_and_debits_never_overspend_available_balance() {
    let s = "k05_race";
    let c = account_with(s, 1_000, 0).await;
    drop(c);
    let mut tasks = Vec::new();
    for i in 0..40usize {
        tasks.push(tokio::spawn(async move {
            let c = connect().await;
            if i % 2 == 0 {
                holds::place(&c, s, &hold(&format!("hld_r{i}"), 100))
                    .await
                    .is_ok()
            } else {
                post(
                    &c,
                    s,
                    Side::Debit,
                    100,
                    &format!("jr_r{i}"),
                    &format!("ent_r{i}"),
                )
                .await
                .is_ok()
            }
        }));
    }
    let mut ok = 0;
    for t in tasks {
        ok += usize::from(t.await.unwrap());
    }
    let c = connect().await;
    assert_eq!(
        ok, 10,
        "1000 pays for exactly ten of 100, however they race"
    );
    assert!(posted(&c, s).await >= 0);
    assert_eq!(available(&c, s).await, 0);
    drop_schema(&c, s).await;
}
