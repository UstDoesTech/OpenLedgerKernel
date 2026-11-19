//! One suite for the whole [`ControlPlane`] contract. `tests/memory.rs` runs it against the
//! in-memory implementation and `tests/etcd.rs` against etcd, so the two cannot drift apart.
//!
//! Time is the one thing that differs: the in-memory implementation reads a clock the test
//! moves by hand, and etcd expires leases on its own clock. [`Rig::elapse`] hides that.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::time::Duration;

use control_plane::{
    ControlError, ControlPlane, Epoch, Fence, FollowError, NodeId, follow_fences, renew_homed,
};
use ledger_core::AccountId;

pub trait Rig {
    type Cp: ControlPlane;
    fn cp(&self) -> &Self::Cp;
    /// How long a lease lasts without renewal.
    fn ttl(&self) -> Duration;
    /// Lets time pass for the control plane.
    async fn elapse(&self, by: Duration);
}

pub fn acc(s: &str) -> AccountId {
    format!("acc_{s}").parse().unwrap()
}

pub fn node(s: &str) -> NodeId {
    s.parse().unwrap()
}

fn part(d: Duration, num: u32, den: u32) -> Duration {
    d * num / den
}

fn lease_expired<T: std::fmt::Debug>(r: Result<T, ControlError>) -> bool {
    matches!(r, Err(ControlError::LeaseExpired { .. }))
}

pub async fn register_starts_at_epoch_one_with_a_first_fence<R: Rig>(rig: &R) {
    let cp = rig.cp();
    cp.register(&acc("p"), &node("a")).await.unwrap();
    assert_eq!(cp.home(&acc("p")).await.unwrap(), (node("a"), Epoch(1)));
    assert_eq!(
        cp.fences().await.unwrap(),
        vec![Fence {
            account: acc("p"),
            epoch: Epoch(1),
            home: node("a"),
            prev_epoch: Epoch(0),
            prev_cutoff_seq: 0
        }]
    );
    assert_eq!(
        cp.check_lease(&acc("p"), &node("a")).await.unwrap(),
        Epoch(1)
    );
}

pub async fn registering_twice_or_using_an_unknown_account_is_an_error<R: Rig>(rig: &R) {
    let cp = rig.cp();
    cp.register(&acc("p"), &node("a")).await.unwrap();
    assert!(matches!(
        cp.register(&acc("p"), &node("b")).await,
        Err(ControlError::AlreadyRegistered(_))
    ));
    assert_eq!(cp.home(&acc("p")).await.unwrap().0, node("a"));
    let q = acc("q");
    assert!(matches!(
        cp.home(&q).await,
        Err(ControlError::UnknownAccount(_))
    ));
    assert!(matches!(
        cp.check_lease(&q, &node("a")).await,
        Err(ControlError::UnknownAccount(_))
    ));
    assert!(matches!(
        cp.expire(&q).await,
        Err(ControlError::UnknownAccount(_))
    ));
    assert!(matches!(
        cp.renew(&q).await,
        Err(ControlError::UnknownAccount(_))
    ));
    assert!(matches!(
        cp.promote(&q, &node("a"), Epoch(1), 0).await,
        Err(ControlError::UnknownAccount(_))
    ));
}

pub async fn a_lease_runs_out_unless_the_home_keeps_using_it<R: Rig>(rig: &R) {
    let cp = rig.cp();
    let (p, a) = (acc("p"), node("a"));
    cp.register(&p, &a).await.unwrap();
    for _ in 0..3 {
        rig.elapse(part(rig.ttl(), 1, 2)).await;
        assert_eq!(cp.check_lease(&p, &a).await.unwrap(), Epoch(1));
    }
    rig.elapse(rig.ttl() * 2).await;
    assert!(lease_expired(cp.check_lease(&p, &a).await));
}

pub async fn not_home_takes_precedence_over_expired<R: Rig>(rig: &R) {
    let cp = rig.cp();
    let p = acc("p");
    cp.register(&p, &node("a")).await.unwrap();
    rig.elapse(rig.ttl() * 2).await;
    assert!(matches!(
        cp.check_lease(&p, &node("b")).await,
        Err(ControlError::NotHome { .. })
    ));
    assert!(lease_expired(cp.check_lease(&p, &node("a")).await));
}

