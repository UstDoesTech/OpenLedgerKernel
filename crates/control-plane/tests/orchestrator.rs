//! The orchestrator with fake cells, so every refusal and every partial failure can be shown
//! without a database. `failover_local.rs` runs it against real Postgres and real etcd.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use control_plane::{
    Cell, CellError, ControlPlane, Epoch, FailoverError, Fence, InMemoryControlPlane, ManualClock,
    NodeId, Orchestrator,
};
use ledger_core::AccountId;

fn acc(s: &str) -> AccountId {
    format!("acc_{s}").parse().unwrap()
}
fn node(s: &str) -> NodeId {
    s.parse().unwrap()
}

#[derive(Clone)]
struct Fake {
    high_water: Result<(Epoch, u64), String>,
    install: Result<(), String>,
    delay: Duration,
    installed: Arc<Mutex<Vec<Fence>>>,
}

impl Fake {
    fn level_at(epoch: u64, seq: u64) -> Self {
        Fake {
            high_water: Ok((Epoch(epoch), seq)),
            install: Ok(()),
            delay: Duration::ZERO,
            installed: Arc::default(),
        }
    }
    fn installed(&self) -> Vec<Fence> {
        self.installed.lock().unwrap().clone()
    }
}

impl Cell for Fake {
    async fn high_water(&self, _: &AccountId) -> Result<(Epoch, u64), CellError> {
        tokio::time::sleep(self.delay).await;
        self.high_water.clone().map_err(CellError)
    }
    async fn install_fence(&self, f: &Fence) -> Result<(), CellError> {
        tokio::time::sleep(self.delay).await;
        self.install.clone().map_err(CellError)?;
        self.installed.lock().unwrap().push(f.clone());
        Ok(())
    }
}

struct Rig {
    orchestrator: Orchestrator<InMemoryControlPlane, Fake>,
    cells: BTreeMap<NodeId, Fake>,
}

async fn rig(cells: [(&str, Fake); 3], timeout: Duration) -> Rig {
    let cp = InMemoryControlPlane::new(Duration::from_secs(30), Arc::new(ManualClock::new()));
    cp.register(&acc("p"), &node("aws-lon-01")).await.unwrap();
    let cells: BTreeMap<NodeId, Fake> = cells.into_iter().map(|(n, f)| (node(n), f)).collect();
    Rig {
        orchestrator: Orchestrator::new(cp, cells.clone(), timeout),
        cells,
    }
}

