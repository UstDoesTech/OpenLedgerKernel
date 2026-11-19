//! The gate a cell puts in front of new work, so it can be drained without stopping commits
//! that are already running.
//!
//! A request calls [`Admission::enter`] before it starts and holds the [`Ticket`] until it has
//! committed or failed. Once the cell is draining, `enter` refuses new requests with
//! [`Draining`], so the caller routes them elsewhere, while tickets already held keep working
//! and the commits behind them finish. [`Admission::wait_idle`] says when the last one has.
//! Draining is the control plane's marker ([`ControlPlane::drain`]); [`sync_drain`] copies it
//! onto the gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;

use crate::{ControlError, ControlPlane, NodeId};

/// The cell is draining and takes no new requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("this cell is draining and takes no new requests")]
pub struct Draining;

#[derive(Default)]
struct State {
    draining: bool,
    in_flight: usize,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    idle: Notify,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Admission to one cell. Cheap to clone: every clone is the same gate.
#[derive(Clone, Default)]
pub struct Admission(Arc<Shared>);

/// Proof that a request was admitted. Dropping it, on success or failure, ends the request.
#[must_use = "dropping the ticket ends the request"]
pub struct Ticket(Arc<Shared>);

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.in_flight -= 1;
        if state.in_flight == 0 {
            self.0.idle.notify_waiters();
        }
    }
}

impl Admission {
    pub fn new() -> Self {
        Self::default()
    }

    /// Admits a new request, unless the cell is draining.
    pub fn enter(&self) -> Result<Ticket, Draining> {
        let mut state = self.0.lock();
        if state.draining {
            return Err(Draining);
        }
        state.in_flight += 1;
        Ok(Ticket(self.0.clone()))
    }

    /// Starts or stops draining. Requests already admitted are not affected.
    pub fn set_draining(&self, draining: bool) {
        self.0.lock().draining = draining;
    }

    pub fn is_draining(&self) -> bool {
        self.0.lock().draining
    }

    /// How many admitted requests have not finished.
    pub fn in_flight(&self) -> usize {
        self.0.lock().in_flight
    }

    /// Waits until no admitted request is running. False if `timeout` passes first. This does
    /// not stop new requests being admitted: drain first, then wait.
    pub async fn wait_idle(&self, timeout: Duration) -> bool {
        let wait = async {
            loop {
                let notified = self.0.idle.notified();
                tokio::pin!(notified);
                // Register interest before looking, so a ticket dropped in between is not missed.
                notified.as_mut().enable();
                if self.in_flight() == 0 {
                    return;
                }
                notified.await;
            }
        };
        tokio::time::timeout(timeout, wait).await.is_ok()
    }
}

/// Copies the control plane's drain marker for `cell` onto its gate. Returns whether the cell
/// is draining. If the control plane cannot be reached the gate is left as it is: an
/// unreachable control plane neither starts nor ends a drain. Call it on a timer.
pub async fn sync_drain<C: ControlPlane>(
    cp: &C,
    cell: &NodeId,
    gate: &Admission,
) -> Result<bool, ControlError> {
    let draining = cp.draining().await?.contains(cell);
    gate.set_draining(draining);
    Ok(draining)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{InMemoryControlPlane, ManualClock};

    #[test]
    fn a_draining_gate_refuses_new_requests_but_admitted_ones_keep_their_ticket() {
        let gate = Admission::new();
        let before = gate.enter().unwrap();
        gate.set_draining(true);
        assert_eq!(gate.enter().err(), Some(Draining));
        assert_eq!(gate.in_flight(), 1, "the admitted request is untouched");
        drop(before);
        assert_eq!(gate.in_flight(), 0);
        gate.set_draining(false);
        assert!(gate.enter().is_ok());
    }

    #[tokio::test]
    async fn wait_idle_returns_when_the_last_ticket_is_dropped() {
        let gate = Admission::new();
        let t1 = gate.enter().unwrap();
        let t2 = gate.enter().unwrap();
        gate.set_draining(true);
        assert!(
            !gate.wait_idle(Duration::from_millis(50)).await,
            "two are running"
        );
        let waiter = tokio::spawn({
            let gate = gate.clone();
            async move { gate.wait_idle(Duration::from_secs(5)).await }
        });
        drop(t1);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!waiter.is_finished(), "one is still running");
        drop(t2);
        assert!(waiter.await.unwrap());
    }

    #[tokio::test]
    async fn wait_idle_is_immediate_when_nothing_is_running() {
        assert!(Admission::new().wait_idle(Duration::from_millis(10)).await);
    }

    #[tokio::test]
    async fn the_gate_follows_the_control_planes_marker() {
        let cp = InMemoryControlPlane::new(
            Duration::from_secs(30),
            std::sync::Arc::new(ManualClock::new()),
        );
        let (me, other) = ("a".parse().unwrap(), "b".parse().unwrap());
        let gate = Admission::new();
        assert!(!sync_drain(&cp, &me, &gate).await.unwrap());
        cp.drain(&other).await.unwrap();
        assert!(
            !sync_drain(&cp, &me, &gate).await.unwrap(),
            "another cell's drain is not ours"
        );
        cp.drain(&me).await.unwrap();
        assert!(sync_drain(&cp, &me, &gate).await.unwrap());
        assert!(gate.enter().is_err());
        cp.undrain(&me).await.unwrap();
        assert!(!sync_drain(&cp, &me, &gate).await.unwrap());
        assert!(gate.enter().is_ok());
    }
}
