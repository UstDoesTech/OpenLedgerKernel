//! K-03: posting a leg is one SQL call, and the database itself enforces the rules: funds, the
//! epoch fence and idempotency. Everything here runs against real Postgres, never a mock.
//!
//! Run with `make up` (or `scripts/local_nodes.sh start`), then
//! `cargo test -p conformance --test ledger_leg -- --ignored`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

use ledger_core::{AccountId, Currency, EntryId, JournalId, Money, TenantId};
use ledger_store::{Leg, LegError, LegWriter, Side, migrate};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_postgres::{Client, NoTls};

const NODES: [u16; 3] = [5431, 5432, 5433];
const TENANT: &str = "ten_acme";
const OTHER_TENANT: &str = "ten_zed";
const ORIGIN: &str = "aws-lon-01";

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

/// A migrated schema of its own on the first node, so tests do not disturb each other.
async fn fresh(schema: &str) -> Client {
    let mut c = connect_to(NODES[0]).await;
    c.batch_execute(&format!("drop schema if exists {schema} cascade"))
        .await
        .unwrap();
    migrate(&mut c, schema).await.unwrap();
    c
}

async fn drop_schema(c: &Client, schema: &str) {
    c.batch_execute(&format!("drop schema if exists {schema} cascade"))
        .await
        .unwrap();
}

async fn account(c: &Client, schema: &str, tenant: &str, id: &str, extra: &str) {
    c.batch_execute(&format!(
        "insert into {schema}.account (tenant_id, id, kind, currency, normal_side {}) \
         values ('{tenant}', '{id}', 'customer', 'GBP', 'credit' {})",
        if extra.is_empty() {
            ""
        } else {
            ", allow_negative, overdraft_limit"
        },
        if extra.is_empty() {
            String::new()
        } else {
            format!(", {extra}")
        },
    ))
    .await
    .unwrap();
}

async fn fence(
    c: &Client,
    schema: &str,
    tenant: &str,
    id: &str,
    epoch: i64,
    prev: i64,
    cutoff: i64,
) {
    c.batch_execute(&format!(
        "insert into {schema}.epoch_fence (tenant_id, account_id, epoch, home, prev_epoch, prev_cutoff_seq) \
         values ('{tenant}', '{id}', {epoch}, 'aws-lon-01', {prev}, {cutoff})"
    ))
    .await
    .unwrap();
}

fn gbp(minor: i64) -> Money {
    Money::new(minor, "GBP".parse::<Currency>().unwrap())
}

fn leg(
    tenant: &str,
    account: &str,
    journal: &str,
    side: Side,
    minor: i64,
    epoch: i64,
    entry: &str,
) -> Leg {
    Leg {
        tenant: tenant.parse::<TenantId>().unwrap(),
        journal: journal.parse::<JournalId>().unwrap(),
        account: account.parse::<AccountId>().unwrap(),
        leg: 1,
        side,
        amount: gbp(minor),
        counterparty: None,
        origin: ORIGIN.to_owned(),
        epoch,
        entry: entry.parse::<EntryId>().unwrap(),
    }
}

