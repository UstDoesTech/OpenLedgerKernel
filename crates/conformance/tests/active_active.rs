//! T-01: the Rust equivalent of every scenario in `tests/test_active_active.py`, on three real
//! Postgres nodes (ports 5431 to 5433). The Python harness stays the reference. Each test here
//! carries the name of the scenario it matches, and `docs/conformance.md` lists the pairs.
//!
//! The Python harness has one API layer that checks leases before it writes. The Rust kernel
//! has the pieces instead: the control plane decides who is home and whether the lease holds,
//! and the database refuses a writer whose epoch is not the latest. So the lease scenarios use
//! both, and the failover drills run the real orchestrator.
//!
//! Run with `scripts/local_nodes.sh start`, then
//! `cargo test -p conformance --test active_active -- --ignored`.
//! `scripts/mutation_check.sh` shows these tests fail when fencing or the row lock is removed.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use control_plane::{
    Cell, ControlError, ControlPlane, Epoch, InMemoryControlPlane, ManualClock, NodeId,
    Orchestrator, PgCell,
};
use ledger_core::{AccountId, TenantId};
use ledger_repl::strong::acknowledge;
use ledger_repl::{Entry, apply, high_water_of, pull_all};
use ledger_store::cross_cell::{self};
use ledger_store::{LegError, LegWriter, Side};
use tokio_postgres::{Client, NoTls};

mod common;
use common::*;

// 1. test_transfer_is_idempotent
#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn transfer_is_idempotent() {
    let lab = Lab::new("t01_idem", &accounts()).await;
    lab.fund("acc_p", 100).await;
    assert!(
        lab.transfer("jr_t1", "acc_p", "acc_q", 30, &one)
            .await
            .unwrap()
    );
    // A retry, through fresh connections as another region would make, changes nothing.
    assert!(
        !lab.transfer("jr_t1", "acc_p", "acc_q", 30, &one)
            .await
            .unwrap()
    );
    lab.settle().await;
    for c in lab.cells().await {
        assert_eq!(lab.balance(&c, "acc_p").await, 70);
        assert_eq!(lab.balance(&c, "acc_q").await, 30);
    }
    // The same journal with a different amount is refused.
    let err = lab
        .transfer("jr_t1", "acc_p", "acc_q", 31, &one)
        .await
        .unwrap_err();
    assert!(matches!(err, LegError::IdempotencyConflict(_)), "{err:?}");
    lab.assert_healthy(false).await;
    lab.drop_all().await;
}

// 2. test_concurrent_debits_never_overdraw
#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn concurrent_debits_never_overdraw() {
    let lab = Arc::new(Lab::new("t01_race", &accounts()).await);
    lab.fund("acc_p", 100).await;
    let mut tasks = Vec::new();
    for i in 0..20 {
        let lab = lab.clone();
        tasks.push(tokio::spawn(async move {
            lab.transfer(&format!("jr_race{i}"), "acc_p", "acc_q", 10, &one)
                .await
        }));
    }
    let (mut settled, mut declined) = (0, 0);
    for t in tasks {
        match t.await.unwrap() {
            Ok(_) => settled += 1,
            Err(LegError::InsufficientFunds(_)) => declined += 1,
            Err(e) => panic!("{e:?}"),
        }
    }
    assert_eq!((settled, declined), (10, 10));
    lab.settle().await;
    let cells = lab.cells().await;
    assert_eq!(lab.balance(&cells[0], "acc_p").await, 0);
    assert_eq!(lab.balance(&cells[1], "acc_q").await, 100);
    lab.assert_healthy(false).await;
    lab.drop_all().await;
}

