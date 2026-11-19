//! The lease guard a node puts in front of an account: confirm the lease with the control plane
//! before serving, and keep serving on the last confirmation while the control plane cannot be
//! reached, for as long as the lease cannot yet have lapsed.
//!
//! The node times that window itself, on its own monotonic [`Clock`], from the moment it
//! *sent* its last successful confirmation (never later than the control plane started the
//! lease), and stops [`safety_margin`](GuardConfig::safety_margin) before the lease would run
//! out at the control plane. The margin is what pays for clock drift, a slow reply and a node
//! that pauses. It never decides whether money is safe: the epoch fence in SQL voids a stale
//! home's entries whatever the guard says. It decides how much quarantine work a lapsed lease
//! can cause (a margin that is too small) against how long accounts stay unavailable when the
//! control plane blips (a margin that is too large).
//!
//! Three answers are kept apart, because a caller reacts to each differently:
//!
//! - [`GuardError::NotHome`]: the control plane says another cell is home. Route elsewhere.
//! - [`GuardError::LeaseLost`]: the control plane says the lease has lapsed. Stop.
//! - [`GuardError::Unavailable`]: the control plane could not be reached and the local window is
//!   over (or there was never a confirmation). This says nothing about the lease, so it is
//!   never reported as a lost lease and must not by itself start a failover.
//!
//! See `docs/design.md#failure-modes` and `docs/conformance.md`.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ledger_core::AccountId;

use crate::{Clock, ControlError, ControlPlane, Epoch, NodeId};

/// The timings of a [`LeaseGuard`]. Every value has a default and every one can be set.
///
/// With the defaults (a 30 second lease): renew every 10 seconds, so a node rides out one
/// missed renewal; stop serving 6 seconds early, so it serves for at most 24 seconds after
/// its last confirmation; give the control plane 2 seconds to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuardConfig {
    /// How long the control plane keeps a lease after a renewal. Must match the control plane's
    /// own setting (`EtcdConfig::lease_ttl`, or the in-memory one).
    pub lease_ttl: Duration,
    /// How often a healthy node renews. Used by [`LeaseGuard::renew`] callers on their timer.
    pub renew_every: Duration,
    /// How long before the lease could lapse the node stops serving on its own timing.
    pub safety_margin: Duration,
    /// How long one confirmation may take before it counts as unreachable.
    pub confirm_timeout: Duration,
}

/// Why a [`GuardConfig`] was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("{0} must be more than zero")]
    Zero(&'static str),
    #[error("the safety margin ({margin:?}) must be less than the lease ttl ({ttl:?})")]
    MarginTooLarge { margin: Duration, ttl: Duration },
    #[error(
        "renewing every {renew:?} cannot survive one missed renewal inside the serving window of {window:?} (ttl minus margin); renew at least twice per window"
    )]
    RenewTooSlow { renew: Duration, window: Duration },
    #[error(
        "the confirm timeout ({timeout:?}) must not be longer than the renew interval ({renew:?})"
    )]
    TimeoutTooLong { timeout: Duration, renew: Duration },
    #[error("{name} is not a whole number of milliseconds: {value:?}")]
    NotANumber { name: &'static str, value: String },
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self::for_ttl(Duration::from_secs(30))
    }
}

impl GuardConfig {
    /// The defaults for a lease of `ttl`: renew every third of it, stop a fifth early, and
    /// give the control plane a fifteenth to answer.
    pub fn for_ttl(ttl: Duration) -> Self {
        GuardConfig {
            lease_ttl: ttl,
            renew_every: ttl / 3,
            safety_margin: ttl / 5,
            confirm_timeout: ttl / 15,
        }
    }

    /// How long after a confirmation the node may keep serving without another.
    pub fn serving_window(&self) -> Duration {
        self.lease_ttl.saturating_sub(self.safety_margin)
    }