pub async fn a_forced_expiry_is_never_revived_by_the_heartbeat<R: Rig>(rig: &R) {
    let cp = rig.cp();
    let (p, a) = (acc("p"), node("a"));
    cp.register(&p, &a).await.unwrap();
    let none = BTreeSet::new();

    cp.expire(&p).await.unwrap();
    assert!(lease_expired(cp.check_lease(&p, &a).await));
    for _ in 0..3 {
        rig.elapse(part(rig.ttl(), 1, 3)).await;
        assert_eq!(cp.heartbeat(&none).await.unwrap(), 0);
    }
    assert!(lease_expired(cp.check_lease(&p, &a).await));

    cp.renew(&p).await.unwrap();
    assert_eq!(cp.check_lease(&p, &a).await.unwrap(), Epoch(1));
    // Once renewed, the heartbeat looks after it again.
    rig.elapse(part(rig.ttl(), 1, 2)).await;
    assert_eq!(cp.heartbeat(&none).await.unwrap(), 1);
    rig.elapse(part(rig.ttl(), 1, 2)).await;
    assert!(cp.check_lease(&p, &a).await.is_ok());
}

pub async fn the_heartbeat_skips_homes_that_cannot_reach_the_control_plane<R: Rig>(rig: &R) {
    let cp = rig.cp();
    let (p, q) = (acc("p"), acc("q"));
    cp.register(&p, &node("a")).await.unwrap();
    cp.register(&q, &node("b")).await.unwrap();
    let cut_off: BTreeSet<NodeId> = [node("a")].into();
    for _ in 0..4 {
        rig.elapse(part(rig.ttl(), 1, 2)).await;
        assert_eq!(cp.heartbeat(&cut_off).await.unwrap(), 1);
    }
    // p has gone unrenewed for twice the TTL; q has been renewed throughout.
    assert!(lease_expired(cp.check_lease(&p, &node("a")).await));
    assert!(cp.check_lease(&q, &node("b")).await.is_ok());
}

/// A lease that lapsed because its home was cut off comes back when the home reaches the
/// control plane again and the heartbeat sees it.
pub async fn the_heartbeat_restores_a_lease_that_lapsed_on_its_own<R: Rig>(rig: &R) {
    let cp = rig.cp();
    let (p, a) = (acc("p"), node("a"));
    cp.register(&p, &a).await.unwrap();
    rig.elapse(rig.ttl() * 2).await;
    assert!(lease_expired(cp.check_lease(&p, &a).await));
    assert_eq!(cp.heartbeat(&BTreeSet::new()).await.unwrap(), 1);
    assert_eq!(cp.check_lease(&p, &a).await.unwrap(), Epoch(1));
}

pub async fn promote_bumps_the_epoch_records_the_cutoff_and_moves_the_home<R: Rig>(rig: &R) {
    let cp = rig.cp();
    let (p, a, b) = (acc("p"), node("a"), node("b"));
    cp.register(&p, &a).await.unwrap();
    cp.expire(&p).await.unwrap();
    rig.elapse(rig.ttl() * 2).await;

    let fence = cp.promote(&p, &b, Epoch(1), 7).await.unwrap();
    assert_eq!(
        fence,
        Fence {
            account: p.clone(),
            epoch: Epoch(2),
            home: b.clone(),
            prev_epoch: Epoch(1),
            prev_cutoff_seq: 7
        }
    );
    assert_eq!(cp.home(&p).await.unwrap(), (b.clone(), Epoch(2)));
    // The new home has a fresh lease, and a promotion clears the forced expiry.
    assert_eq!(cp.check_lease(&p, &b).await.unwrap(), Epoch(2));
    assert!(matches!(
        cp.check_lease(&p, &a).await,
        Err(ControlError::NotHome { .. })
    ));
    assert_eq!(cp.fences().await.unwrap().len(), 2);
}

pub async fn the_cutoff_epoch_may_be_older_than_current_but_not_newer<R: Rig>(rig: &R) {
    let cp = rig.cp();
    let p = acc("p");
    cp.register(&p, &node("a")).await.unwrap();
    cp.promote(&p, &node("b"), Epoch(1), 3).await.unwrap();
    assert!(cp.promote(&p, &node("c"), Epoch(1), 3).await.is_ok());
    let err = cp.promote(&p, &node("a"), Epoch(9), 0).await.unwrap_err();
    assert!(matches!(err, ControlError::CutoffInTheFuture { .. }));
    assert_eq!(
        cp.home(&p).await.unwrap().1,
        Epoch(3),
        "a refused promotion changes nothing"
    );
    assert_eq!(cp.fences().await.unwrap().len(), 3);
}

