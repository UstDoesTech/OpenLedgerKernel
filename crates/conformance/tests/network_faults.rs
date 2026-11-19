//! T-02: real network faults. Toxiproxy sits in front of every Postgres node (15431 to 15433)
//! and etcd (12379), and these scenarios cut links, slow them and drop connections while real
//! cells and a real etcd do the work. Every scenario ends by settling and running the invariant
//! check on every cell.
//!
//! Peer traffic (cross-cell transfers, replication, the orchestrator's fences) goes through the
//! proxies. A node's own clients, and the checks, go direct, so a cut link leaves a cell
//! reachable to the people next to it, as a partition does.
//!
//! Needs `make up` (or the same ports another way) and the `etcd` feature:
//! `cargo test -p conformance --features etcd --test network_faults -- --ignored`.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use control_plane::{
    ControlError, ControlPlane, Epoch, EtcdConfig, EtcdControlPlane, FailoverError, GuardConfig,
    GuardError, LeaseGuard, NodeId, Orchestrator, PgCell, SystemClock,
};
use ledger_core::{AccountId, TenantId};
use ledger_store::cross_cell;
use ledger_store::{LegError, LegWriter, Side};
use tokio::sync::{Mutex, MutexGuard};

const ETCD: &str = "http://127.0.0.1:2379";
const ETCD_PROXIED: &str = "http://127.0.0.1:12379";

/// Toxiproxy holds one set of faults for everyone, so scenarios take turns.
static ENVIRONMENT: Mutex<()> = Mutex::const_new(());

async fn exclusive() -> MutexGuard<'static, ()> {
    let guard = ENVIRONMENT.lock().await;
    let (status, _) = http("POST", "/reset", "").await;
    assert_eq!(status, 204, "toxiproxy reset");
    wait_for_links().await;
    guard
}

async fn control(url: &str, prefix: &str, ttl: Duration) -> EtcdControlPlane {
    let mut config = EtcdConfig::new(vec![url.to_owned()], prefix, ttl);
    config.request_timeout = Duration::from_secs(1);
    EtcdControlPlane::connect(config)
        .await
        .unwrap_or_else(|e| panic!("etcd at {url}: {e}"))
}

fn prefix(scenario: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("/control-plane-test/t02-{scenario}-{nanos}")
}

fn node(i: usize) -> NodeId {
    NAMES[i].parse().unwrap()
}

fn acc(s: &str) -> AccountId {
    s.parse().unwrap()
}

/// What a client of a cell would see when it is cut: no connection, or a stalled one.
async fn unreachable(port: u16) -> bool {
    tokio::time::timeout(Duration::from_secs(2), try_connect(port))
        .await
        .map_or(true, |r| r.is_err())
}

fn drill_accounts() -> Vec<(&'static str, usize, bool)> {
    vec![
        ("acc_treasury", 0, true),
        ("acc_x", 0, false),
        ("acc_y", 2, false),
    ]
}