// 3. test_only_the_home_with_a_valid_lease_can_debit
#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn only_the_home_with_a_valid_lease_can_debit() {
    let control = InMemoryControlPlane::new(Duration::from_secs(30), Arc::new(ManualClock::new()));
    let p: AccountId = "acc_p".parse().unwrap();
    let (home, other): (NodeId, NodeId) = (NAMES[0].parse().unwrap(), NAMES[1].parse().unwrap());
    control.register(&p, &home).await.unwrap();
    assert!(matches!(
        control.check_lease(&p, &other).await,
        Err(ControlError::NotHome { .. })
    ));
    control.expire(&p).await.unwrap();
    assert!(matches!(
        control.check_lease(&p, &home).await,
        Err(ControlError::LeaseExpired { .. })
    ));
    control.renew(&p).await.unwrap();
    assert_eq!(control.check_lease(&p, &home).await.unwrap(), Epoch(1));

    // The database adds a second line: a writer on the wrong epoch is refused, whatever the
    // control plane says.
    let lab = Lab::new("t01_lease", &accounts()).await;
    lab.fund("acc_p", 100).await;
    let err = lab
        .transfer("jr_stale", "acc_p", "acc_q", 1, &|_| 2)
        .await
        .unwrap_err();
    assert!(matches!(err, LegError::Fenced(_)), "{err:?}");
    lab.transfer("jr_ok", "acc_p", "acc_q", 1, &one)
        .await
        .unwrap();
    lab.drop_all().await;
}

fn entry_of(r: &tokio_postgres::Row) -> Entry {
    Entry {
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
    }
}

// 4. test_replication_is_ordered_idempotent_and_buffers_gaps
#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn replication_is_ordered_idempotent_and_buffers_gaps() {
    let s = "t01_repl";
    let lab = Lab::new(s, &accounts()).await;
    for n in 1..=3 {
        lab.transfer(&format!("jr_r{n}"), "acc_treasury", "acc_p", 1, &one)
            .await
            .unwrap();
    }
    let cells = lab.cells().await;
    let rows = cells[0]
        .query(
            &format!(
                "select id, journal_id, account_id, epoch, seq, leg, kind, amount, counterparty, origin, created_at::text
                   from {s}.entry where account_id = 'acc_treasury' order by seq"
            ),
            &[],
        )
        .await
        .unwrap();
    let e: Vec<Entry> = rows.iter().map(entry_of).collect();
    assert_eq!(e.len(), 3, "one entry per journal on the treasury");
    let dst = &cells[1];
    // Out of order: buffered, not applied.
    let r = apply(dst, s, TENANT, None, &e[1..2]).await.unwrap();
    assert_eq!((r.buffered, r.applied), (1, 0));
    assert!(
        log_of(dst, s, "acc_treasury")
            .await
            .iter()
            .all(|(_, seq, _)| *seq != e[1].seq)
    );
    // The missing first entry arrives and the buffered one drains behind it.
    let r = apply(dst, s, TENANT, None, &e[0..1]).await.unwrap();
    assert_eq!((r.applied, r.drained), (1, 1));
    // Redelivery is harmless.
    let r = apply(dst, s, TENANT, None, &e[1..2]).await.unwrap();
    assert_eq!((r.dup, r.applied), (1, 0));
    apply(dst, s, TENANT, None, &e[2..]).await.unwrap();
    // The relay forgets its position and ships everything again: nothing applies twice.
    dst.batch_execute(&format!("delete from {s}.replication_cursor"))
        .await
        .ok();
    let r = pull_all(&cells[0], dst, s, TENANT, NAMES[0], 50)
        .await
        .unwrap();
    // The three treasury entries are duplicates; the three acc_p entries are new here.
    assert_eq!((r.applied.dup, r.applied.applied), (3, 3));
    assert_eq!(
        log_of(&cells[0], s, "acc_treasury").await,
        log_of(dst, s, "acc_treasury").await
    );
    assert_eq!(
        lab.balance(&cells[0], "acc_treasury").await,
        lab.balance(dst, "acc_treasury").await
    );
    lab.drop_all().await;
}

async fn credits(c: &Client, s: &str) -> i64 {
    c.query_one(
        &format!(
            "select count(*) from {s}.entry where journal_id = 'jr_crash' and account_id = 'acc_q'"
        ),
        &[],
    )
    .await
    .unwrap()
    .get(0)
}

