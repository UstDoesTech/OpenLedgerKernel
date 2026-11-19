//! Promoting a standby, and draining a cell.
//!
//! [`Orchestrator::promote`] is the failover of `docs/design.md#failover`: it reads the
//! standby's high-water mark, asks the control plane for epoch + 1 with that mark as the cutoff
//! for the old epoch, and pushes the new fence to every node it can reach, the new home first.
//! A node it cannot reach is reported, and installs the fence itself when it rejoins, because
//! every node follows the control plane's fence feed ([`follow_fences`](crate::follow_fences)).
//!
//! Whether to promote is a decision for a person or a policy, not for this code: it does what
//! it is told and refuses only what would be unsafe. In particular it never guesses a cutoff.
//! If the standby's high-water mark cannot be read, nothing is promoted.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use ledger_core::AccountId;

use crate::{ControlError, ControlPlane, Epoch, Fence, NodeId};

/// A cell could not answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CellError(pub String);

/// What the orchestrator needs from a cell.
pub trait Cell: Send + Sync {
    /// The highest (epoch, sequence) the cell holds for the account; (0, 0) if none.
    fn high_water(
        &self,
        account: &AccountId,
    ) -> impl Future<Output = Result<(Epoch, u64), CellError>> + Send;

    /// Installs a fence, which also sweeps the account. Safe to repeat.
    fn install_fence(&self, fence: &Fence) -> impl Future<Output = Result<(), CellError>> + Send;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FailoverError {
    #[error("{0} is not a cell this orchestrator knows")]
    UnknownCell(NodeId),
    #[error("{cell} is already home for {account}")]
    AlreadyHome { account: AccountId, cell: NodeId },
    #[error("cannot read the high-water mark of {cell}, so no cutoff can be set: {reason}")]
    StandbyUnreachable { cell: NodeId, reason: String },
    #[error("control plane: {0}")]
    Control(#[from] ControlError),
    /// The control plane promoted, but the new home did not take the fence. It is recorded
    /// in the control plane, so the new home installs it from the fence feed when it can.
    #[error("{fence_summary}, but {cell} has not installed it: {reason}")]
    NewHomeNotFenced {
        fence_summary: String,
        cell: NodeId,
        reason: String,
        fence: Fence,
    },
}

/// What a promotion did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Promotion {
    pub fence: Fence,
    pub previous_home: NodeId,
    /// What the standby held when it was promoted: the cutoff for the old epoch.
    pub standby_high_water: (Epoch, u64),
    /// Cells that hold the fence now, the new home first.
    pub installed: Vec<NodeId>,
    /// Cells that could not be reached, with why. They install the fence when they rejoin.
    pub not_installed: Vec<(NodeId, String)>,
    pub took: Duration,
}

pub struct Orchestrator<C, K> {
    control: C,
    cells: BTreeMap<NodeId, K>,
    per_cell_timeout: Duration,
}

impl<C: ControlPlane, K: Cell> Orchestrator<C, K> {
    /// `cells` is every cell that holds the account's data. `per_cell_timeout` bounds each call
    /// to a cell, so one slow or cut-off node cannot hold a promotion up.
    pub fn new(control: C, cells: BTreeMap<NodeId, K>, per_cell_timeout: Duration) -> Self {
        Orchestrator {
            control,
            cells,
            per_cell_timeout,
        }
    }

    pub fn control(&self) -> &C {
        &self.control
    }

    async fn within<T>(
        &self,
        call: impl Future<Output = Result<T, CellError>>,
    ) -> Result<T, CellError> {
        match tokio::time::timeout(self.per_cell_timeout, call).await {
            Ok(r) => r,
            Err(_) => Err(CellError(format!(
                "no answer within {:?}",
                self.per_cell_timeout
            ))),
        }
    }

    /// Makes `to` the home of `account`.
    pub async fn promote(
        &self,
        account: &AccountId,
        to: &NodeId,
    ) -> Result<Promotion, FailoverError> {
        let started = Instant::now();
        let standby = self
            .cells
            .get(to)
            .ok_or_else(|| FailoverError::UnknownCell(to.clone()))?;
        let (previous_home, _) = self.control.home(account).await?;
        if &previous_home == to {
            return Err(FailoverError::AlreadyHome {
                account: account.clone(),
                cell: to.clone(),
            });
        }

        // The cutoff is whatever the standby holds, read now. No read, no promotion.
        let (hw_epoch, hw_seq) = self
            .within(standby.high_water(account))
            .await
            .map_err(|e| FailoverError::StandbyUnreachable {
                cell: to.clone(),
                reason: e.0,
            })?;

        let fence = self.control.promote(account, to, hw_epoch, hw_seq).await?;

        // The new home first, because it cannot write at the new epoch until it has the fence.
        let mut installed = Vec::new();
        let mut not_installed = Vec::new();
        if let Err(e) = self.within(standby.install_fence(&fence)).await {
            return Err(FailoverError::NewHomeNotFenced {
                fence_summary: format!("{account} was promoted to epoch {} at {to}", fence.epoch),
                cell: to.clone(),
                reason: e.0,
                fence,
            });
        }
        installed.push(to.clone());

        // Then every other cell, all at once, so the slowest decides the time, not the sum.
        let others: Vec<_> = self.cells.iter().filter(|(n, _)| *n != to).collect();
        let results = join_all(
            others
                .iter()
                .map(|(_, cell)| self.within(cell.install_fence(&fence))),
        )
        .await;
        for ((name, _), result) in others.iter().zip(results) {
            match result {
                Ok(()) => installed.push((*name).clone()),
                Err(e) => not_installed.push(((*name).clone(), e.0)),
            }
        }

        Ok(Promotion {
            fence,
            previous_home,
            standby_high_water: (hw_epoch, hw_seq),
            installed,
            not_installed,
            took: started.elapsed(),
        })
    }

    /// Routes new requests away from `cell`. Commits already running finish; no lease is
    /// touched and no account moves. Each node applies the marker with
    /// [`sync_drain`](crate::sync_drain).
    pub async fn drain(&self, cell: &NodeId) -> Result<(), FailoverError> {
        Ok(self.control.drain(cell).await?)
    }

    pub async fn undrain(&self, cell: &NodeId) -> Result<(), FailoverError> {
        Ok(self.control.undrain(cell).await?)
    }
}