    /// Checks that the values fit together.
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (name, d) in [
            ("lease ttl", self.lease_ttl),
            ("renew interval", self.renew_every),
            ("confirm timeout", self.confirm_timeout),
        ] {
            if d.is_zero() {
                return Err(ConfigError::Zero(name));
            }
        }
        if self.safety_margin >= self.lease_ttl {
            return Err(ConfigError::MarginTooLarge {
                margin: self.safety_margin,
                ttl: self.lease_ttl,
            });
        }
        let window = self.serving_window();
        if self.renew_every * 2 > window {
            return Err(ConfigError::RenewTooSlow {
                renew: self.renew_every,
                window,
            });
        }
        if self.confirm_timeout > self.renew_every {
            return Err(ConfigError::TimeoutTooLong {
                timeout: self.confirm_timeout,
                renew: self.renew_every,
            });
        }
        Ok(())
    }

    /// Reads the timings from `lookup` (the environment, in [`from_env`](Self::from_env)). Names,
    /// in whole milliseconds: `LEDGER_LEASE_TTL_MS`, `LEDGER_LEASE_RENEW_MS`,
    /// `LEDGER_LEASE_SAFETY_MARGIN_MS` and `LEDGER_LEASE_CONFIRM_TIMEOUT_MS`. A value that is not
    /// set takes the default for the ttl in force, so setting only the ttl scales the rest.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let read = |name: &'static str| -> Result<Option<Duration>, ConfigError> {
            lookup(name)
                .map(|v| {
                    v.trim()
                        .parse::<u64>()
                        .map(Duration::from_millis)
                        .map_err(|_| ConfigError::NotANumber { name, value: v })
                })
                .transpose()
        };
        let mut c = match read("LEDGER_LEASE_TTL_MS")? {
            Some(ttl) => Self::for_ttl(ttl),
            None => Self::default(),
        };
        if let Some(d) = read("LEDGER_LEASE_RENEW_MS")? {
            c.renew_every = d;
        }
        if let Some(d) = read("LEDGER_LEASE_SAFETY_MARGIN_MS")? {
            c.safety_margin = d;
        }
        if let Some(d) = read("LEDGER_LEASE_CONFIRM_TIMEOUT_MS")? {
            c.confirm_timeout = d;
        }
        c.validate()?;
        Ok(c)
    }

    /// [`from_lookup`](Self::from_lookup) over the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }
}

/// Why the guard would not let a node serve an account.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GuardError {
    #[error("{node} is not home for {account} (home={home}, epoch={epoch})")]
    NotHome {
        account: AccountId,
        node: NodeId,
        home: NodeId,
        epoch: Epoch,
    },
    /// The control plane answered, and the lease has lapsed.
    #[error("the lease for {account} at {node} has lapsed")]
    LeaseLost { account: AccountId, node: NodeId },
    /// The control plane could not be reached, and there is no confirmation recent enough to
    /// serve on. This is not a lost lease.
    #[error(
        "cannot confirm the lease for {account}: the control plane is unavailable ({reason}); last confirmed {last_confirmed:?} ago"
    )]
    Unavailable {
        account: AccountId,
        reason: String,
        /// How long ago the last successful confirmation was sent, if there ever was one.
        last_confirmed: Option<Duration>,
    },
    /// Any other refusal from the control plane, such as an unknown account.
    #[error("{0}")]
    Refused(ControlError),
}

impl GuardError {
    /// True when the control plane could not be asked, as opposed to having said no.
    pub fn is_unavailable(&self) -> bool {
        matches!(self, GuardError::Unavailable { .. })
    }
}

/// Per account: when the last successful confirmation was sent, and the epoch it gave.
type Confirmations = BTreeMap<AccountId, (Duration, Epoch)>;

/// The guard for one node. Cheap to clone: every clone shares the same confirmations.
#[derive(Clone)]
pub struct LeaseGuard {
    node: NodeId,
    config: GuardConfig,
    clock: Arc<dyn Clock>,
    confirmed: Arc<Mutex<Confirmations>>,
}

impl LeaseGuard {
    /// A guard for `node`. The config is checked.
    pub fn new(
        node: NodeId,
        config: GuardConfig,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(LeaseGuard {
            node,
            config,
            clock,
            confirmed: Arc::default(),
        })
    }