// 5. test_crash_between_legs_is_recovered_exactly_once
#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn crash_between_legs_is_recovered_exactly_once() {
    let s = "t01_crash";
    let lab = Lab::new(s, &accounts()).await;
    lab.fund("acc_p", 100).await;
    let cells = lab.cells().await;
    let t = lab.cross("jr_crash", "acc_p", "acc_q", 40, &one);
    cross_cell::send(&cells[0], s, &t).await.unwrap(); // the coordinator dies after the debit leg
    assert_eq!(credits(&cells[1], s).await, 0);
    let tenant: TenantId = TENANT.parse().unwrap();
    let r = cross_cell::recover(&cells[0], &cells[1], s, &tenant)
        .await
        .unwrap();
    assert_eq!(r.settled, ["jr_crash"]);
    let again = cross_cell::recover(&cells[0], &cells[1], s, &tenant)
        .await
        .unwrap();
    assert_eq!(
        (again.found, again.settled.len()),
        (0, 0),
        "nothing left to do"
    );
    lab.settle().await;
    assert_eq!(credits(&cells[1], s).await, 1);
    lab.assert_healthy(false).await;
    lab.drop_all().await;
}

// 6. test_credit_to_unreachable_home_stays_pending_then_completes
#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn credit_to_unreachable_home_stays_pending_then_completes() {
    let s = "t01_pending";
    let lab = Lab::new(s, &accounts()).await;
    lab.fund("acc_p", 100).await;
    let cells = lab.cells().await;
    let t = lab.cross("jr_pend", "acc_p", "acc_q", 25, &one);
    // The payee's cell is cut off: its answer never comes, so the transfer waits.
    cross_cell::send(&cells[0], s, &t).await.unwrap();
    let cut = Link::start(PORTS[1]).await;
    let (client, conn) = tokio_postgres::Config::new()
        .host("127.0.0.1")
        .port(cut.port)
        .user("ledger")
        .dbname("postgres")
        .connect_timeout(Duration::from_secs(5))
        .connect(NoTls)
        .await
        .unwrap_or_else(|_| panic!("the link accepts before it stalls"));
    tokio::spawn(conn);
    cut.stall(true); // cut after the connection is up, as a partition arrives mid-flight
    let waited = tokio::time::timeout(
        Duration::from_millis(500),
        cross_cell::receive(&client, s, &t),
    )
    .await;
    assert!(
        waited.is_err(),
        "the credit cannot complete across a cut link"
    );
    let tenant: TenantId = TENANT.parse().unwrap();
    let found = cross_cell::unfinished(&cells[0], s, &tenant).await.unwrap();
    assert_eq!(found.len(), 1, "the journal is pending, not lost");
    assert_eq!(lab.balance(&cells[0], "acc_p").await, 75);
    drop(client);
    // The link heals and recovery completes it exactly once.
    let r = cross_cell::recover(&cells[0], &cells[1], s, &tenant)
        .await
        .unwrap();
    assert_eq!(r.settled, ["jr_pend"]);
    lab.settle().await;
    for c in &cells {
        assert_eq!(lab.balance(c, "acc_q").await, 25);
    }
    lab.assert_healthy(false).await;
    lab.drop_all().await;
}

/// A TCP relay that can be cut, as a partitioned link to one cell is.
struct Link {
    port: u16,
    stalled: Arc<std::sync::atomic::AtomicBool>,
}

impl Link {
    async fn start(target: u16) -> Link {
        use std::sync::atomic::Ordering;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let stalled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stalled.clone();
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let server = TcpStream::connect(("127.0.0.1", target)).await.unwrap();
                let (mut cr, mut cw) = client.into_split();
                let (mut sr, mut sw) = server.into_split();
                let (f1, f2) = (flag.clone(), flag.clone());
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 64 * 1024];
                    while let Ok(n) = cr.read(&mut buf).await {
                        while f1.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        if n == 0 || sw.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 64 * 1024];
                    while let Ok(n) = sr.read(&mut buf).await {
                        while f2.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        if n == 0 || cw.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        Link { port, stalled }
    }

