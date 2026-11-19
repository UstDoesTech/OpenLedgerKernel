//! The shared suite against the in-memory implementation.

mod suite;

use std::sync::Arc;
use std::time::Duration;

use control_plane::{InMemoryControlPlane, ManualClock};

struct MemoryRig {
    clock: Arc<ManualClock>,
    cp: InMemoryControlPlane,
}

const TTL: Duration = Duration::from_secs(30);

impl MemoryRig {
    fn new() -> Self {
        let clock = Arc::new(ManualClock::new());
        let cp = InMemoryControlPlane::new(TTL, clock.clone());
        MemoryRig { clock, cp }
    }
}

impl suite::Rig for MemoryRig {
    type Cp = InMemoryControlPlane;
    fn cp(&self) -> &Self::Cp {
        &self.cp
    }
    fn ttl(&self) -> Duration {
        TTL
    }
    async fn elapse(&self, by: Duration) {
        self.clock.advance(by);
    }
}

macro_rules! memory_suite_tests {
    ($($name:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() {
                let rig = MemoryRig::new();
                suite::$name(&rig).await;
            }
        )+
    };
}

memory_suite_tests!(
    register_starts_at_epoch_one_with_a_first_fence,
    registering_twice_or_using_an_unknown_account_is_an_error,
    a_lease_runs_out_unless_the_home_keeps_using_it,
    not_home_takes_precedence_over_expired,
    a_forced_expiry_is_never_revived_by_the_heartbeat,
    the_heartbeat_skips_homes_that_cannot_reach_the_control_plane,
    the_heartbeat_restores_a_lease_that_lapsed_on_its_own,
    promote_bumps_the_epoch_records_the_cutoff_and_moves_the_home,
    the_cutoff_epoch_may_be_older_than_current_but_not_newer,
    racing_promotions_each_take_a_distinct_epoch,
    listings_are_ordered,
    listings_are_ordered_for_names_that_share_a_prefix,
    the_fence_feed_replays_history_then_follows_live,
    follow_fences_installs_each_fence_until_the_installer_stops,
    renew_homed_keeps_a_nodes_own_leases_alive,
    draining_is_a_marker_that_can_be_set_repeated_and_cleared,
    draining_leaves_homes_and_leases_alone,
);