// Scenario 1: cut a home from its peers and the control plane.
#[tokio::test]
#[ignore = "needs `make up` and the etcd feature"]
async fn a_home_cut_from_its_peers_and_the_control_plane_is_replaced_and_comes_back_fenced() {
    let _env = exclusive().await;
    let schema = "t02_isolate";
    let ttl = Duration::from_secs(2);
    let pfx = prefix("isolate");
    let lab = Lab::new(schema, &drill_accounts()).await;
    let peers = lab.via(PROXIED);
    let direct_cp = control(ETCD, &pfx, ttl).await;
    let homes_cp = control(ETCD_PROXIED, &pfx, ttl).await;
    let x = acc("acc_x");
    direct_cp.register(&x, &node(0)).await.unwrap();

    lab.fund("acc_x", 1000).await;
    for n in 0..5 {
        peers
            .transfer(&format!("jr_s{n}"), "acc_x", "acc_y", 1, &one)
            .await
            .unwrap();
    }
    peers.settle().await;
    // Two acknowledged writes the standby never sees: the tail.
    for n in 0..2 {
        lab.transfer(&format!("jr_tail{n}"), "acc_x", "acc_y", 1, &one)
            .await
            .unwrap();
    }

    // The orchestrator's connections to every cell are made before the cut, as a running
    // operator's would be. Cutting the home's proxy breaks the one to the home.
    let mut cells = std::collections::BTreeMap::new();
    for (i, port) in PROXIED.into_iter().enumerate() {
        cells.insert(
            node(i),
            PgCell::new(Arc::new(connect(port).await), schema, TENANT),
        );
    }

    cut("pg1").await;
    cut("etcd").await;
    assert!(
        unreachable(PROXIED[0]).await,
        "the home must be cut from its peers"
    );
    assert!(
        matches!(
            homes_cp.check_lease(&x, &node(0)).await,
            Err(ControlError::Unavailable(_))
        ),
        "from the home, the control plane is unavailable, which is not the same as a lost lease"
    );

    // The home cannot renew, so its lease lapses at etcd within the ttl.
    tokio::time::sleep(ttl + Duration::from_secs(2)).await;
    assert!(matches!(
        direct_cp.check_lease(&x, &node(0)).await,
        Err(ControlError::LeaseExpired { .. })
    ));

    // The rest promote azure. The cut home is named, not waited for.
    let orchestrator = Orchestrator::new(direct_cp, cells, Duration::from_secs(5));
    let started = Instant::now();
    let done = orchestrator.promote(&x, &node(1)).await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(done.fence.epoch, Epoch(2));
    assert_eq!(
        done.not_installed
            .iter()
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>(),
        [node(0)],
        "only the cut home missed the fence"
    );
    let new_home = lab_with_home(&lab, "acc_x", 1).via(PROXIED);
    let at_epoch_2 = |a: &str| if a == "acc_x" { 2 } else { 1 };
    for n in 0..2 {
        new_home
            .transfer(&format!("jr_new{n}"), "acc_x", "acc_y", 1, &at_epoch_2)
            .await
            .unwrap();
    }

    // The isolated home, unaware, takes a write at its old epoch. Its own database has not seen
    // the fence, so it accepts it. That is what the sweep must undo.
    let direct = lab.cells().await;
    let zombie = LegWriter::prepare(&direct[0], schema).await.unwrap();
    zombie
        .post(&lab.leg("jr_zombie", "acc_x", 1, Side::Debit, 5, (NAMES[0], 1)))
        .await
        .unwrap();

    heal().await;
    // The home rejoins: the control plane's fences reach it, then every cell ships to every other.
    let a_cell = PgCell::new(Arc::new(connect(PROXIED[0]).await), schema, TENANT);
    for f in orchestrator.control().fences().await.unwrap() {
        control_plane::Cell::install_fence(&a_cell, &f)
            .await
            .unwrap();
    }
    peers.settle().await;

    let late = lab.transfer("jr_late", "acc_x", "acc_y", 1, &one).await;
    assert!(
        matches!(late, Err(LegError::Fenced(_))),
        "the old home is fenced: {late:?}"
    );
    let base = log_of(&direct[0], schema, "acc_x").await;
    for c in &direct[1..] {
        assert_eq!(
            log_of(c, schema, "acc_x").await,
            base,
            "one log for X on every cell"
        );
    }
    // Reconcile as the design says, complete the zombie's credit, and the books are clean.
    let reposted = repost_quarantined(&lab, &direct, "acc_x", 1, 2).await;
    assert_eq!(reposted, 3, "two tail debits and the zombie's");
    LegWriter::prepare(&direct[2], schema)
        .await
        .unwrap()
        .post(&lab.leg("jr_zombie", "acc_y", 2, Side::Credit, 5, (NAMES[2], 1)))
        .await
        .unwrap();
    peers.settle().await;
    for c in &direct {
        assert_eq!(lab.balance(c, "acc_x").await, 1000 - (5 + 2 + 2 + 5));
        assert_eq!(lab.balance(c, "acc_y").await, 5 + 2 + 2 + 5);
    }
    lab.assert_healthy(true).await;
    lab.drop_all().await;
}