    pub fn config(&self) -> &GuardConfig {
        &self.config
    }

    fn confirmations(&self) -> std::sync::MutexGuard<'_, Confirmations> {
        self.confirmed.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Confirms the lease for `account` with `cp`, which also renews it, and returns the epoch to
    /// serve under. If `cp` cannot be reached, returns the last confirmed epoch while the local
    /// window lasts, and [`GuardError::Unavailable`] after.
    pub async fn check<C: ControlPlane>(
        &self,
        cp: &C,
        account: &AccountId,
    ) -> Result<Epoch, GuardError> {
        self.check_with(account, cp.check_lease(account, &self.node))
            .await
    }

    /// [`check`](Self::check) for a confirmation obtained some other way: `confirmation` is the
    /// control plane's answer, not yet awaited. Its time starts now.
    pub async fn check_with(
        &self,
        account: &AccountId,
        confirmation: impl Future<Output = Result<Epoch, ControlError>>,
    ) -> Result<Epoch, GuardError> {
        let sent = self.clock.now();
        let answer = tokio::time::timeout(self.config.confirm_timeout, confirmation).await;
        let unreachable = match answer {
            Ok(Ok(epoch)) => {
                self.confirmations().insert(account.clone(), (sent, epoch));
                return Ok(epoch);
            }
            Ok(Err(ControlError::Unavailable(reason))) => reason,
            Err(_) => format!("no answer within {:?}", self.config.confirm_timeout),
            Ok(Err(ControlError::NotHome {
                account,
                node,
                home,
                epoch,
            })) => {
                self.confirmations().remove(&account);
                return Err(GuardError::NotHome {
                    account,
                    node,
                    home,
                    epoch,
                });
            }
            Ok(Err(ControlError::LeaseExpired { account, node })) => {
                self.confirmations().remove(&account);
                return Err(GuardError::LeaseLost { account, node });
            }
            Ok(Err(other)) => return Err(GuardError::Refused(other)),
        };
        let now = self.clock.now();
        let last = self.confirmations().get(account).copied();
        if let Some((at, epoch)) = last
            && now.saturating_sub(at) < self.config.serving_window()
        {
            return Ok(epoch);
        }
        Err(GuardError::Unavailable {
            account: account.clone(),
            reason: unreachable,
            last_confirmed: last.map(|(at, _)| now.saturating_sub(at)),
        })
    }

    /// What a healthy node does on its timer, every [`renew_every`](GuardConfig::renew_every):
    /// confirm every account homed here, which renews each lease and refreshes the local window.
    /// Returns how many were confirmed. Accounts that moved away or whose lease was expired on
    /// purpose are skipped. If the control plane cannot be reached the error is returned, and
    /// the windows run down on their own.
    pub async fn renew<C: ControlPlane>(&self, cp: &C) -> Result<usize, ControlError> {
        let mut renewed = 0;
        for account in cp.homed_at(&self.node).await? {
            match self.check(cp, &account).await {
                Ok(_) => renewed += 1,
                Err(GuardError::NotHome { .. } | GuardError::LeaseLost { .. }) => {}
                Err(GuardError::Unavailable { reason, .. }) => {
                    return Err(ControlError::Unavailable(reason));
                }
                Err(GuardError::Refused(e)) => return Err(e),
            }
        }
        Ok(renewed)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{InMemoryControlPlane, ManualClock};

    const S: Duration = Duration::from_secs(1);

    fn acc() -> AccountId {
        "acc_x".parse().unwrap()
    }
    fn node(n: &str) -> NodeId {
        n.parse().unwrap()
    }
    fn guard() -> (LeaseGuard, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::new());
        let g = LeaseGuard::new(node("a"), GuardConfig::default(), clock.clone()).unwrap();
        (g, clock)
    }
    async fn confirmed(g: &LeaseGuard) -> Result<Epoch, GuardError> {
        g.check_with(&acc(), async { Ok(Epoch(3)) }).await
    }
    async fn cut(g: &LeaseGuard) -> Result<Epoch, GuardError> {
        g.check_with(&acc(), async {
            Err(ControlError::Unavailable("etcd is cut".into()))
        })
        .await
    }

