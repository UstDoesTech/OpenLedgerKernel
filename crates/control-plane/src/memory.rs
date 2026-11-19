use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use ledger_core::AccountId;

use tokio::sync::mpsc;

use crate::{Clock, ControlError, ControlPlane, Epoch, Fence, FenceWatch, NodeId};

struct Account {
    home: NodeId,
    epoch: Epoch,
    lease_until: Duration,
    /// Set by `expire`: the lease is dead and the heartbeat must not revive it.
    forced: bool,
    fences: Vec<Fence>,
}

/// An in-process control plane. It has none of the availability of a consensus store, which
/// is the point of the interface: swap it for an etcd-backed one in production.
pub struct InMemoryControlPlane {
    lease_ttl: Duration,
    clock: Arc<dyn Clock>,
    accounts: Mutex<BTreeMap<AccountId, Account>>,
    /// Open fence feeds. Always locked after `accounts`, and sent to while `accounts` is held,
    /// so a feed never sees a fence out of order or twice.
    draining: Mutex<BTreeSet<NodeId>>,
    watchers: Mutex<Vec<mpsc::UnboundedSender<Result<Fence, ControlError>>>>,
}

impl InMemoryControlPlane {
    pub fn new(lease_ttl: Duration, clock: Arc<dyn Clock>) -> Self {
        InMemoryControlPlane {
            lease_ttl,
            clock,
            accounts: Mutex::new(BTreeMap::new()),
            draining: Mutex::new(BTreeSet::new()),
            watchers: Mutex::new(Vec::new()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<AccountId, Account>> {
        // No call below can leave an account half-updated, so a poisoned lock is still usable.
        self.accounts.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn deadline(&self) -> Duration {
        self.clock.now() + self.lease_ttl
    }

    fn publish(&self, fence: &Fence) {
        let mut watchers = self.watchers.lock().unwrap_or_else(|e| e.into_inner());
        watchers.retain(|w| w.send(Ok(fence.clone())).is_ok());
    }
}

fn find<'a>(
    map: &'a mut BTreeMap<AccountId, Account>,
    account: &AccountId,
) -> Result<&'a mut Account, ControlError> {
    map.get_mut(account)
        .ok_or_else(|| ControlError::UnknownAccount(account.clone()))
}

impl ControlPlane for InMemoryControlPlane {
    async fn register(&self, account: &AccountId, home: &NodeId) -> Result<(), ControlError> {
        let deadline = self.deadline();
        let mut map = self.lock();
        if map.contains_key(account) {
            return Err(ControlError::AlreadyRegistered(account.clone()));
        }
        let first = Fence {
            account: account.clone(),
            epoch: Epoch::FIRST,
            home: home.clone(),
            prev_epoch: Epoch::NONE,
            prev_cutoff_seq: 0,
        };
        map.insert(
            account.clone(),
            Account {
                home: home.clone(),
                epoch: Epoch::FIRST,
                lease_until: deadline,
                forced: false,
                fences: vec![first.clone()],
            },
        );
        self.publish(&first);
        Ok(())
    }

    async fn home(&self, account: &AccountId) -> Result<(NodeId, Epoch), ControlError> {
        let mut map = self.lock();
        let a = find(&mut map, account)?;
        Ok((a.home.clone(), a.epoch))
    }

    async fn accounts(&self) -> Result<Vec<AccountId>, ControlError> {
        Ok(self.lock().keys().cloned().collect())
    }

    async fn homed_at(&self, node: &NodeId) -> Result<Vec<AccountId>, ControlError> {
        Ok(self
            .lock()
            .iter()
            .filter(|(_, a)| &a.home == node)
            .map(|(k, _)| k.clone())
            .collect())
    }

    async fn check_lease(&self, account: &AccountId, node: &NodeId) -> Result<Epoch, ControlError> {
        let now = self.clock.now();
        let deadline = self.deadline();
        let mut map = self.lock();
        let a = find(&mut map, account)?;
        if &a.home != node {
            return Err(ControlError::NotHome {
                account: account.clone(),
                node: node.clone(),
                home: a.home.clone(),
                epoch: a.epoch,
            });
        }
        if a.forced || now > a.lease_until {
            return Err(ControlError::LeaseExpired {
                account: account.clone(),
                node: node.clone(),
            });
        }
        a.lease_until = deadline;
        Ok(a.epoch)
    }

    async fn expire(&self, account: &AccountId) -> Result<(), ControlError> {
        let mut map = self.lock();
        let a = find(&mut map, account)?;
        a.lease_until = Duration::ZERO;
        a.forced = true;
        Ok(())
    }

    async fn renew(&self, account: &AccountId) -> Result<(), ControlError> {
        let deadline = self.deadline();
        let mut map = self.lock();
        let a = find(&mut map, account)?;
        a.lease_until = deadline;
        a.forced = false;
        Ok(())
    }

    async fn heartbeat(&self, unreachable: &BTreeSet<NodeId>) -> Result<usize, ControlError> {
        let deadline = self.deadline();
        let mut renewed = 0;
        for a in self.lock().values_mut() {
            if !unreachable.contains(&a.home) && !a.forced {
                a.lease_until = deadline;
                renewed += 1;
            }
        }
        Ok(renewed)
    }

    async fn promote(
        &self,
        account: &AccountId,
        new_home: &NodeId,
        prev_epoch: Epoch,
        prev_cutoff_seq: u64,
    ) -> Result<Fence, ControlError> {
        let deadline = self.deadline();
        let mut map = self.lock();
        let a = find(&mut map, account)?;
        if prev_epoch > a.epoch {
            return Err(ControlError::CutoffInTheFuture {
                account: account.clone(),
                current: a.epoch,
                claimed: prev_epoch,
            });
        }
        a.epoch = a.epoch.next();
        a.home = new_home.clone();
        a.lease_until = deadline;
        a.forced = false;
        let fence = Fence {
            account: account.clone(),
            epoch: a.epoch,
            home: new_home.clone(),
            prev_epoch,
            prev_cutoff_seq,
        };
        a.fences.push(fence.clone());
        self.publish(&fence);
        Ok(fence)
    }

    async fn fences(&self) -> Result<Vec<Fence>, ControlError> {
        Ok(self
            .lock()
            .values()
            .flat_map(|a| a.fences.iter().cloned())
            .collect())
    }

    async fn watch_fences(&self) -> Result<FenceWatch, ControlError> {
        let (tx, rx) = mpsc::unbounded_channel();
        let map = self.lock();
        for fence in map.values().flat_map(|a| a.fences.iter()) {
            let _ = tx.send(Ok(fence.clone()));
        }
        self.watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        drop(map);
        Ok(FenceWatch::new(rx, None))
    }

    async fn drain(&self, cell: &NodeId) -> Result<(), ControlError> {
        self.draining
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(cell.clone());
        Ok(())
    }

    async fn undrain(&self, cell: &NodeId) -> Result<(), ControlError> {
        self.draining
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(cell);
        Ok(())
    }

    async fn draining(&self) -> Result<BTreeSet<NodeId>, ControlError> {
        Ok(self
            .draining
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ManualClock;
    use proptest::prelude::*;

    const TTL: Duration = Duration::from_secs(30);

    fn acc(s: &str) -> AccountId {
        format!("acc_{s}").parse().unwrap()
    }

    fn node(s: &str) -> NodeId {
        s.parse().unwrap()
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn setup() -> (Arc<ManualClock>, InMemoryControlPlane) {
        let clock = Arc::new(ManualClock::new());
        let cp = InMemoryControlPlane::new(TTL, clock.clone());
        (clock, cp)
    }

    #[tokio::test]
    async fn register_starts_at_epoch_one_with_a_first_fence() {
        let (_c, cp) = setup();
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
    }

    #[tokio::test]
    async fn registering_twice_or_using_an_unknown_account_is_an_error() {
        let (_c, cp) = setup();
        cp.register(&acc("p"), &node("a")).await.unwrap();
        assert!(matches!(
            cp.register(&acc("p"), &node("b")).await,
            Err(ControlError::AlreadyRegistered(_))
        ));
        // The failed second registration changed nothing.
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

    #[tokio::test]
    async fn a_lease_runs_out_unless_the_home_keeps_using_it() {
        let (clock, cp) = setup();
        let (p, a) = (acc("p"), node("a"));
        cp.register(&p, &a).await.unwrap();

        // Each check renews, so a home that keeps working never lapses.
        for _ in 0..5 {
            clock.advance(secs(20));
            assert_eq!(cp.check_lease(&p, &a).await.unwrap(), Epoch(1));
        }
        // A silent home lapses once the TTL passes.
        clock.advance(secs(31));
        assert!(matches!(
            cp.check_lease(&p, &a).await,
            Err(ControlError::LeaseExpired { .. })
        ));
    }

    #[tokio::test]
    async fn not_home_takes_precedence_over_expired() {
        let (clock, cp) = setup();
        let p = acc("p");
        cp.register(&p, &node("a")).await.unwrap();
        clock.advance(secs(100));
        assert!(matches!(
            cp.check_lease(&p, &node("b")).await,
            Err(ControlError::NotHome { .. })
        ));
        assert!(matches!(
            cp.check_lease(&p, &node("a")).await,
            Err(ControlError::LeaseExpired { .. })
        ));
    }

    #[tokio::test]
    async fn a_forced_expiry_is_never_revived_by_the_heartbeat() {
        let (clock, cp) = setup();
        let (p, a) = (acc("p"), node("a"));
        cp.register(&p, &a).await.unwrap();
        let none = BTreeSet::new();

        // Even at time zero, where an expiry date of zero would not look expired.
        cp.expire(&p).await.unwrap();
        assert!(matches!(
            cp.check_lease(&p, &a).await,
            Err(ControlError::LeaseExpired { .. })
        ));

        for _ in 0..5 {
            clock.advance(secs(10));
            assert_eq!(cp.heartbeat(&none).await.unwrap(), 0);
        }
        assert!(matches!(
            cp.check_lease(&p, &a).await,
            Err(ControlError::LeaseExpired { .. })
        ));

        cp.renew(&p).await.unwrap();
        assert_eq!(cp.check_lease(&p, &a).await.unwrap(), Epoch(1));
        // And once renewed, the heartbeat looks after it again.
        clock.advance(secs(20));
        assert_eq!(cp.heartbeat(&none).await.unwrap(), 1);
        clock.advance(secs(20));
        assert!(cp.check_lease(&p, &a).await.is_ok());
    }

    #[tokio::test]
    async fn the_heartbeat_skips_homes_that_cannot_reach_the_control_plane() {
        let (clock, cp) = setup();
        let (p, q) = (acc("p"), acc("q"));
        cp.register(&p, &node("a")).await.unwrap();
        cp.register(&q, &node("b")).await.unwrap();
        let cut_off: BTreeSet<NodeId> = [node("a")].into();

        for _ in 0..4 {
            clock.advance(secs(20));
            assert_eq!(cp.heartbeat(&cut_off).await.unwrap(), 1);
        }
        assert!(matches!(
            cp.check_lease(&p, &node("a")).await,
            Err(ControlError::LeaseExpired { .. })
        ));
        assert!(cp.check_lease(&q, &node("b")).await.is_ok());
    }

    #[tokio::test]
    async fn promote_bumps_the_epoch_records_the_cutoff_and_moves_the_home() {
        let (clock, cp) = setup();
        let (p, a, b) = (acc("p"), node("a"), node("b"));
        cp.register(&p, &a).await.unwrap();
        cp.expire(&p).await.unwrap();
        clock.advance(secs(100));

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
        // The new home has a fresh lease, and the old home is no longer home.
        assert_eq!(cp.check_lease(&p, &b).await.unwrap(), Epoch(2));
        assert!(matches!(
            cp.check_lease(&p, &a).await,
            Err(ControlError::NotHome { .. })
        ));
        assert_eq!(cp.fences().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn the_cutoff_epoch_may_be_older_than_current_but_not_newer() {
        let (_c, cp) = setup();
        let p = acc("p");
        cp.register(&p, &node("a")).await.unwrap();
        cp.promote(&p, &node("b"), Epoch(1), 3).await.unwrap(); // now epoch 2
        // The new home holds nothing from epoch 2 yet, so its high-water is still epoch 1.
        assert!(cp.promote(&p, &node("c"), Epoch(1), 3).await.is_ok());
        // Nobody can hold entries from an epoch that has not happened.
        let err = cp.promote(&p, &node("a"), Epoch(9), 0).await.unwrap_err();
        assert!(matches!(err, ControlError::CutoffInTheFuture { .. }));
        assert_eq!(
            cp.home(&p).await.unwrap().1,
            Epoch(3),
            "a refused promotion changes nothing"
        );
    }

    #[tokio::test]
    async fn listings_are_ordered() {
        let (_c, cp) = setup();
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

    /// The trait is usable generically, which is how the data nodes and relay will hold it.
    async fn renewed_by_check<C: ControlPlane>(cp: &C, p: &AccountId, n: &NodeId) -> Epoch {
        cp.check_lease(p, n).await.unwrap()
    }

    #[tokio::test]
    async fn usable_through_the_trait() {
        let (_c, cp) = setup();
        cp.register(&acc("p"), &node("a")).await.unwrap();
        assert_eq!(renewed_by_check(&cp, &acc("p"), &node("a")).await, Epoch(1));
    }

    #[derive(Debug, Clone)]
    enum Op {
        Advance(u64),
        Check(usize),
        Expire(usize),
        Renew(usize),
        Heartbeat(Vec<usize>),
        Promote(usize, usize),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u64..40).prop_map(Op::Advance),
            (0usize..3).prop_map(Op::Check),
            (0usize..3).prop_map(Op::Expire),
            (0usize..3).prop_map(Op::Renew),
            prop::collection::vec(0usize..3, 0..3).prop_map(Op::Heartbeat),
            (0usize..3, 0usize..3).prop_map(|(a, n)| Op::Promote(a, n)),
        ]
    }

    proptest! {
        /// Whatever happens: epochs only grow by one per promotion and match the fence
        /// history, and a forced expiry is never revived by anything except renew or promote.
        #[test]
        fn invariants_hold_under_any_sequence(ops in prop::collection::vec(op(), 0..60)) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let clock = Arc::new(ManualClock::new());
                let cp = InMemoryControlPlane::new(TTL, clock.clone());
                let accounts: Vec<_> = (0..3).map(|i| acc(&format!("a{i}"))).collect();
                let nodes: Vec<_> = (0..3).map(|i| node(&format!("n{i}"))).collect();
                let mut forced = [false; 3];
                for (i, a) in accounts.iter().enumerate() {
                    cp.register(a, &nodes[i]).await.unwrap();
                }
                for op in ops {
                    match op {
                        Op::Advance(s) => clock.advance(secs(s)),
                        Op::Check(i) => {
                            let (home, _) = cp.home(&accounts[i]).await.unwrap();
                            let r = cp.check_lease(&accounts[i], &home).await;
                            if forced[i] { prop_assert!(r.is_err(), "forced expiry was revived"); }
                        }
                        Op::Expire(i) => { cp.expire(&accounts[i]).await.unwrap(); forced[i] = true; }
                        Op::Renew(i) => { cp.renew(&accounts[i]).await.unwrap(); forced[i] = false; }
                        Op::Heartbeat(cut) => {
                            let cut: BTreeSet<NodeId> = cut.into_iter().map(|i| nodes[i].clone()).collect();
                            cp.heartbeat(&cut).await.unwrap();
                        }
                        Op::Promote(i, n) => {
                            let (_, e) = cp.home(&accounts[i]).await.unwrap();
                            cp.promote(&accounts[i], &nodes[n], e, 0).await.unwrap();
                            forced[i] = false;
                        }
                    }
                    let fences = cp.fences().await.unwrap();
                    for a in &accounts {
                        let (home, epoch) = cp.home(a).await.unwrap();
                        let mine: Vec<_> = fences.iter().filter(|f| &f.account == a).collect();
                        prop_assert_eq!(mine.len() as u64, epoch.0);
                        for (k, f) in mine.iter().enumerate() {
                            prop_assert_eq!(f.epoch.0, k as u64 + 1);
                        }
                        prop_assert_eq!(&mine.last().unwrap().home, &home);
                    }
                }
                Ok(())
            })?;
        }
    }
}