/// Promotions are compare-and-swap on the epoch: however they interleave, each takes one
/// epoch, and no two fences share one.
pub async fn racing_promotions_each_take_a_distinct_epoch<R: Rig>(rig: &R) {
    let cp = rig.cp();
    let p = acc("p");
    cp.register(&p, &node("a")).await.unwrap();
    let (n1, n2, n3, n4) = (node("n1"), node("n2"), node("n3"), node("n4"));
    let (r1, r2, r3, r4) = tokio::join!(
        cp.promote(&p, &n1, Epoch(1), 1),
        cp.promote(&p, &n2, Epoch(1), 2),
        cp.promote(&p, &n3, Epoch(1), 3),
        cp.promote(&p, &n4, Epoch(1), 4),
    );
    let mut epochs: Vec<u64> = [r1, r2, r3, r4]
        .into_iter()
        .map(|r| r.unwrap().epoch.0)
        .collect();
    epochs.sort_unstable();
    assert_eq!(epochs, vec![2, 3, 4, 5]);
    let fences = cp.fences().await.unwrap();
    let held: Vec<u64> = fences.iter().map(|f| f.epoch.0).collect();
    assert_eq!(held, vec![1, 2, 3, 4, 5]);
    // The record agrees with the last fence.
    let (home, epoch) = cp.home(&p).await.unwrap();
    assert_eq!(epoch, Epoch(5));
    assert_eq!(home, fences.last().unwrap().home);
}

pub async fn listings_are_ordered<R: Rig>(rig: &R) {
    let cp = rig.cp();
    for (k, n) in [("c", "a"), ("a", "b"), ("b", "a")] {
        cp.register(&acc(k), &node(n)).await.unwrap();
    }
    assert_eq!(
        cp.accounts().await.unwrap(),
        vec![acc("a"), acc("b"), acc("c")]
    );
    assert_eq!(
        cp.homed_at(&node("a")).await.unwrap(),
        vec![acc("b"), acc("c")]
    );
    let accounts: Vec<_> = cp
        .fences()
        .await
        .unwrap()
        .into_iter()
        .map(|f| f.account)
        .collect();
    assert_eq!(accounts, vec![acc("a"), acc("b"), acc("c")]);
}

/// Names that are prefixes of each other, where the order of keys in a store is easiest to
/// get wrong.
pub async fn listings_are_ordered_for_names_that_share_a_prefix<R: Rig>(rig: &R) {
    let cp = rig.cp();
    for k in ["ab", "a", "a1", "abc"] {
        cp.register(&acc(k), &node("n")).await.unwrap();
    }
    cp.promote(&acc("a"), &node("m"), Epoch(1), 0)
        .await
        .unwrap();
    let mut expected = vec![acc("ab"), acc("a"), acc("a1"), acc("abc")];
    expected.sort();
    assert_eq!(cp.accounts().await.unwrap(), expected);
    let fences = cp.fences().await.unwrap();
    let mut sorted = fences.clone();
    sorted.sort_by(|x, y| (&x.account, x.epoch).cmp(&(&y.account, y.epoch)));
    assert_eq!(fences, sorted);
    assert_eq!(fences.len(), 5);
}

async fn next_fence<C: ControlPlane>(feed: &mut control_plane::FenceWatch, _cp: &C) -> Fence {
    tokio::time::timeout(Duration::from_secs(10), feed.next())
        .await
        .expect("a fence within ten seconds")
        .expect("the feed is open")
        .expect("no feed error")
}

/// A new feed replays history, then delivers promotions as they happen.
pub async fn the_fence_feed_replays_history_then_follows_live<R: Rig>(rig: &R) {
    let cp = rig.cp();
    cp.register(&acc("a"), &node("n1")).await.unwrap();
    cp.register(&acc("b"), &node("n1")).await.unwrap();
    cp.promote(&acc("a"), &node("n2"), Epoch(1), 5)
        .await
        .unwrap();

    let mut feed = cp.watch_fences().await.unwrap();
    let mut history = Vec::new();
    for _ in 0..3 {
        history.push(next_fence(&mut feed, cp).await);
    }
    history.sort_by(|x, y| (&x.account, x.epoch).cmp(&(&y.account, y.epoch)));
    assert_eq!(history, cp.fences().await.unwrap());

    cp.register(&acc("c"), &node("n3")).await.unwrap();
    let live = cp
        .promote(&acc("b"), &node("n3"), Epoch(1), 9)
        .await
        .unwrap();
    let first = next_fence(&mut feed, cp).await;
    assert_eq!((first.account, first.epoch), (acc("c"), Epoch(1)));
    assert_eq!(next_fence(&mut feed, cp).await, live);
    // Nothing else is waiting, and nothing arrived twice.
    assert!(
        tokio::time::timeout(Duration::from_millis(300), feed.next())
            .await
            .is_err()
    );
}

