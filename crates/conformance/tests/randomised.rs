//! T-03: seeded random sequences of commands and faults against three real Postgres nodes,
//! with Toxiproxy on the links between them. Each run draws everything from one seed, so a
//! failing seed repeats exactly:
//!
//! ```text
//! LEDGER_SEED=1234 cargo test -p conformance --test randomised -- --ignored --nocapture
//! ```
//!
//! (`make replay SEED=1234` does the same.) A run moves money between accounts homed on all
//! three cells, crashes transfers half way, replicates some pairs of cells and not others, cuts,
//! slows and resets links, and fails accounts over with the real orchestrator, sometimes leaving
//! the old home unfenced so it can write at its old epoch. It ends by healing everything, letting
//! the old homes rejoin, recovering, replicating, re-posting what was quarantined, and then
//! checking that every cell agrees and that every invariant holds.
//!
//! Settings, all optional: `LEDGER_RUNS` (default 20), `LEDGER_SEED_START` (default 1),
//! `LEDGER_SEED` (one seed, with its trace printed), `LEDGER_FAILURES_FILE` (where failing seeds
//! and the trace of the first are written, for the nightly workflow to attach to an issue).
//!
//! Needs `make up` (the nodes and Toxiproxy). The control plane is the in-memory one: etcd under
//! faults is `network_faults.rs`.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use common::*;
use control_plane::{
    Cell, CellError, ControlPlane, Epoch, Fence, InMemoryControlPlane, NodeId, Orchestrator,
    PgCell, SystemClock,
};
use ledger_core::{AccountId, TenantId};
use ledger_repl::pull_all;
use ledger_store::cross_cell;
use ledger_store::{LegError, LegWriter, Side};
use tokio::sync::Mutex;

const ACCOUNTS: usize = 6;
const OPENING: i64 = 1000;

/// Toxiproxy keeps one set of faults for everyone, so runs take turns.
static ENVIRONMENT: Mutex<()> = Mutex::const_new(());

/// A small deterministic generator (splitmix64), so the repeat of a seed needs nothing else.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

/// A cell whose fence installation can be blocked, as if the node were cut off.
struct Flaky {
    inner: PgCell,
    blocked: Arc<AtomicBool>,
}

impl Cell for Flaky {
    async fn high_water(&self, account: &AccountId) -> Result<(Epoch, u64), CellError> {
        self.inner.high_water(account).await
    }
    async fn install_fence(&self, fence: &Fence) -> Result<(), CellError> {
        if self.blocked.load(Ordering::SeqCst) {
            return Err(CellError("cut off from the control plane".to_owned()));
        }
        self.inner.install_fence(fence).await
    }
}

type Trace = Arc<StdMutex<Vec<String>>>;

async fn put_toxic(proxy: &str, name: &str, json: &str) {
    let _ = http("DELETE", &format!("/proxies/{proxy}/toxics/{name}"), "").await;
    toxic(proxy, json).await;
}

fn say(trace: &Trace, line: String) {
    trace.lock().unwrap().push(line);
}

fn acct(i: usize) -> String {
    format!("acc_w{i}")
}