    fn stall(&self, on: bool) {
        self.stalled.store(on, std::sync::atomic::Ordering::SeqCst);
    }
}

/// What a failover drill saw.
struct Drill {
    legit_write_on_isolated: &'static str,
    tail_on_home: (i64, i64),
    tail_on_standby: (i64, i64),
    quarantined: BTreeSet<String>,
    zombie_after_rejoin: &'static str,
    balances: [i64; 3],
    moved: i64,
}

/// The scripted drill from `ledger/scenarios.py`: the home (a) dies with acknowledged writes in
/// flight, the standby (b) is promoted by the orchestrator, and the old home comes back still
/// believing it is home. X is homed on a, Y on c.
async fn failover_drill(schema: &str, strong_tail: bool) -> Drill {
    let steady = 5;
    let lab = Lab::new(
        schema,
        &[
            ("acc_treasury", 0, true),
            ("acc_x", 0, false),
            ("acc_y", 2, false),
        ],
    )
    .await;
    let cells = lab.cells().await;
    let node = |i: usize| -> NodeId { NAMES[i].parse().unwrap() };
    let control = InMemoryControlPlane::new(Duration::from_secs(30), Arc::new(ManualClock::new()));
    let x: AccountId = "acc_x".parse().unwrap();
    control.register(&x, &node(0)).await.unwrap();

    lab.fund("acc_x", 1000).await;
    for n in 0..steady {
        lab.transfer(&format!("jr_s{n}"), "acc_x", "acc_y", 1, &one)
            .await
            .unwrap();
    }
    lab.settle().await;

    for n in 0..2 {
        let j = format!("jr_tail{n}");
        lab.transfer(&j, "acc_x", "acc_y", 1, &one).await.unwrap();
        if strong_tail {
            // Acknowledged to the client only once the standby holds it.
            let ids: Vec<String> = cells[0]
                .query(
                    &format!("select id from {schema}.entry where account_id = 'acc_x' and journal_id = $1"),
                    &[&j],
                )
                .await
                .unwrap()
                .iter()
                .map(|r| r.get(0))
                .collect();
            acknowledge(
                &cells[0],
                &cells[1],
                schema,
                TENANT,
                NAMES[0],
                &ids,
                Duration::from_secs(5),
            )
            .await
            .unwrap();
        }
    }
    let tail_on_home = high_water_of(&cells[0], schema, TENANT, "acc_x")
        .await
        .unwrap();
    let tail_on_standby = high_water_of(&cells[1], schema, TENANT, "acc_x")
        .await
        .unwrap();

    // The home is cut off. It cannot renew, so even a well-behaved writer refuses.
    control.expire(&x).await.unwrap();
    let legit = match control.check_lease(&x, &node(0)).await {
        Err(ControlError::LeaseExpired { .. }) => "LeaseExpired",
        Ok(_) => "allowed",
        Err(_) => "other",
    };

    // The orchestrator promotes b. Cell a is unreachable, so it gets no fence.
    let reachable = [
        (
            node(1),
            PgCell::new(Arc::new(connect(PORTS[1]).await), schema, TENANT),
        ),
        (
            node(2),
            PgCell::new(Arc::new(connect(PORTS[2]).await), schema, TENANT),
        ),
    ];
    let orchestrator = Orchestrator::new(
        control,
        reachable.into_iter().collect(),
        Duration::from_secs(5),
    );
    let done = orchestrator.promote(&x, &node(1)).await.unwrap();
    assert_eq!(done.fence.epoch, Epoch(2));
    assert_eq!(done.fence.prev_cutoff_seq as i64, tail_on_standby.1);
    let epochs = |a: &str| if a == "acc_x" { 2 } else { 1 };
    // The new home, b, takes X from here. It writes its own two transfers at epoch 2.
    let b_lab = lab_with_home(&lab, "acc_x", 1);
    for n in 0..2 {
        b_lab
            .transfer(&format!("jr_new{n}"), "acc_x", "acc_y", 1, &epochs)
            .await
            .unwrap();
    }

    // The zombie, a, ignores its lease and writes X at epoch 1: the database on a has not
    // seen the fence, so it accepts the entry, which is exactly what the sweep must undo.
    let w = LegWriter::prepare(&cells[0], schema).await.unwrap();
    w.post(&lab.leg("jr_zombie", "acc_x", 1, Side::Debit, 5, (NAMES[0], 1)))
        .await
        .unwrap();

    // a rejoins: the fence reaches it, and the logs are shipped both ways.
    let pg_a = PgCell::new(Arc::new(connect(PORTS[0]).await), schema, TENANT);
    for f in orchestrator.control().fences().await.unwrap() {
        pg_a.install_fence(&f).await.unwrap();
    }
    lab.settle().await;

    let mut quarantined = BTreeSet::new();
    for c in &cells {
        for r in c
            .query(
                &format!("select id from {schema}.quarantine where account_id = 'acc_x'"),
                &[],
            )
            .await
            .unwrap()
        {
            quarantined.insert(r.get::<_, String>(0));
        }
    }
    let zombie_after_rejoin = match lab.transfer("jr_late", "acc_x", "acc_y", 1, &one).await {
        Err(LegError::Fenced(_)) => "Fenced",
        Ok(_) => "allowed",
        Err(_) => "other",
    };

    // Every cell holds the same log for X, and the same balance.
    let base = log_of(&cells[0], schema, "acc_x").await;
    for c in &cells[1..] {
        assert_eq!(log_of(c, schema, "acc_x").await, base);
    }

    // Reconciliation, as design.md describes it, then complete the zombie's credit.
    repost_quarantined(&lab, &cells, "acc_x", 1, 2).await;
    let wc = LegWriter::prepare(&cells[2], schema).await.unwrap();
    wc.post(&lab.leg("jr_zombie", "acc_y", 2, Side::Credit, 5, (NAMES[2], 1)))
        .await
        .unwrap();
    lab.settle().await;

    let mut balances = [0; 3];
    for (i, a) in ["acc_treasury", "acc_x", "acc_y"].into_iter().enumerate() {
        let first = lab.balance(&cells[0], a).await;
        for c in &cells[1..] {
            assert_eq!(lab.balance(c, a).await, first, "{a} agrees on every cell");
        }
        balances[i] = first;
    }
    lab.assert_healthy(true).await;
    lab.drop_all().await;
    Drill {
        legit_write_on_isolated: legit,
        tail_on_home,
        tail_on_standby,
        quarantined,
        zombie_after_rejoin,
        balances,
        moved: steady + 2 + 2 + 5,
    }
}