    #[test]
    fn the_defaults_are_a_thirty_second_lease_renewed_every_ten_stopping_six_early() {
        let c = GuardConfig::default();
        assert_eq!(c.lease_ttl, 30 * S);
        assert_eq!(c.renew_every, 10 * S);
        assert_eq!(c.safety_margin, 6 * S);
        assert_eq!(c.confirm_timeout, 2 * S);
        assert_eq!(c.serving_window(), 24 * S);
        c.validate().unwrap();
        GuardConfig::for_ttl(10 * S).validate().unwrap();
    }

    #[test]
    fn values_that_do_not_fit_together_are_refused() {
        let ok = GuardConfig::default();
        let bad = |f: fn(&mut GuardConfig)| {
            let mut c = ok;
            f(&mut c);
            c.validate().unwrap_err()
        };
        assert!(matches!(
            bad(|c| c.safety_margin = 30 * S),
            ConfigError::MarginTooLarge { .. }
        ));
        assert!(matches!(
            bad(|c| c.renew_every = 13 * S),
            ConfigError::RenewTooSlow { .. }
        ));
        assert!(matches!(
            bad(|c| c.confirm_timeout = 11 * S),
            ConfigError::TimeoutTooLong { .. }
        ));
        assert!(matches!(
            bad(|c| c.renew_every = Duration::ZERO),
            ConfigError::Zero(_)
        ));
    }