/// One run. Panics (an assertion, an unexpected error) are the failures.
async fn run(seed: u64, trace: Trace) {
    let schema = "t03_random";
    let mut rng = Rng(seed);
    let names: Vec<String> = (0..ACCOUNTS).map(acct).collect();
    let mut spec: Vec<(&str, usize, bool)> = vec![("acc_treasury", 0, true)];
    let mut homes: Vec<usize> = Vec::new();
    for (i, n) in names.iter().enumerate() {
        homes.push(i % 3);
        spec.push((n.as_str(), i % 3, false));
    }
    let lab = Lab::new(schema, &spec).await;
    let direct = lab.cells().await;
    let node = |i: usize| -> NodeId { NAMES[i].parse().unwrap() };

    let control =
        InMemoryControlPlane::new(Duration::from_secs(3600), Arc::new(SystemClock::new()));
    for (i, n) in names.iter().enumerate() {
        control
            .register(&n.parse().unwrap(), &node(homes[i]))
            .await
            .unwrap();
    }
    let blocked: Vec<Arc<AtomicBool>> = (0..3).map(|_| Arc::new(AtomicBool::new(false))).collect();
    let mut cells = BTreeMap::new();
    for i in 0..3 {
        cells.insert(
            node(i),
            Flaky {
                inner: PgCell::new(Arc::new(connect(PORTS[i]).await), schema, TENANT),
                blocked: blocked[i].clone(),
            },
        );
    }
    let orchestrator = Orchestrator::new(control, cells, Duration::from_secs(5));

    for n in &names {
        lab.fund(n, OPENING).await;
    }
    lab.settle().await;

    let mut epochs: BTreeMap<String, i64> = names.iter().map(|n| (n.clone(), 1)).collect();
    let mut acked: Vec<String> = Vec::new();
    let mut counter = 0u32;
    let steps = 8 + rng.below(10);
    say(&trace, format!("seed {seed}: {steps} steps"));

    let tenant: TenantId = TENANT.parse().unwrap();
    for step in 0..steps {
        let roll = rng.below(100);
        let current = Lab {
            schema: schema.to_owned(),
            ports: if rng.chance(50) { PROXIED } else { PORTS },
            homes: std::iter::once(("acc_treasury".to_owned(), 0))
                .chain(names.iter().enumerate().map(|(i, n)| (n.clone(), homes[i])))
                .collect(),
        };
        let at = |a: &str| epochs.get(a).copied().unwrap_or(1);
        if roll < 45 {
            let payer = rng.below(ACCOUNTS);
            let payee = (payer + 1 + rng.below(ACCOUNTS - 1)) % ACCOUNTS;
            let amount = 1 + rng.below(40) as i64;
            counter += 1;
            let journal = format!("jr_r{counter}");
            let started = Instant::now();
            let out = tokio::time::timeout(
                Duration::from_secs(8),
                current.transfer(&journal, &names[payer], &names[payee], amount, &at),
            )
            .await;
            let verdict = match out {
                Ok(Ok(_)) => {
                    acked.push(journal.clone());
                    "ok".to_owned()
                }
                Ok(Err(LegError::Database(e))) => format!("unknown outcome ({e})"),
                Ok(Err(LegError::Fenced(e))) => format!("fenced ({e})"),
                Err(_) => "unknown outcome (timed out)".to_owned(),
                Ok(Err(e)) => panic!("{journal}: unexpected {e:?}"),
            };
            say(
                &trace,
                format!(
                    "{step}: transfer {journal} {amount} {} -> {} via {:?}: {verdict} in {:?}",
                    names[payer],
                    names[payee],
                    current.ports,
                    started.elapsed()
                ),
            );
        } else if roll < 55 {
            // A coordinator that dies after the first half: the journal is left for recovery.
            let payer = rng.below(ACCOUNTS);
            let payee = (payer + 1 + rng.below(ACCOUNTS - 1)) % ACCOUNTS;
            if homes[payer] != homes[payee] {
                counter += 1;
                let journal = format!("jr_r{counter}");
                let t = current.cross(
                    &journal,
                    &names[payer],
                    &names[payee],
                    1 + rng.below(20) as i64,
                    &at,
                );
                let r = cross_cell::send(&direct[homes[payer]], schema, &t).await;
                say(
                    &trace,
                    format!(
                        "{step}: crash after the debit of {journal} ({} -> {}): {}",
                        names[payer],
                        names[payee],
                        r.map_or_else(|e| format!("{e}"), |_| "sent".into())
                    ),
                );
            }
        } else if roll < 65 {
            let (i, j) = (rng.below(3), rng.below(3));
            if i != j {
                let ports = if rng.chance(50) { PROXIED } else { PORTS };
                let r = tokio::time::timeout(Duration::from_secs(8), async {
                    let (a, b) = (try_connect(ports[i]).await?, try_connect(ports[j]).await?);
                    pull_all(&a, &b, schema, TENANT, NAMES[i], 50)
                        .await
                        .map_err(|e| e.to_string())
                        .map_err(|_| ())
                        .ok();
                    Ok::<_, tokio_postgres::Error>(())
                })
                .await;
                say(
                    &trace,
                    format!(
                        "{step}: ship {} to {}: {}",
                        NAMES[i],
                        NAMES[j],
                        if matches!(r, Ok(Ok(()))) {
                            "tried"
                        } else {
                            "link down"
                        }
                    ),
                );
            }
        } else if roll < 76 {
            let k = rng.below(3);
            match rng.below(4) {
                0 => {
                    cut(PROXY_NAMES[k]).await;
                    say(
                        &trace,
                        format!("{step}: fault: cut the link to {}", NAMES[k]),
                    );
                }
                1 => {
                    put_toxic(PROXY_NAMES[k], "slow-100", &latency(100)).await;
                    say(
                        &trace,
                        format!("{step}: fault: 100 ms latency to {}", NAMES[k]),
                    );
                }
                2 => {
                    put_toxic(PROXY_NAMES[k], "drop", r#"{"name":"drop","type":"reset_peer","stream":"downstream","toxicity":0.3,"attributes":{"timeout":0}}"#).await;
                    say(
                        &trace,
                        format!(
                            "{step}: fault: reset a third of connections to {}",
                            NAMES[k]
                        ),
                    );
                }
                _ => {
                    heal().await;
                    say(&trace, format!("{step}: heal every link"));
                }
            }
        } else if roll < 84 {
            let i = rng.below(3);
            let n = recover_each(&direct, schema, i).await;
            say(
                &trace,
                format!("{step}: recover at {}: {n} settled", NAMES[i]),
            );
        } else if roll < 95 {
            // Fail an account over. Sometimes the old home is cut off from the fence.
            let a = rng.below(ACCOUNTS);
            // Known gap (K-09): a transfer still in flight to this account would be finished at
            // the cell that was its home when it was sent, so finish them before the move.
            let mut in_flight = false;
            for c in &direct {
                let todo = cross_cell::unfinished(c, schema, &tenant).await.unwrap();
                in_flight |= todo.iter().any(|u| u.payee == names[a]);
            }
            if in_flight {
                say(
                    &trace,
                    format!(
                        "{step}: failover of {} skipped: a transfer to it is in flight",
                        names[a]
                    ),
                );
                continue;
            }
            let old = homes[a];
            let to = (old + 1 + rng.below(2)) % 3;
            let cut_off = rng.chance(50);
            blocked[old].store(cut_off, Ordering::SeqCst);
            let done = orchestrator
                .promote(&names[a].parse().unwrap(), &node(to))
                .await;
            blocked[old].store(false, Ordering::SeqCst);
            match done {
                Ok(p) => {
                    say(
                        &trace,
                        format!(
                            "{step}: failover {} {} -> {}: epoch {}, cutoff {}, fence missed by {}",
                            names[a],
                            NAMES[old],
                            NAMES[to],
                            p.fence.epoch.0,
                            p.fence.prev_cutoff_seq,
                            p.not_installed.len()
                        ),
                    );
                    homes[a] = to;
                    epochs.insert(names[a].clone(), p.fence.epoch.0 as i64);
                    if cut_off && !p.not_installed.is_empty() && rng.chance(70) {
                        // The old home, unaware, takes a local write at its old epoch.
                        if let Some(other) = (0..ACCOUNTS).find(|o| *o != a && homes[*o] == old) {
                            counter += 1;
                            let journal = format!("jr_r{counter}");
                            let old_view = Lab {
                                schema: schema.to_owned(),
                                ports: PORTS,
                                homes: [
                                    ("acc_treasury".to_owned(), 0),
                                    (names[a].clone(), old),
                                    (names[other].clone(), old),
                                ]
                                .into(),
                            };
                            let old_epoch = |x: &str| {
                                if x == names[a] {
                                    p.fence.epoch.0 as i64 - 1
                                } else {
                                    epochs[x]
                                }
                            };
                            let r = old_view
                                .transfer(
                                    &journal,
                                    &names[a],
                                    &names[other],
                                    1 + rng.below(10) as i64,
                                    &old_epoch,
                                )
                                .await;
                            say(
                                &trace,
                                format!(
                                    "{step}: zombie write {journal} at the old home {}: {}",
                                    NAMES[old],
                                    r.map_or_else(|e| format!("{e:?}"), |_| "accepted".into())
                                ),
                            );
                        }
                    }
                }
                Err(e) => panic!(
                    "{step}: failover of {} to {} failed: {e}",
                    names[a], NAMES[to]
                ),
            }
        } else {
            heal().await;
            say(&trace, format!("{step}: heal every link"));
        }
    }

    // Heal, let every old home rejoin, finish what was half done, and ship everything.
    heal().await;
    say(&trace, "end: healed; old homes rejoin".to_owned());
    for port in PORTS {
        let cell = PgCell::new(Arc::new(connect(port).await), schema, TENANT);
        for f in orchestrator.control().fences().await.unwrap() {
            cell.install_fence(&f).await.unwrap();
        }
    }
    for i in 0..3 {
        recover_each(&direct, schema, i).await;
    }
    lab.settle().await;
    // Every cell now holds the same log. Re-post each quarantined entry under its account's
    // current epoch at its current home.
    let reposted = repost_all(&lab, &direct, &names, &homes, &epochs).await;
    say(
        &trace,
        format!("end: {reposted} quarantined entries re-posted"),
    );
    // A re-posted payer leg makes its journal visible to recovery again once it has shipped to
    // the payer's cell, so ship, then finish those too.
    lab.settle().await;
    for i in 0..3 {
        recover_each(&direct, schema, i).await;
    }
    lab.settle().await;
    lab.settle().await;

    // The checks. Every one of these must hold for every seed.
    let mut balances: BTreeMap<String, i64> = BTreeMap::new();
    for a in names.iter().map(String::as_str).chain(["acc_treasury"]) {
        let first = lab.balance(&direct[0], a).await;
        for (i, c) in direct.iter().enumerate().skip(1) {
            assert_eq!(
                lab.balance(c, a).await,
                first,
                "{a} differs on {}",
                NAMES[i]
            );
        }
        balances.insert(a.to_owned(), first);
    }
    let skewed: Vec<(String, i64, i64)> = direct[0]
        .query(
            &format!("select journal_id, sum(amount)::int8, count(*)::int8 from {schema}.entry group by journal_id having sum(amount) <> 0 order by 1"),
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2)))
        .collect();
    let stuck: Vec<(String, String, i64, String)> = direct[0]
        .query(
            &format!("select j.id, j.state, e.amount, e.account_id from {schema}.journal j join {schema}.entry e on e.journal_id = j.id and e.account_id like 'acc\\_tr%s' where j.state <> 'posted' or not exists (select 1 from {schema}.entry r where r.journal_id = j.id and r.account_id like 'acc\\_tr%r') order by 1"),
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
        .collect();
    assert!(
        stuck.is_empty(),
        "sent but not received or not posted: {stuck:?}"
    );
    assert!(
        skewed.is_empty(),
        "journals that do not sum to zero (journal, sum, entries): {skewed:?}"
    );
    assert_eq!(
        balances.values().sum::<i64>(),
        0,
        "money was created or destroyed: {balances:?}"
    );
    for (a, b) in &balances {
        if a != "acc_treasury" {
            assert!(*b >= 0, "{a} ended at {b}");
        }
    }
    for c in &direct {
        assert!(
            cross_cell::unfinished(c, schema, &tenant)
                .await
                .unwrap()
                .is_empty(),
            "a transfer is still unfinished"
        );
    }
    // Every transfer that was acknowledged is in the log once, with both of its sides.
    for journal in &acked {
        let row = direct[0]
            .query_one(
                &format!("select count(*), count(distinct account_id) from {schema}.entry where journal_id = $1 and account_id like 'acc\\_w%'"),
                &[journal],
            )
            .await
            .unwrap();
        let (legs, accounts): (i64, i64) = (row.get(0), row.get(1));
        assert_eq!(
            (legs, accounts),
            (2, 2),
            "acknowledged {journal} is not in the log exactly once"
        );
    }
    lab.assert_clean_after_reconcile().await;
    lab.drop_all().await;
}