// 7. test_failover_async_tail_is_quarantined_then_reconciled
#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn failover_async_tail_is_quarantined_then_reconciled() {
    let d = failover_drill("t01_drill_async", false).await;
    assert_eq!(d.legit_write_on_isolated, "LeaseExpired");
    assert!(
        d.tail_on_standby < d.tail_on_home,
        "async: the standby is behind, so RPO > 0"
    );
    // Two acknowledged but unreplicated debits plus the zombie's write, nothing silently merged.
    assert_eq!(d.quarantined.len(), 3, "{:?}", d.quarantined);
    assert_eq!(d.zombie_after_rejoin, "Fenced");
    assert_eq!(d.balances, [-1000, 1000 - d.moved, d.moved]);
}

// 8. test_failover_with_strong_tier_loses_nothing
#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn failover_with_strong_tier_loses_nothing() {
    let d = failover_drill("t01_drill_strong", true).await;
    assert_eq!(d.tail_on_standby, d.tail_on_home, "strong: RPO 0");
    assert_eq!(
        d.quarantined.len(),
        1,
        "only the zombie's write: {:?}",
        d.quarantined
    );
    assert_eq!(d.zombie_after_rejoin, "Fenced");
    assert_eq!(d.balances, [-1000, 1000 - d.moved, d.moved]);
}