fn three(standby: Fake, other: Fake) -> [(&'static str, Fake); 3] {
    [
        ("aws-lon-01", Fake::level_at(1, 5)),
        ("azure-lon-01", standby),
        ("gcp-lon-01", other),
    ]
}

#[tokio::test]
async fn promotion_uses_the_standbys_own_high_water_mark_as_the_cutoff() {
    let r = rig(
        three(Fake::level_at(1, 3), Fake::level_at(1, 5)),
        Duration::from_secs(1),
    )
    .await;
    let done = r
        .orchestrator
        .promote(&acc("p"), &node("azure-lon-01"))
        .await
        .unwrap();
    assert_eq!(done.fence.epoch, Epoch(2));
    assert_eq!(done.fence.home, node("azure-lon-01"));
    assert_eq!(
        (done.fence.prev_epoch, done.fence.prev_cutoff_seq),
        (Epoch(1), 3)
    );
    assert_eq!(done.standby_high_water, (Epoch(1), 3));
    assert_eq!(done.previous_home, node("aws-lon-01"));
    let home = r.orchestrator.control().home(&acc("p")).await.unwrap();
    assert_eq!(home, (node("azure-lon-01"), Epoch(2)));
}

#[tokio::test]
async fn the_fence_goes_to_every_cell_and_the_new_home_comes_first() {
    let r = rig(
        three(Fake::level_at(1, 5), Fake::level_at(1, 5)),
        Duration::from_secs(1),
    )
    .await;
    let done = r
        .orchestrator
        .promote(&acc("p"), &node("azure-lon-01"))
        .await
        .unwrap();
    assert_eq!(done.installed[0], node("azure-lon-01"));
    assert_eq!(done.installed.len(), 3);
    assert!(done.not_installed.is_empty());
    for (name, cell) in &r.cells {
        assert_eq!(cell.installed(), vec![done.fence.clone()], "{name}");
    }
}

#[tokio::test]
async fn an_unreachable_cell_is_reported_and_does_not_stop_the_promotion() {
    let mut cut = Fake::level_at(1, 5);
    cut.install = Err("connection refused".into());
    let r = rig(three(Fake::level_at(1, 5), cut), Duration::from_secs(1)).await;
    let done = r
        .orchestrator
        .promote(&acc("p"), &node("azure-lon-01"))
        .await
        .unwrap();
    assert_eq!(done.installed.len(), 2);
    assert_eq!(
        done.not_installed,
        vec![(node("gcp-lon-01"), "connection refused".to_owned())]
    );
    // The control plane holds the fence, so the cut-off cell installs it when it rejoins.
    assert_eq!(r.orchestrator.control().fences().await.unwrap().len(), 2);
}

#[tokio::test]
async fn if_the_standbys_high_water_mark_cannot_be_read_nothing_is_promoted() {
    let mut down = Fake::level_at(1, 5);
    down.high_water = Err("no route to host".into());
    let r = rig(three(down, Fake::level_at(1, 5)), Duration::from_secs(1)).await;
    let err = r
        .orchestrator
        .promote(&acc("p"), &node("azure-lon-01"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, FailoverError::StandbyUnreachable { .. }),
        "{err}"
    );
    let cp = r.orchestrator.control();
    assert_eq!(
        cp.home(&acc("p")).await.unwrap(),
        (node("aws-lon-01"), Epoch(1))
    );
    assert_eq!(cp.fences().await.unwrap().len(), 1, "no fence was issued");
    for cell in r.cells.values() {
        assert!(cell.installed().is_empty());
    }
}

#[tokio::test]
async fn a_standby_that_does_not_answer_in_time_is_unreachable_not_waited_for() {
    let mut slow = Fake::level_at(1, 5);
    slow.delay = Duration::from_secs(30);
    let r = rig(
        three(slow, Fake::level_at(1, 5)),
        Duration::from_millis(200),
    )
    .await;
    let started = Instant::now();
    let err = r
        .orchestrator
        .promote(&acc("p"), &node("azure-lon-01"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, FailoverError::StandbyUnreachable { .. }),
        "{err}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn a_cell_that_hangs_holds_the_promotion_up_for_the_timeout_and_no_longer() {
    let mut hangs = Fake::level_at(1, 5);
    hangs.delay = Duration::from_secs(30);
    let r = rig(
        three(Fake::level_at(1, 5), hangs),
        Duration::from_millis(300),
    )
    .await;
    let done = r
        .orchestrator
        .promote(&acc("p"), &node("azure-lon-01"))
        .await
        .unwrap();
    assert!(done.took < Duration::from_secs(2), "{:?}", done.took);
    assert_eq!(done.not_installed.len(), 1);
    assert!(done.not_installed[0].1.contains("no answer within"));
}

#[tokio::test]
async fn the_other_cells_are_fenced_at_the_same_time_not_one_after_another() {
    let mut slow_a = Fake::level_at(1, 5);
    slow_a.delay = Duration::from_millis(400);
    let mut slow_b = Fake::level_at(1, 5);
    slow_b.delay = Duration::from_millis(400);
    let r = rig(
        [
            ("aws-lon-01", slow_a),
            ("azure-lon-01", Fake::level_at(1, 5)),
            ("gcp-lon-01", slow_b),
        ],
        Duration::from_secs(2),
    )
    .await;
    let done = r
        .orchestrator
        .promote(&acc("p"), &node("azure-lon-01"))
        .await
        .unwrap();
    assert_eq!(done.installed.len(), 3);
    assert!(
        done.took < Duration::from_millis(700),
        "two 400 ms cells took {:?}",
        done.took
    );
}

#[tokio::test]
async fn a_new_home_that_cannot_take_the_fence_is_an_error_that_carries_the_fence() {
    let mut bad = Fake::level_at(1, 5);
    bad.install = Err("disk full".into());
    let r = rig(three(bad, Fake::level_at(1, 5)), Duration::from_secs(1)).await;
    let err = r
        .orchestrator
        .promote(&acc("p"), &node("azure-lon-01"))
        .await
        .unwrap_err();
    let FailoverError::NewHomeNotFenced {
        fence,
        cell,
        reason,
        ..
    } = err
    else {
        panic!("{err}")
    };
    assert_eq!(
        (cell, reason.as_str(), fence.epoch),
        (node("azure-lon-01"), "disk full", Epoch(2))
    );
    // The control plane did promote, so the new home installs the fence from the feed later.
    let cp = r.orchestrator.control();
    assert_eq!(cp.home(&acc("p")).await.unwrap().1, Epoch(2));
    assert!(
        r.cells[&node("gcp-lon-01")].installed().is_empty(),
        "no other cell is fenced first"
    );
}

#[tokio::test]
async fn promoting_the_current_home_an_unknown_cell_or_an_unknown_account_is_refused() {
    let r = rig(
        three(Fake::level_at(1, 5), Fake::level_at(1, 5)),
        Duration::from_secs(1),
    )
    .await;
    assert!(matches!(
        r.orchestrator.promote(&acc("p"), &node("aws-lon-01")).await,
        Err(FailoverError::AlreadyHome { .. })
    ));
    assert!(matches!(
        r.orchestrator.promote(&acc("p"), &node("nowhere-01")).await,
        Err(FailoverError::UnknownCell(_))
    ));
    assert!(matches!(
        r.orchestrator
            .promote(&acc("zz"), &node("azure-lon-01"))
            .await,
        Err(FailoverError::Control(
            control_plane::ControlError::UnknownAccount(_)
        ))
    ));
    assert_eq!(r.orchestrator.control().fences().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_second_promotion_takes_the_next_epoch_from_the_new_home() {
    let r = rig(
        three(Fake::level_at(1, 5), Fake::level_at(1, 5)),
        Duration::from_secs(1),
    )
    .await;
    r.orchestrator
        .promote(&acc("p"), &node("azure-lon-01"))
        .await
        .unwrap();
    let again = r.orchestrator.promote(&acc("p"), &node("gcp-lon-01")).await;
    // The fake standby still reports epoch 1, which is older than the current epoch, and an
    // older cutoff epoch is allowed: the entries it lacks are void.
    let again = again.unwrap();
    assert_eq!(again.fence.epoch, Epoch(3));
}

#[tokio::test]
async fn drain_and_undrain_are_markers_in_the_control_plane() {
    let r = rig(
        three(Fake::level_at(1, 5), Fake::level_at(1, 5)),
        Duration::from_secs(1),
    )
    .await;
    r.orchestrator.drain(&node("aws-lon-01")).await.unwrap();
    let cp = r.orchestrator.control();
    assert_eq!(cp.draining().await.unwrap(), [node("aws-lon-01")].into());
    assert_eq!(
        cp.home(&acc("p")).await.unwrap().1,
        Epoch(1),
        "draining promotes nothing"
    );
    r.orchestrator.undrain(&node("aws-lon-01")).await.unwrap();
    assert!(cp.draining().await.unwrap().is_empty());
}