pub async fn follow_fences_installs_each_fence_until_the_installer_stops<R: Rig>(rig: &R) {
    let cp = rig.cp();
    cp.register(&acc("a"), &node("n1")).await.unwrap();
    cp.promote(&acc("a"), &node("n2"), Epoch(1), 4)
        .await
        .unwrap();
    let installed = std::sync::Mutex::new(Vec::new());
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        follow_fences(cp, |f| {
            let installed = &installed;
            async move {
                let mut held = installed.lock().unwrap();
                held.push((f.account.to_string(), f.epoch.0));
                if held.len() == 3 {
                    Err("enough")
                } else {
                    Ok(())
                }
            }
        }),
    );
    // The third fence is issued while the follower is running.
    let issue = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cp.promote(&acc("a"), &node("n3"), Epoch(2), 8)
            .await
            .unwrap();
    };
    let (result, ()) = tokio::join!(result, issue);
    assert!(matches!(
        result.unwrap(),
        Err(FollowError::Install("enough"))
    ));
    let installed = installed.into_inner().unwrap();
    assert_eq!(
        installed,
        vec![
            ("acc_a".to_owned(), 1),
            ("acc_a".to_owned(), 2),
            ("acc_a".to_owned(), 3)
        ]
    );
}

pub async fn renew_homed_keeps_a_nodes_own_leases_alive<R: Rig>(rig: &R) {
    let cp = rig.cp();
    let (a, b) = (node("a"), node("b"));
    for (k, n) in [("p", &a), ("q", &a), ("r", &b)] {
        cp.register(&acc(k), n).await.unwrap();
    }
    assert_eq!(renew_homed(cp, &a).await.unwrap(), 2);
    // An account expired on purpose is skipped, not revived.
    cp.expire(&acc("q")).await.unwrap();
    assert_eq!(renew_homed(cp, &a).await.unwrap(), 1);
    // A node that renews on a timer outlives the TTL, and one that does not, does not.
    for _ in 0..3 {
        rig.elapse(part(rig.ttl(), 1, 2)).await;
        renew_homed(cp, &a).await.unwrap();
    }
    assert!(cp.check_lease(&acc("p"), &a).await.is_ok());
    assert!(lease_expired(cp.check_lease(&acc("r"), &b).await));
}

pub async fn draining_is_a_marker_that_can_be_set_repeated_and_cleared<R: Rig>(rig: &R) {
    let cp = rig.cp();
    assert!(cp.draining().await.unwrap().is_empty());
    cp.drain(&node("b")).await.unwrap();
    cp.drain(&node("a")).await.unwrap();
    cp.drain(&node("a")).await.unwrap();
    let both: BTreeSet<NodeId> = [node("a"), node("b")].into();
    assert_eq!(cp.draining().await.unwrap(), both);
    cp.undrain(&node("a")).await.unwrap();
    cp.undrain(&node("a")).await.unwrap();
    assert_eq!(cp.draining().await.unwrap(), [node("b")].into());
    cp.undrain(&node("b")).await.unwrap();
    assert!(cp.draining().await.unwrap().is_empty());
}

/// Draining a cell moves no account and touches no lease, so commits already admitted keep
/// the home and the lease they were admitted under.
pub async fn draining_leaves_homes_and_leases_alone<R: Rig>(rig: &R) {
    let cp = rig.cp();
    let (p, a) = (acc("p"), node("a"));
    cp.register(&p, &a).await.unwrap();
    cp.drain(&a).await.unwrap();
    assert_eq!(cp.home(&p).await.unwrap(), (a.clone(), Epoch(1)));
    assert_eq!(cp.check_lease(&p, &a).await.unwrap(), Epoch(1));
    for _ in 0..3 {
        rig.elapse(part(rig.ttl(), 1, 2)).await;
        assert_eq!(
            renew_homed(cp, &a).await.unwrap(),
            1,
            "a draining node still renews"
        );
    }
    assert_eq!(cp.fences().await.unwrap().len(), 1);
    cp.undrain(&a).await.unwrap();
}