// Scenario 2: cut only the control plane.
#[tokio::test]
#[ignore = "needs `make up` and the etcd feature"]
async fn serving_continues_on_valid_leases_when_only_the_control_plane_is_cut() {
    let _env = exclusive().await;
    let schema = "t02_cp_cut";
    let ttl = Duration::from_secs(6);
    let pfx = prefix("cpcut");
    let lab = Lab::new(
        schema,
        &[
            ("acc_treasury", 0, true),
            ("acc_p", 0, false),
            ("acc_q", 1, false),
        ],
    )
    .await;
    let peers = lab.via(PROXIED);
    let direct_cp = control(ETCD, &pfx, ttl).await;
    let homes_cp = control(ETCD_PROXIED, &pfx, ttl).await;
    let p = acc("acc_p");
    direct_cp.register(&p, &node(0)).await.unwrap();
    lab.fund("acc_p", 1000).await;
    // The node-side guard (K-08), with the defaults scaled to this ttl: serve for at most
    // 4.8 s after the last confirmation (ttl 6 s, margin 1.2 s).
    let guard = LeaseGuard::new(
        node(0),
        GuardConfig {
            confirm_timeout: Duration::from_secs(1),
            ..GuardConfig::for_ttl(ttl)
        },
        Arc::new(SystemClock::new()),
    )
    .unwrap();
    assert_eq!(guard.renew(&direct_cp).await.unwrap(), 1);

    cut("etcd").await;
    let cut_at = Instant::now();
    // The data path never touches the control plane, so writes carry on: cross-cell through
    // the proxies, and local ones. Stay well inside the lease.
    let mut done = 0;
    while cut_at.elapsed() < Duration::from_millis(1500) {
        peers
            .transfer(&format!("jr_c{done}"), "acc_p", "acc_q", 3, &one)
            .await
            .unwrap();
        done += 1;
    }
    assert!(done >= 3, "only {done} transfers in the window");
    lab.transfer("jr_local", "acc_treasury", "acc_p", 1, &one)
        .await
        .unwrap();

    // From the home, the control plane is unavailable, and that is not a lost lease.
    assert!(matches!(
        homes_cp.check_lease(&p, &node(0)).await,
        Err(ControlError::Unavailable(_))
    ));
    // The guard keeps serving on the lease it last confirmed, and says so, not that it lost it.
    assert_eq!(
        guard.check(&homes_cp, &p).await,
        Ok(Epoch(1)),
        "inside the serving window the guard serves on the last confirmation"
    );
    // Nothing can fail over while it is cut: the attempt is refused and changes nothing.
    let cells = [
        (
            node(1),
            PgCell::new(Arc::new(connect(PROXIED[1]).await), schema, TENANT),
        ),
        (
            node(2),
            PgCell::new(Arc::new(connect(PROXIED[2]).await), schema, TENANT),
        ),
    ];
    let homes_cp_again = control(ETCD_PROXIED, &pfx, ttl).await;
    let blocked = Orchestrator::new(
        homes_cp,
        cells.into_iter().collect(),
        Duration::from_secs(5),
    );
    let err = blocked.promote(&p, &node(1)).await.unwrap_err();
    assert!(
        matches!(err, FailoverError::Control(ControlError::Unavailable(_))),
        "{err}"
    );
    assert!(
        cut_at.elapsed() < ttl,
        "the first part must stay inside the lease"
    );

    heal().await;
    // The lease was valid throughout, so it is still valid, and nothing moved.
    assert_eq!(direct_cp.check_lease(&p, &node(0)).await.unwrap(), Epoch(1));
    assert_eq!(direct_cp.home(&p).await.unwrap(), (node(0), Epoch(1)));
    assert_eq!(
        direct_cp.fences().await.unwrap().len(),
        1,
        "no fence was issued"
    );
    peers.settle().await;
    let cells = lab.cells().await;
    assert_eq!(lab.balance(&cells[0], "acc_p").await, 1000 + 1 - 3 * done);
    assert_eq!(lab.balance(&cells[1], "acc_q").await, 3 * done);

    // A lease is valid only while it is renewed. Cut for longer than the ttl and it lapses.
    // The guard stops on its own timing before that, as unavailable and never as a lost
    // lease, and only the control plane, once reachable, says the lease was lost.
    assert_eq!(guard.check(&direct_cp, &p).await, Ok(Epoch(1)));
    cut("etcd").await;
    let cut_at = Instant::now();
    tokio::time::sleep(guard.config().serving_window() + Duration::from_millis(300)).await;
    assert!(
        cut_at.elapsed() < ttl,
        "the lease itself has not yet lapsed"
    );
    let err = guard.check(&homes_cp_again, &p).await.unwrap_err();
    assert!(
        err.is_unavailable(),
        "the guard stops before the lease lapses, as unavailable: {err}"
    );
    tokio::time::sleep(ttl + Duration::from_secs(2) - cut_at.elapsed()).await;
    let err = guard.check(&homes_cp_again, &p).await.unwrap_err();
    assert!(
        err.is_unavailable() && !matches!(err, GuardError::LeaseLost { .. }),
        "still cut: unavailable, not a lost lease: {err}"
    );
    heal().await;
    assert!(matches!(
        direct_cp.check_lease(&p, &node(0)).await,
        Err(ControlError::LeaseExpired { .. })
    ));
    assert!(matches!(
        guard.check(&homes_cp_again, &p).await,
        Err(GuardError::LeaseLost { .. })
    ));
    lab.assert_healthy(false).await;
    lab.drop_all().await;
}