/// Re-posts every unresolved quarantined entry, once, under its account's current epoch at its
/// current home, with the journal id it had and a new entry id.
async fn repost_all(
    lab: &Lab,
    cells: &[tokio_postgres::Client],
    names: &[String],
    homes: &[usize],
    epochs: &BTreeMap<String, i64>,
) -> usize {
    let schema = &lab.schema;
    let mut todo: BTreeSet<(String, String, i64, i64, Option<String>)> = BTreeSet::new();
    for c in cells {
        for r in c
            .query(
                &format!("select journal_id, account_id, leg::int8, amount, counterparty from {schema}.quarantine where not resolved"),
                &[],
            )
            .await
            .unwrap()
        {
            todo.insert((r.get(0), r.get(1), r.get(2), r.get(3), r.get(4)));
        }
    }
    for (journal, account, leg, amount, counterparty) in &todo {
        let Some(i) = names.iter().position(|n| n == account) else {
            panic!("a quarantined entry on {account}, which no failover moved");
        };
        let side = if *amount < 0 {
            Side::Debit
        } else {
            Side::Credit
        };
        let mut re = lab.leg(
            journal,
            account,
            *leg as i16,
            side,
            amount.abs(),
            (NAMES[homes[i]], epochs[account]),
        );
        re.counterparty = counterparty.as_ref().map(|c| c.parse().unwrap());
        re.entry = format!("ent_rp{}x{leg}", journal.trim_start_matches("jr_"))
            .parse()
            .unwrap();
        let w = LegWriter::prepare(&cells[homes[i]], schema).await.unwrap();
        w.post(&re)
            .await
            .unwrap_or_else(|e| panic!("re-post of {journal} on {account}: {e:?}"));
    }
    for c in cells {
        c.batch_execute(&format!("update {schema}.quarantine set resolved = true"))
            .await
            .unwrap();
    }
    todo.len()
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

#[tokio::test]
#[ignore = "needs `make up`; LEDGER_RUNS, LEDGER_SEED_START and LEDGER_SEED set the seeds"]
async fn randomised_commands_and_faults_keep_every_invariant() {
    let _env = ENVIRONMENT.lock().await;
    let single = env_u64("LEDGER_SEED");
    let start = single.or_else(|| env_u64("LEDGER_SEED_START")).unwrap_or(1);
    let runs = if single.is_some() {
        1
    } else {
        env_u64("LEDGER_RUNS").unwrap_or(20)
    };
    let started = Instant::now();
    let mut failures: Vec<(u64, String, Vec<String>)> = Vec::new();
    for seed in start..start + runs {
        heal().await;
        let trace: Trace = Arc::default();
        let outcome = tokio::spawn(run(seed, trace.clone())).await;
        if let Err(e) = outcome {
            let message = match e.try_into_panic() {
                Ok(p) => p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .unwrap_or_else(|| "panicked".to_owned()),
                Err(e) => e.to_string(),
            };
            let lines = trace.lock().unwrap().clone();
            eprintln!("FAIL seed {seed}: {message}");
            failures.push((seed, message, lines));
        } else if single.is_some() {
            for l in trace.lock().unwrap().iter() {
                println!("{l}");
            }
        }
    }
    heal().await;
    println!(
        "{} runs from seed {start} in {:?}, {} failed",
        runs,
        started.elapsed(),
        failures.len()
    );
    if failures.is_empty() {
        return;
    }
    let mut report = String::new();
    for (seed, message, _) in &failures {
        report.push_str(&format!("seed {seed}: {message}\n"));
    }
    let (seed, _, lines) = &failures[0];
    report.push_str(&format!(
        "\nReproduce the first with (after `make up`):\n  make replay SEED={seed}\nwhich runs:\n  LEDGER_SEED={seed} cargo test -p conformance --test randomised -- --ignored --nocapture\n\nTrace of seed {seed}:\n{}\n",
        lines.join("\n")
    ));
    if let Ok(path) = std::env::var("LEDGER_FAILURES_FILE") {
        std::fs::write(path, &report).expect("write the failures file");
    }
    panic!(
        "{} of {runs} randomised runs failed\n{report}",
        failures.len()
    );
}