    #[test]
    fn the_environment_overrides_each_value_and_a_ttl_alone_scales_the_rest() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            GuardConfig::from_lookup(move |n| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == n)
                    .map(|(_, v)| v.to_string())
            })
        };
        assert_eq!(env(&[]).unwrap(), GuardConfig::default());
        assert_eq!(
            env(&[("LEDGER_LEASE_TTL_MS", "6000")]).unwrap(),
            GuardConfig::for_ttl(6 * S)
        );
        let c = env(&[
            ("LEDGER_LEASE_TTL_MS", "60000"),
            ("LEDGER_LEASE_RENEW_MS", "15000"),
            ("LEDGER_LEASE_SAFETY_MARGIN_MS", "10000"),
            ("LEDGER_LEASE_CONFIRM_TIMEOUT_MS", "1000"),
        ])
        .unwrap();
        assert_eq!(
            (
                c.lease_ttl,
                c.renew_every,
                c.safety_margin,
                c.confirm_timeout
            ),
            (60 * S, 15 * S, 10 * S, S)
        );
        assert!(matches!(
            env(&[("LEDGER_LEASE_TTL_MS", "soon")]),
            Err(ConfigError::NotANumber { .. })
        ));
        assert!(matches!(
            env(&[("LEDGER_LEASE_SAFETY_MARGIN_MS", "40000")]),
            Err(ConfigError::MarginTooLarge { .. })
        ));
    }

    #[tokio::test]
    async fn a_confirmed_lease_gives_its_epoch() {
        let (g, _) = guard();
        assert_eq!(confirmed(&g).await, Ok(Epoch(3)));
    }

    #[tokio::test]
    async fn with_the_control_plane_cut_it_serves_on_the_last_confirmation_until_the_margin() {
        let (g, clock) = guard();
        confirmed(&g).await.unwrap();
        clock.advance(12 * S);
        assert_eq!(cut(&g).await, Ok(Epoch(3)), "inside the window");
        clock.advance(11 * S + Duration::from_millis(999)); // 23.999 s after
        assert_eq!(cut(&g).await, Ok(Epoch(3)), "one millisecond to go");
        clock.advance(Duration::from_millis(1)); // 24 s: ttl 30 minus margin 6
        let err = cut(&g).await.unwrap_err();
        assert!(err.is_unavailable(), "{err}");
        assert!(
            matches!(&err, GuardError::Unavailable { last_confirmed: Some(d), .. } if *d == 24 * S),
            "{err}"
        );
    }

    #[tokio::test]
    async fn the_window_runs_from_when_the_confirmation_was_sent_not_when_it_came_back() {
        let (g, clock) = guard();
        let slow = async {
            clock.advance(2 * S);
            Ok(Epoch(3))
        };
        g.check_with(&acc(), slow).await.unwrap();
        clock.advance(22 * S); // 24 s after it was sent, 22 s after it came back
        assert!(cut(&g).await.unwrap_err().is_unavailable());
    }

    #[tokio::test]
    async fn a_node_that_never_confirmed_is_unavailable_not_serving() {
        let (g, _) = guard();
        let err = cut(&g).await.unwrap_err();
        assert!(matches!(
            err,
            GuardError::Unavailable {
                last_confirmed: None,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn a_slow_control_plane_counts_as_unreachable_and_never_as_a_lost_lease() {
        let clock = Arc::new(ManualClock::new());
        let config = GuardConfig {
            confirm_timeout: Duration::from_millis(20),
            ..GuardConfig::for_ttl(Duration::from_millis(300))
        };
        let g = LeaseGuard::new(node("a"), config, clock.clone()).unwrap();
        confirmed(&g).await.unwrap();
        let a = acc();
        let hung = g.check_with(&a, std::future::pending());
        assert_eq!(hung.await, Ok(Epoch(3)), "inside the window");
        clock.advance(config.serving_window());
        let err = g
            .check_with(&acc(), std::future::pending())
            .await
            .unwrap_err();
        assert!(err.is_unavailable(), "{err}");
    }

    #[tokio::test]
    async fn a_lapsed_lease_is_lost_and_the_old_confirmation_is_forgotten() {
        let (g, _) = guard();
        confirmed(&g).await.unwrap();
        let err = g
            .check_with(&acc(), async {
                Err(ControlError::LeaseExpired {
                    account: acc(),
                    node: node("a"),
                })
            })
            .await
            .unwrap_err();
        assert!(matches!(err, GuardError::LeaseLost { .. }), "{err}");
        assert!(
            cut(&g).await.unwrap_err().is_unavailable(),
            "a lost lease is not served on later"
        );
    }

    #[tokio::test]
    async fn not_home_is_refused_and_the_old_confirmation_is_forgotten() {
        let (g, _) = guard();
        confirmed(&g).await.unwrap();
        let err = g
            .check_with(&acc(), async {
                Err(ControlError::NotHome {
                    account: acc(),
                    node: node("a"),
                    home: node("b"),
                    epoch: Epoch(4),
                })
            })
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            GuardError::NotHome {
                epoch: Epoch(4),
                ..
            }
        ));
        assert!(cut(&g).await.unwrap_err().is_unavailable());
    }

    #[tokio::test]
    async fn against_the_in_memory_control_plane_a_cut_off_home_stops_then_learns_it_lost_the_lease()
     {
        let clock = Arc::new(ManualClock::new());
        let cp = InMemoryControlPlane::new(30 * S, clock.clone());
        let g = LeaseGuard::new(node("a"), GuardConfig::default(), clock.clone()).unwrap();
        cp.register(&acc(), &node("a")).await.unwrap();
        assert_eq!(g.renew(&cp).await, Ok(1));
        assert_eq!(g.check(&cp, &acc()).await, Ok(Epoch(1)));
        // The home is cut off: nothing renews. The control plane lets the lease run out.
        clock.advance(31 * S);
        let err = g.check(&cp, &acc()).await.unwrap_err();
        assert!(matches!(err, GuardError::LeaseLost { .. }), "{err}");
        // A promotion elsewhere makes the old home refuse as not home.
        cp.promote(&acc(), &node("b"), Epoch(1), 0).await.unwrap();
        assert!(matches!(
            g.check(&cp, &acc()).await,
            Err(GuardError::NotHome {
                epoch: Epoch(2),
                ..
            })
        ));
    }
}