// Scenario 3: 200 ms of latency on every link.
#[tokio::test]
#[ignore = "needs `make up` and the etcd feature"]
async fn two_hundred_milliseconds_of_latency_slows_everything_and_loses_nothing() {
    let _env = exclusive().await;
    let schema = "t02_latency";
    let pfx = prefix("latency");
    let lab = Lab::new(
        schema,
        &[
            ("acc_treasury", 0, true),
            ("acc_p", 0, false),
            ("acc_q", 1, false),
        ],
    )
    .await;
    let peers = lab.via(PROXIED);
    let homes_cp = control(ETCD_PROXIED, &pfx, Duration::from_secs(60)).await;
    let p = acc("acc_p");
    homes_cp.register(&p, &node(0)).await.unwrap();
    lab.fund("acc_p", 100).await;

    let fast = {
        let started = Instant::now();
        peers
            .transfer("jr_fast", "acc_p", "acc_q", 1, &one)
            .await
            .unwrap();
        started.elapsed()
    };

    for proxy in PROXY_NAMES.into_iter().chain(["etcd"]) {
        toxic(proxy, &latency(200)).await;
    }
    let started = Instant::now();
    for n in 0..6 {
        peers
            .transfer(&format!("jr_slow{n}"), "acc_p", "acc_q", 1, &one)
            .await
            .unwrap();
    }
    let slow = started.elapsed();
    assert!(
        slow >= Duration::from_millis(6 * 200),
        "six transfers took {slow:?} with 200 ms links"
    );
    assert!(
        slow > fast * 6,
        "latency had no effect: fast {fast:?}, slow {slow:?}"
    );
    // The control plane answers, slowly.
    let started = Instant::now();
    homes_cp.check_lease(&p, &node(0)).await.unwrap();
    assert!(
        started.elapsed() >= Duration::from_millis(200),
        "{:?}",
        started.elapsed()
    );

    heal().await;
    peers.settle().await;
    let tenant: TenantId = TENANT.parse().unwrap();
    let cells = lab.cells().await;
    assert!(
        cross_cell::unfinished(&cells[0], schema, &tenant)
            .await
            .unwrap()
            .is_empty()
    );
    for c in &cells {
        assert_eq!(lab.balance(c, "acc_p").await, 100 - 7);
        assert_eq!(lab.balance(c, "acc_q").await, 7);
    }
    lab.assert_healthy(false).await;
    lab.drop_all().await;
}

// Scenario 4: dropped connections. TCP does not lose packets, it resets or stalls, so this
// resets a third of the connections to each node part-way through, and stalls some.
#[tokio::test]
#[ignore = "needs `make up` and the etcd feature"]
async fn dropped_connections_are_retried_to_exactly_once_and_recovered() {
    let _env = exclusive().await;
    let schema = "t02_drops";
    let lab = Lab::new(
        schema,
        &[
            ("acc_treasury", 0, true),
            ("acc_p", 0, false),
            ("acc_q", 1, false),
        ],
    )
    .await;
    let peers = lab.via(PROXIED);
    lab.fund("acc_p", 1000).await;

    for proxy in PROXY_NAMES {
        toxic(
            proxy,
            r#"{"name":"drop","type":"reset_peer","stream":"downstream","toxicity":0.3,"attributes":{"timeout":0}}"#,
        )
        .await;
    }
    let (mut failed_attempts, mut settled) = (0, 0);
    for n in 0..20 {
        let journal = format!("jr_d{n}");
        let mut done = false;
        for _ in 0..40 {
            let attempt = tokio::time::timeout(
                Duration::from_secs(5),
                peers.transfer(&journal, "acc_p", "acc_q", 5, &one),
            )
            .await;
            match attempt {
                Ok(Ok(_)) => {
                    done = true;
                    break;
                }
                Ok(Err(LegError::Database(_))) | Err(_) => failed_attempts += 1,
                Ok(Err(e)) => panic!("{journal}: {e:?}"),
            }
        }
        if done {
            settled += 1;
        }
    }
    heal().await;
    assert!(
        failed_attempts > 0,
        "the proxy dropped nothing, so this tested nothing"
    );

    // Whatever was left half done is finished once, and nothing is applied twice.
    let tenant: TenantId = TENANT.parse().unwrap();
    let cells = lab.cells().await;
    cross_cell::recover(&cells[0], &cells[1], schema, &tenant)
        .await
        .unwrap();
    assert!(
        cross_cell::unfinished(&cells[0], schema, &tenant)
            .await
            .unwrap()
            .is_empty()
    );
    peers.settle().await;
    let moved = 5 * 20;
    assert_eq!(settled, 20, "every transfer got through in the end");
    for c in &cells {
        assert_eq!(lab.balance(c, "acc_p").await, 1000 - moved);
        assert_eq!(lab.balance(c, "acc_q").await, moved);
    }
    lab.assert_healthy(false).await;
    lab.drop_all().await;
}