// 9. test_random_concurrent_workload_converges
#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn random_concurrent_workload_converges() {
    let names: Vec<String> = (0..6).map(|i| format!("acc_w{i}")).collect();
    let mut accts: Vec<(&str, usize, bool)> = vec![("acc_treasury", 0, true)];
    for (i, n) in names.iter().enumerate() {
        accts.push((n.as_str(), i % 3, false));
    }
    let lab = Arc::new(Lab::new("t01_random", &accts).await);
    for n in &names {
        lab.fund(n, 1000).await;
    }
    let expected = Arc::new(tokio::sync::Mutex::new(
        names
            .iter()
            .map(|n| (n.clone(), 1000_i64))
            .collect::<std::collections::BTreeMap<_, _>>(),
    ));
    let mut tasks = Vec::new();
    for seed in 0..12u64 {
        let (lab, names, expected) = (lab.clone(), names.clone(), expected.clone());
        tasks.push(tokio::spawn(async move {
            // A small deterministic generator, so a failing run can be repeated.
            let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
            let mut next = move |n: u64| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state % n
            };
            for k in 0..15 {
                let src = next(6) as usize;
                let dst = (src + 1 + next(5) as usize) % 6;
                let amt = 1 + next(40) as i64;
                let j = format!("jr_rw{seed}x{k}");
                match lab.transfer(&j, &names[src], &names[dst], amt, &one).await {
                    Ok(_) => {
                        let mut e = expected.lock().await;
                        *e.get_mut(&names[src]).unwrap() -= amt;
                        *e.get_mut(&names[dst]).unwrap() += amt;
                    }
                    Err(LegError::InsufficientFunds(_)) => {}
                    Err(e) => panic!("{e:?}"),
                }
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    lab.settle().await;
    let expected = expected.lock().await.clone();
    for (i, c) in lab.cells().await.iter().enumerate() {
        for n in &names {
            assert_eq!(lab.balance(c, n).await, expected[n], "{n} on {}", NAMES[i]);
        }
    }
    lab.assert_healthy(false).await;
    lab.drop_all().await;
}

// 10. test_healthy_home_keeps_its_lease_through_idle_periods
#[tokio::test]
#[ignore = "needs Postgres on ports 5431-5433"]
async fn healthy_home_keeps_its_lease_through_idle_periods() {
    use control_plane::renew_homed;
    let clock = Arc::new(ManualClock::new());
    let ttl = Duration::from_millis(1500);
    let control = InMemoryControlPlane::new(ttl, clock.clone());
    let p: AccountId = "acc_p".parse().unwrap();
    let a: NodeId = NAMES[0].parse().unwrap();
    control.register(&p, &a).await.unwrap();
    // Idle for well past the ttl, but the home heartbeats every half ttl.
    for _ in 0..8 {
        clock.advance(ttl / 2);
        assert_eq!(renew_homed(&control, &a).await.unwrap(), 1);
    }
    assert!(
        control.check_lease(&p, &a).await.is_ok(),
        "the heartbeat kept the lease alive"
    );
    // A lease expired on purpose is not revived by the heartbeat.
    control.expire(&p).await.unwrap();
    clock.advance(ttl);
    assert_eq!(renew_homed(&control, &a).await.unwrap(), 0);
    assert!(matches!(
        control.check_lease(&p, &a).await,
        Err(ControlError::LeaseExpired { .. })
    ));
    // And a home that cannot reach the control plane stops renewing and loses it.
    control.renew(&p).await.unwrap();
    clock.advance(ttl * 2);
    assert!(matches!(
        control.check_lease(&p, &a).await,
        Err(ControlError::LeaseExpired { .. })
    ));
}
