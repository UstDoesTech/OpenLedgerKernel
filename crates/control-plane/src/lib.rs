//! The control plane: who is home for each account, at which epoch, and until when.
//!
//! [`ControlPlane`] is the whole contract. [`InMemoryControlPlane`] is the implementation
//! for tests and the local environment, and reproduces `ledger/control.py`, including the
//! heartbeat. [`EtcdControlPlane`] keeps the same state in etcd (decision D-03), with the
//! lease expiry decided by etcd itself and promotions made by compare-and-swap on the epoch.
//!
//! Per account the control plane holds the current home, the epoch and a lease. A healthy
//! home renews its lease continuously. Every promotion bumps the epoch and appends a
//! [`Fence`] recording the cutoff the old epoch ends at; fences are pushed to every data
//! node, which then voids later entries from the old epoch in SQL.
//!
//! See `docs/design.md#cells-homes-and-epochs`.

#![forbid(unsafe_code)]

mod admission;
mod clock;
mod error;
#[cfg(feature = "etcd")]
mod etcd;
mod failover;
mod guard;
mod memory;
mod node;
mod pg;
mod types;
mod watch;

pub use admission::{Admission, Draining, Ticket, sync_drain};
pub use clock::{Clock, ManualClock, SystemClock};
pub use error::ControlError;
#[cfg(feature = "etcd")]
pub use etcd::{EtcdConfig, EtcdControlPlane};
pub use failover::{Cell, CellError, FailoverError, Orchestrator, Promotion};
pub use guard::{ConfigError, GuardConfig, GuardError, LeaseGuard};
pub use memory::InMemoryControlPlane;
pub use node::{FollowError, follow_fences, renew_homed};
pub use pg::PgCell;
pub use types::{Epoch, Fence, NodeId, NodeIdError};
pub use watch::FenceWatch;

use std::collections::BTreeSet;
use std::future::Future;

use ledger_core::AccountId;

/// The control plane contract. Methods are async because the production implementation
/// talks to a consensus store over the network.
pub trait ControlPlane: Send + Sync {
    /// Starts tracking an account: homed at `home`, epoch 1, lease running. Registering an
    /// account twice is an error, because it would reset the epoch.
    fn register(
        &self,
        account: &AccountId,
        home: &NodeId,
    ) -> impl Future<Output = Result<(), ControlError>> + Send;

    /// The current home and epoch.
    fn home(
        &self,
        account: &AccountId,
    ) -> impl Future<Output = Result<(NodeId, Epoch), ControlError>> + Send;

    /// Every registered account, in order.
    fn accounts(&self) -> impl Future<Output = Result<Vec<AccountId>, ControlError>> + Send;

    /// The accounts currently homed at `node`, in order.
    fn homed_at(
        &self,
        node: &NodeId,
    ) -> impl Future<Output = Result<Vec<AccountId>, ControlError>> + Send;

    /// Returns the epoch if `node` holds a valid lease for `account`, and renews the lease.
    /// [`ControlError::NotHome`] takes precedence over [`ControlError::LeaseExpired`].
    fn check_lease(
        &self,
        account: &AccountId,
        node: &NodeId,
    ) -> impl Future<Output = Result<Epoch, ControlError>> + Send;

    /// Simulates missed heartbeats. The lease stays expired, and [`heartbeat`](Self::heartbeat)
    /// will not revive it, until [`renew`](Self::renew) or a promotion.
    fn expire(&self, account: &AccountId) -> impl Future<Output = Result<(), ControlError>> + Send;

    /// Renews the lease and clears a forced expiry.
    fn renew(&self, account: &AccountId) -> impl Future<Output = Result<(), ControlError>> + Send;

    /// What healthy homes do continuously: renew their leases. Homes in `unreachable`
    /// (they cannot reach the control plane) and leases expired on purpose are left to
    /// run out. Returns how many leases were renewed.
    fn heartbeat(
        &self,
        unreachable: &BTreeSet<NodeId>,
    ) -> impl Future<Output = Result<usize, ControlError>> + Send;

    /// Moves the home to `new_home` under the next epoch. The new home reports its
    /// high-water mark: `prev_epoch` and `prev_cutoff_seq` become the cutoff, and entries
    /// from `prev_epoch` beyond it are void. `prev_epoch` is the epoch of the highest entry
    /// the new home holds, so it may be older than the current epoch but never newer.
    fn promote(
        &self,
        account: &AccountId,
        new_home: &NodeId,
        prev_epoch: Epoch,
        prev_cutoff_seq: u64,
    ) -> impl Future<Output = Result<Fence, ControlError>> + Send;

    /// Every fence ever issued, grouped by account in order, oldest first within an account.
    /// This is what a rejoining node pulls.
    fn fences(&self) -> impl Future<Output = Result<Vec<Fence>, ControlError>> + Send;

    /// A feed of every fence issued so far and every one issued from now on. This is how a
    /// node learns of a promotion at once instead of on its next poll. See [`FenceWatch`].
    fn watch_fences(&self) -> impl Future<Output = Result<FenceWatch, ControlError>> + Send;

    /// Marks a cell as draining: nothing new should be sent to it. In-flight work is not
    /// touched, and nothing here moves an account's home or expires a lease, so commits already
    /// admitted finish normally. Repeating it is harmless.
    fn drain(&self, cell: &NodeId) -> impl Future<Output = Result<(), ControlError>> + Send;

    /// Lets a drained cell take new work again. Repeating it is harmless.
    fn undrain(&self, cell: &NodeId) -> impl Future<Output = Result<(), ControlError>> + Send;

    /// The cells currently draining, in order.
    fn draining(&self) -> impl Future<Output = Result<BTreeSet<NodeId>, ControlError>> + Send;
}
