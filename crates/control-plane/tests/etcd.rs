#![cfg(feature = "etcd")]
//! The shared suite against a real etcd, plus what only etcd can show: a node cut off from
//! etcd stops renewing and loses its lease.
//!
//! Needs etcd on `LEDGER_ETCD` (default `http://127.0.0.1:2379`, which `make up` provides).
//! Run with `cargo test -p control-plane --test etcd -- --ignored`. Every test works under
//! its own key prefix, so they share one cluster and run in parallel.

mod suite;

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use control_plane::{ControlError, ControlPlane, Epoch, EtcdConfig, EtcdControlPlane, renew_homed};
use suite::{acc, node};
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

/// etcd's floor for a lease is about two seconds.
const TTL: Duration = Duration::from_secs(2);

fn endpoint() -> String {
    std::env::var("LEDGER_ETCD").unwrap_or_else(|_| "http://127.0.0.1:2379".into())
}

fn unique_prefix() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "/control-plane-test/{nanos}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

async fn connect(endpoints: Vec<String>, prefix: &str) -> EtcdControlPlane {
    EtcdControlPlane::connect(EtcdConfig::new(endpoints, prefix, TTL))
        .await
        .expect("etcd is reachable")
}

struct EtcdRig {
    cp: EtcdControlPlane,
}

impl EtcdRig {
    async fn new() -> Self {
        EtcdRig {
            cp: connect(vec![endpoint()], &unique_prefix()).await,
        }
    }
}

impl suite::Rig for EtcdRig {
    type Cp = EtcdControlPlane;
    fn cp(&self) -> &Self::Cp {
        &self.cp
    }
    fn ttl(&self) -> Duration {
        TTL
    }
    async fn elapse(&self, by: Duration) {
        // A wait that is meant to outlast a lease needs slack, because etcd checks for expired
        // leases about twice a second. A short one (renewing in time) must not get any, or a
        // slow runner has no room.
        let slack = if by >= TTL {
            Duration::from_millis(600)
        } else {
            Duration::ZERO
        };
        tokio::time::sleep(by + slack).await;
    }
}

macro_rules! etcd_suite_tests {
    ($($name:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            #[ignore = "needs etcd; see the file comment"]
            async fn $name() {
                let rig = EtcdRig::new().await;
                suite::$name(&rig).await;
            }
        )+
    };
}

etcd_suite_tests!(
    register_starts_at_epoch_one_with_a_first_fence,
    registering_twice_or_using_an_unknown_account_is_an_error,
    a_lease_runs_out_unless_the_home_keeps_using_it,
    not_home_takes_precedence_over_expired,
    a_forced_expiry_is_never_revived_by_the_heartbeat,
    the_heartbeat_skips_homes_that_cannot_reach_the_control_plane,
    the_heartbeat_restores_a_lease_that_lapsed_on_its_own,
    promote_bumps_the_epoch_records_the_cutoff_and_moves_the_home,
    the_cutoff_epoch_may_be_older_than_current_but_not_newer,
    racing_promotions_each_take_a_distinct_epoch,
    listings_are_ordered,
    listings_are_ordered_for_names_that_share_a_prefix,
    the_fence_feed_replays_history_then_follows_live,
    follow_fences_installs_each_fence_until_the_installer_stops,
    renew_homed_keeps_a_nodes_own_leases_alive,
    draining_is_a_marker_that_can_be_set_repeated_and_cleared,
    draining_leaves_homes_and_leases_alone,
);

/// A TCP relay to etcd that can be cut, standing in for a network partition: the node's
/// only path to etcd goes through it.
struct Partition {
    addr: String,
    cut: watch::Sender<bool>,
}

impl Partition {
    async fn open(target: &str) -> Partition {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let target = target.trim_start_matches("http://").to_owned();
        let (cut, cut_rx) = watch::channel(false);
        tokio::spawn(async move {
            loop {
                let Ok((mut inbound, _)) = listener.accept().await else {
                    return;
                };
                if *cut_rx.borrow() {
                    continue; // refuse by dropping the connection
                }
                let target = target.clone();
                let mut cut_rx = cut_rx.clone();
                tokio::spawn(async move {
                    let Ok(mut outbound) = TcpStream::connect(&target).await else {
                        return;
                    };
                    tokio::select! {
                        _ = copy_bidirectional(&mut inbound, &mut outbound) => {}
                        _ = cut_rx.wait_for(|c| *c) => {}
                    }
                });
            }
        });
        Partition { addr, cut }
    }

    fn cut(&self) {
        let _ = self.cut.send(true);
    }
}

/// The acceptance line "a node that cannot reach etcd stops renewing and loses its lease":
/// the node renews through a link that is then cut. Its renewals fail, so it cannot hold the
/// lease, while another client sees the lease lapse on etcd's own clock. A node that stays
/// connected is unaffected.
#[tokio::test]
#[ignore = "needs etcd; see the file comment"]
async fn a_node_cut_off_from_etcd_stops_renewing_and_loses_its_lease() {
    let prefix = unique_prefix();
    let partition = Partition::open(&endpoint()).await;
    let operator = connect(vec![endpoint()], &prefix).await; // sees etcd directly
    let isolated = connect(vec![partition.addr.clone()], &prefix).await; // the node's view
    let healthy = connect(vec![endpoint()], &prefix).await;

    let (a, b) = (node("a"), node("b"));
    operator.register(&acc("p"), &a).await.unwrap();
    operator.register(&acc("q"), &b).await.unwrap();

    // While connected, both nodes keep their leases past the TTL.
    for _ in 0..3 {
        tokio::time::sleep(TTL * 2 / 3).await;
        assert_eq!(renew_homed(&isolated, &a).await.unwrap(), 1);
        assert_eq!(renew_homed(&healthy, &b).await.unwrap(), 1);
    }

    partition.cut();
    let started = std::time::Instant::now();
    // Cut off, the node's renewals fail rather than quietly succeed.
    let err = renew_homed(&isolated, &a).await.unwrap_err();
    assert!(matches!(err, ControlError::Unavailable(_)), "{err}");
    // Nobody renews p, so etcd lets its lease run out. q's node is unaffected.
    while started.elapsed() < TTL * 2 {
        let _ = renew_homed(&isolated, &a).await;
        assert_eq!(renew_homed(&healthy, &b).await.unwrap(), 1);
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(matches!(
        operator.check_lease(&acc("p"), &a).await,
        Err(ControlError::LeaseExpired { .. })
    ));
    assert_eq!(operator.check_lease(&acc("q"), &b).await.unwrap(), Epoch(1));
}

/// A second control plane handle (another node, say) sees what the first wrote.
#[tokio::test]
#[ignore = "needs etcd; see the file comment"]
async fn state_is_shared_through_etcd_not_held_in_the_handle() {
    let prefix = unique_prefix();
    let one = connect(vec![endpoint()], &prefix).await;
    let two = connect(vec![endpoint()], &prefix).await;
    one.register(&acc("p"), &node("a")).await.unwrap();
    let fence = two
        .promote(&acc("p"), &node("b"), Epoch(1), 4)
        .await
        .unwrap();
    assert_eq!(one.home(&acc("p")).await.unwrap(), (node("b"), Epoch(2)));
    assert_eq!(one.fences().await.unwrap().last(), Some(&fence));
    assert!(matches!(
        two.register(&acc("p"), &node("c")).await,
        Err(ControlError::AlreadyRegistered(_))
    ));
}
