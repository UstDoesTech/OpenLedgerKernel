use ledger_core::AccountId;

use crate::{Epoch, NodeId};

/// Why a control plane call was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ControlError {
    #[error("unknown account {0}")]
    UnknownAccount(AccountId),
    #[error("account {0} is already registered")]
    AlreadyRegistered(AccountId),
    #[error("{node} is not home for {account} (home={home}, epoch={epoch})")]
    NotHome {
        account: AccountId,
        node: NodeId,
        home: NodeId,
        epoch: Epoch,
    },
    #[error("lease for {account} at {node} expired")]
    LeaseExpired { account: AccountId, node: NodeId },
    #[error("{account} is at epoch {current}; a cutoff cannot be for later epoch {claimed}")]
    CutoffInTheFuture {
        account: AccountId,
        current: Epoch,
        claimed: Epoch,
    },
    /// For implementations backed by a network service. The in-memory one never returns it.
    #[error("control plane unavailable: {0}")]
    Unavailable(String),
}
