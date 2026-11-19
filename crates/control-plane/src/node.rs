//! What a data node does with the control plane, written once for any implementation.

use std::future::Future;

use crate::{ControlError, ControlPlane, Fence, NodeId};

/// Why [`follow_fences`] stopped.
#[derive(Debug, thiserror::Error)]
pub enum FollowError<E> {
    #[error("the fence feed broke: {0}")]
    Feed(ControlError),
    #[error("installing a fence failed: {0}")]
    Install(E),
}

/// Renews the lease of every account homed at `node`, which is what a healthy node does on
/// a timer. Returns how many leases were renewed. An account that has moved away or whose
/// lease was expired on purpose is skipped, because only a promotion or [`renew`] brings it
/// back. If the control plane cannot be reached the error is returned and nothing is
/// renewed, so the leases run out on their own: a node cut off from the control plane
/// stops being able to act as home once its lease lapses.
///
/// [`renew`]: ControlPlane::renew
pub async fn renew_homed<C: ControlPlane>(cp: &C, node: &NodeId) -> Result<usize, ControlError> {
    let mut renewed = 0;
    for account in cp.homed_at(node).await? {
        match cp.check_lease(&account, node).await {
            Ok(_) => renewed += 1,
            Err(ControlError::LeaseExpired { .. } | ControlError::NotHome { .. }) => {}
            Err(other) => return Err(other),
        }
    }
    Ok(renewed)
}

/// Installs every fence the control plane has issued and every one it issues later, until the
/// feed breaks or `install` fails. `install` should be idempotent (the SQL one is), because a
/// restarted feed replays history. Run it in a loop, starting a fresh call after an error.
pub async fn follow_fences<C, F, Fut, E>(cp: &C, mut install: F) -> Result<(), FollowError<E>>
where
    C: ControlPlane,
    F: FnMut(Fence) -> Fut,
    Fut: Future<Output = Result<(), E>>,
{
    let mut feed = cp.watch_fences().await.map_err(FollowError::Feed)?;
    while let Some(next) = feed.next().await {
        let fence = next.map_err(FollowError::Feed)?;
        install(fence).await.map_err(FollowError::Install)?;
    }
    Ok(())
}