async fn balance(c: &Client, schema: &str, tenant: &str, id: &str) -> i64 {
    c.query_one(
        &format!("select balance from {schema}.account where tenant_id = $1 and id = $2"),
        &[&tenant, &id],
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

/// A TCP proxy that counts round trips: each time traffic turns from the server back to the
/// client and then the client speaks again, that is another round trip.
struct CountingProxy {
    port: u16,
    turns: Arc<AtomicUsize>,
}

const NOBODY: u8 = 0;
const CLIENT: u8 = 1;
const SERVER: u8 = 2;

impl CountingProxy {
    async fn start(target: u16) -> CountingProxy {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let turns = Arc::new(AtomicUsize::new(0));
        let counted = turns.clone();
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let server = TcpStream::connect(("127.0.0.1", target)).await.unwrap();
                let (mut cr, mut cw) = client.into_split();
                let (mut sr, mut sw) = server.into_split();
                let last = Arc::new(AtomicU8::new(NOBODY));
                let (turns_up, last_up) = (counted.clone(), last.clone());
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 16 * 1024];
                    while let Ok(n) = cr.read(&mut buf).await {
                        if n == 0 || sw.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                        if last_up.swap(CLIENT, Ordering::SeqCst) != CLIENT {
                            turns_up.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                });
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 16 * 1024];
                    while let Ok(n) = sr.read(&mut buf).await {
                        if n == 0 || cw.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                        last.store(SERVER, Ordering::SeqCst);
                    }
                });
            }
        });
        CountingProxy { port, turns }
    }

    fn turns(&self) -> usize {
        self.turns.load(Ordering::SeqCst)
    }
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn posting_a_leg_is_one_round_trip_and_returns_the_watermark() {
    let s = "k03_round_trip";
    let admin = fresh(s).await;
    account(&admin, s, TENANT, "acc_a", "").await;
    fence(&admin, s, TENANT, "acc_a", 1, 0, 0).await;

    let proxy = CountingProxy::start(NODES[0]).await;
    let c = connect_to(proxy.port).await;
    let writer = LegWriter::prepare(&c, s).await.unwrap();
    // Let the connection settle: preparing is its own trip, made once, not for every leg.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Calibrate the counter: two separate statements are two round trips.
    let before = proxy.turns();
    c.batch_execute("select 1").await.unwrap();
    c.batch_execute("select 2").await.unwrap();
    assert_eq!(
        proxy.turns() - before,
        2,
        "the counter must see each round trip"
    );

    let before = proxy.turns();
    let first = writer
        .post(&leg(TENANT, "acc_a", "jr_1", Side::Credit, 100, 1, "ent_1"))
        .await
        .unwrap();
    assert_eq!(proxy.turns() - before, 1, "one leg must be one round trip");

    let before = proxy.turns();
    let second = writer
        .post(&leg(TENANT, "acc_a", "jr_2", Side::Debit, 30, 1, "ent_2"))
        .await
        .unwrap();
    assert_eq!(proxy.turns() - before, 1);

    assert_eq!((first.seq, first.balance, first.replayed), (1, 100, false));
    assert_eq!(
        (second.seq, second.balance, second.replayed),
        (2, 70, false)
    );
    assert!(second.watermark > first.watermark, "the watermark moves on");

    // The watermark is the outbox position of the entry just written.
    let position: i64 = admin
        .query_one(
            &format!("select position from {s}.outbox where entry_id = 'ent_2'"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(second.watermark, position);
    drop_schema(&admin, s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn twenty_concurrent_debits_of_ten_against_one_hundred_from_three_clients_yield_exactly_ten()
{
    let s = "k03_concurrent";
    let admin = fresh(s).await;
    account(&admin, s, TENANT, "acc_a", "").await;
    fence(&admin, s, TENANT, "acc_a", 1, 0, 0).await;
    admin
        .batch_execute(&format!(
            "update {s}.account set balance = 100 where id = 'acc_a'"
        ))
        .await
        .unwrap();

    let clients: Vec<Arc<Client>> = vec![
        Arc::new(connect_to(NODES[0]).await),
        Arc::new(connect_to(NODES[0]).await),
        Arc::new(connect_to(NODES[0]).await),
    ];
    let mut tasks = Vec::new();
    for i in 0..20 {
        let client = clients[i % 3].clone();
        let schema = s.to_owned();
        tasks.push(tokio::spawn(async move {
            let writer = LegWriter::prepare(&client, &schema).await.unwrap();
            writer
                .post(&leg(
                    TENANT,
                    "acc_a",
                    &format!("jr_d{i}"),
                    Side::Debit,
                    10,
                    1,
                    &format!("ent_d{i}"),
                ))
                .await
        }));
    }
    let mut ok = 0;
    let mut refused = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(p) => {
                assert!(p.balance >= 0);
                ok += 1;
            }
            Err(LegError::InsufficientFunds(_)) => refused += 1,
            Err(e) => panic!("unexpected: {e}"),
        }
    }
    assert_eq!((ok, refused), (10, 10));
    assert_eq!(balance(&admin, s, TENANT, "acc_a").await, 0);
    assert_eq!(count(&admin, s, "entry").await, 10);
    assert_eq!(count(&admin, s, "outbox").await, 10);

    // Sequence numbers are contiguous from 1: the account lock made the writers take turns.
    let seqs: Vec<i64> = admin
        .query(&format!("select seq from {s}.entry order by seq"), &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert_eq!(seqs, (1..=10).collect::<Vec<i64>>());
    drop_schema(&admin, s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn reusing_a_journal_with_a_different_amount_is_rejected_and_the_same_amount_returns_the_original()
 {
    let s = "k03_idempotent";
    let c = fresh(s).await;
    account(&c, s, TENANT, "acc_a", "").await;
    fence(&c, s, TENANT, "acc_a", 1, 0, 0).await;
    let writer = LegWriter::prepare(&c, s).await.unwrap();

    let original = writer
        .post(&leg(
            TENANT,
            "acc_a",
            "jr_x",
            Side::Credit,
            500,
            1,
            "ent_x1",
        ))
        .await
        .unwrap();
    assert!(!original.replayed);

    // The same leg again, even with a new entry id: the original entry comes back.
    let again = writer
        .post(&leg(
            TENANT,
            "acc_a",
            "jr_x",
            Side::Credit,
            500,
            1,
            "ent_x2",
        ))
        .await
        .unwrap();
    assert!(again.replayed);
    assert_eq!(again.entry, original.entry);
    assert_eq!(
        (again.seq, again.watermark),
        (original.seq, original.watermark)
    );
    assert_eq!(count(&c, s, "entry").await, 1);
    assert_eq!(count(&c, s, "outbox").await, 1);
    assert_eq!(balance(&c, s, TENANT, "acc_a").await, 500);

    // A different amount, or the other side, is refused and changes nothing.
    for (side, amount) in [(Side::Credit, 501), (Side::Credit, 499), (Side::Debit, 500)] {
        let e = writer
            .post(&leg(TENANT, "acc_a", "jr_x", side, amount, 1, "ent_x3"))
            .await
            .unwrap_err();
        assert!(
            matches!(e, LegError::IdempotencyConflict(_)),
            "{side:?} {amount}: {e}"
        );
    }
    assert_eq!(count(&c, s, "entry").await, 1);
    assert_eq!(balance(&c, s, TENANT, "acc_a").await, 500);

    // Another leg of the same journal is a different entry.
    let mut second = leg(TENANT, "acc_a", "jr_x", Side::Credit, 7, 1, "ent_x4");
    second.leg = 2;
    assert!(!writer.post(&second).await.unwrap().replayed);
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn a_writer_holding_a_stale_epoch_is_rejected_by_the_database() {
    let s = "k03_fence";
    let c = fresh(s).await;
    account(&c, s, TENANT, "acc_a", "").await;
    let writer = LegWriter::prepare(&c, s).await.unwrap();

    // No fence yet: the account has no home, so nobody may write.
    let e = writer
        .post(&leg(TENANT, "acc_a", "jr_0", Side::Credit, 10, 1, "ent_0"))
        .await
        .unwrap_err();
    assert!(matches!(e, LegError::Fenced(_)), "{e}");

    fence(&c, s, TENANT, "acc_a", 1, 0, 0).await;
    writer
        .post(&leg(TENANT, "acc_a", "jr_1", Side::Credit, 100, 1, "ent_1"))
        .await
        .unwrap();

    // The control plane moves the account's home: epoch 2.
    fence(&c, s, TENANT, "acc_a", 2, 1, 1).await;
    let stale = writer
        .post(&leg(TENANT, "acc_a", "jr_2", Side::Credit, 5, 1, "ent_2"))
        .await
        .unwrap_err();
    assert!(matches!(stale, LegError::Fenced(_)), "{stale}");
    // An epoch the database has not heard of is refused too: it never guesses.
    let ahead = writer
        .post(&leg(TENANT, "acc_a", "jr_3", Side::Credit, 5, 3, "ent_3"))
        .await
        .unwrap_err();
    assert!(matches!(ahead, LegError::Fenced(_)), "{ahead}");
    assert_eq!(count(&c, s, "entry").await, 1);

    // The new home writes at epoch 2, and its sequence starts again at 1.
    let fresh_home = writer
        .post(&leg(TENANT, "acc_a", "jr_4", Side::Credit, 5, 2, "ent_4"))
        .await
        .unwrap();
    assert_eq!((fresh_home.epoch, fresh_home.seq), (2, 1));
    // The fence applies on a retry of a stale write too: a repeat of the epoch-1 leg still
    // returns the original, because a repeat writes nothing.
    let repeat = writer
        .post(&leg(TENANT, "acc_a", "jr_1", Side::Credit, 100, 1, "ent_9"))
        .await
        .unwrap();
    assert!(repeat.replayed);
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn funds_overdraft_currency_and_unknown_accounts_are_checked() {
    let s = "k03_checks";
    let c = fresh(s).await;
    account(&c, s, TENANT, "acc_a", "false, 50").await; // overdraft of 50
    account(&c, s, TENANT, "acc_b", "true, 0").await; // may go negative without limit
    account(&c, s, TENANT, "acc_c", "").await;
    for a in ["acc_a", "acc_b", "acc_c"] {
        fence(&c, s, TENANT, a, 1, 0, 0).await;
    }
    let writer = LegWriter::prepare(&c, s).await.unwrap();

    // acc_a: 0 balance, overdraft 50: a debit of 50 is allowed, 51 is not.
    writer
        .post(&leg(TENANT, "acc_a", "jr_a1", Side::Debit, 50, 1, "ent_a1"))
        .await
        .unwrap();
    let e = writer
        .post(&leg(TENANT, "acc_a", "jr_a2", Side::Debit, 1, 1, "ent_a2"))
        .await
        .unwrap_err();
    assert!(matches!(e, LegError::InsufficientFunds(_)), "{e}");
    assert_eq!(balance(&c, s, TENANT, "acc_a").await, -50);

    // acc_b has no limit.
    writer
        .post(&leg(
            TENANT,
            "acc_b",
            "jr_b1",
            Side::Debit,
            1_000_000,
            1,
            "ent_b1",
        ))
        .await
        .unwrap();

    // acc_c has nothing to spend.
    let e = writer
        .post(&leg(TENANT, "acc_c", "jr_c1", Side::Debit, 1, 1, "ent_c1"))
        .await
        .unwrap_err();
    assert!(matches!(e, LegError::InsufficientFunds(_)), "{e}");

    // Credits are never refused for funds.
    writer
        .post(&leg(TENANT, "acc_c", "jr_c2", Side::Credit, 1, 1, "ent_c2"))
        .await
        .unwrap();

    let e = writer
        .post(&leg(
            TENANT,
            "acc_nobody",
            "jr_n",
            Side::Credit,
            1,
            1,
            "ent_n",
        ))
        .await
        .unwrap_err();
    assert!(matches!(e, LegError::UnknownAccount(_)), "{e}");

    let mut dollars = leg(TENANT, "acc_c", "jr_usd", Side::Credit, 1, 1, "ent_usd");
    dollars.amount = Money::new(1, "USD".parse::<Currency>().unwrap());
    assert!(matches!(
        writer.post(&dollars).await.unwrap_err(),
        LegError::CurrencyMismatch(_)
    ));

    for minor in [0, -5] {
        let e = writer
            .post(&leg(
                TENANT,
                "acc_c",
                "jr_z",
                Side::Credit,
                minor,
                1,
                "ent_z",
            ))
            .await
            .unwrap_err();
        assert!(matches!(e, LegError::NotPositive), "{minor}");
    }
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn the_balance_is_the_sum_of_entries_and_the_outbox_matches_the_log() {
    let s = "k03_projection";
    let c = fresh(s).await;
    account(&c, s, TENANT, "acc_a", "").await;
    fence(&c, s, TENANT, "acc_a", 1, 0, 0).await;
    let writer = LegWriter::prepare(&c, s).await.unwrap();
    for i in 0..30 {
        let side = if i % 3 == 2 {
            Side::Debit
        } else {
            Side::Credit
        };
        writer
            .post(&leg(
                TENANT,
                "acc_a",
                &format!("jr_p{i}"),
                side,
                10 + i,
                1,
                &format!("ent_p{i}"),
            ))
            .await
            .unwrap();
    }
    let sum: i64 = c
        .query_one(&format!("select sum(amount)::bigint from {s}.entry"), &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(balance(&c, s, TENANT, "acc_a").await, sum);

    // Every entry has exactly one outbox row, in the same order as (epoch, seq).
    let mismatched: i64 = c
        .query_one(
            &format!(
                "select count(*) from {s}.entry e left join {s}.outbox o using (tenant_id, account_id, epoch, seq) \
                 where o.entry_id is distinct from e.id"
            ),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(mismatched, 0);
    let in_order: bool = c
        .query_one(
            &format!(
                "select coalesce(bool_and(position > lag_pos), true) from \
                 (select position, lag(position) over (order by seq) as lag_pos from {s}.outbox) x where lag_pos is not null"
            ),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(in_order);
    let next_seq: i64 = c
        .query_one(
            &format!("select next_seq from {s}.account where id = 'acc_a'"),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(next_seq, 31);
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn tenants_never_share_an_account_or_a_journal() {
    let s = "k03_tenants";
    let c = fresh(s).await;
    for t in [TENANT, OTHER_TENANT] {
        account(&c, s, t, "acc_a", "").await;
        fence(&c, s, t, "acc_a", 1, 0, 0).await;
    }
    let writer = LegWriter::prepare(&c, s).await.unwrap();
    // The same account id and journal id in two tenants are two different things.
    let one = writer
        .post(&leg(
            TENANT,
            "acc_a",
            "jr_same",
            Side::Credit,
            100,
            1,
            "ent_t1",
        ))
        .await
        .unwrap();
    let two = writer
        .post(&leg(
            OTHER_TENANT,
            "acc_a",
            "jr_same",
            Side::Credit,
            7,
            1,
            "ent_t2",
        ))
        .await
        .unwrap();
    assert!(!one.replayed && !two.replayed);
    assert_eq!(balance(&c, s, TENANT, "acc_a").await, 100);
    assert_eq!(balance(&c, s, OTHER_TENANT, "acc_a").await, 7);
    drop_schema(&c, s).await;
}

#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn a_failed_leg_writes_nothing() {
    let s = "k03_atomic";
    let c = fresh(s).await;
    account(&c, s, TENANT, "acc_a", "").await;
    fence(&c, s, TENANT, "acc_a", 1, 0, 0).await;
    let writer = LegWriter::prepare(&c, s).await.unwrap();
    writer
        .post(&leg(TENANT, "acc_a", "jr_1", Side::Credit, 10, 1, "ent_1"))
        .await
        .unwrap();

    // The same entry id for a different journal fails inside the statement, after the checks
    // have passed: the entry, the balance and the outbox must all stay as they were.
    let e = writer
        .post(&leg(TENANT, "acc_a", "jr_2", Side::Credit, 10, 1, "ent_1"))
        .await
        .unwrap_err();
    assert!(matches!(e, LegError::Database(_)), "{e}");
    assert_eq!(count(&c, s, "entry").await, 1);
    assert_eq!(count(&c, s, "outbox").await, 1);
    assert_eq!(balance(&c, s, TENANT, "acc_a").await, 10);
    drop_schema(&c, s).await;
}
