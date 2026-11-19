use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A monotonic clock. Leases are measured on it, so tests can move time by hand.
pub trait Clock: Send + Sync {
    /// Time since an arbitrary fixed start. Never goes backwards.
    fn now(&self) -> Duration;
}

/// Real time.
#[derive(Debug)]
pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        SystemClock {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
}

/// A clock that moves only when told to.
#[derive(Debug, Default)]
pub struct ManualClock {
    now: Mutex<Duration>,
}

impl ManualClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn advance(&self, by: Duration) {
        let mut now = self.now.lock().unwrap_or_else(|e| e.into_inner());
        *now += by;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        *self.now.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl<C: Clock + ?Sized> Clock for std::sync::Arc<C> {
    fn now(&self) -> Duration {
        (**self).now()
    }
}
