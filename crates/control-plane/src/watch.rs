use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::{ControlError, Fence};

/// A feed of fences. It yields every fence ever issued, then each new one as it happens,
/// so a node that has just started (or rejoined) needs nothing else to catch up. Fences
/// for one account arrive in epoch order. Fences for different accounts may interleave.
///
/// `next` returns `None` when the feed has ended, and `Some(Err(..))` when it broke: the
/// caller starts a new feed, which replays history and so loses nothing. Installing a
/// fence twice is harmless.
pub struct FenceWatch {
    rx: mpsc::UnboundedReceiver<Result<Fence, ControlError>>,
    task: Option<JoinHandle<()>>,
}

impl FenceWatch {
    pub(crate) fn new(
        rx: mpsc::UnboundedReceiver<Result<Fence, ControlError>>,
        task: Option<JoinHandle<()>>,
    ) -> Self {
        FenceWatch { rx, task }
    }

    pub async fn next(&mut self) -> Option<Result<Fence, ControlError>> {
        self.rx.recv().await
    }
}

impl Drop for FenceWatch {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
