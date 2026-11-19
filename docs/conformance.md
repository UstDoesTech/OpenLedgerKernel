# Conformance: the Rust kernel against the Python reference

The Python harness (`ledger/`, `tests/test_active_active.py`) is the reference implementation. The
Rust kernel must behave the same way in every scenario it covers. This page lists each pair, how
the Rust test differs where it has to, and how we know the suite can fail.

Run the Rust suite against three local nodes (`make up`, or `scripts/local_nodes.sh start`):

```
cargo test -p conformance --test active_active -- --ignored
```

## The scenarios

| Python scenario | Rust test (`crates/conformance/tests/active_active.rs`) | What the Rust test shows |
| --- | --- | --- |
| `test_transfer_is_idempotent` | `transfer_is_idempotent` | A retry through fresh connections changes nothing, every cell ends with 70 and 30, and the same journal with another amount is refused. |
| `test_concurrent_debits_never_overdraw` | `concurrent_debits_never_overdraw` | 20 racing transfers of 10 against 100, each on its own connections: exactly 10 settle and 10 are declined. |
| `test_only_the_home_with_a_valid_lease_can_debit` | `only_the_home_with_a_valid_lease_can_debit` | The control plane refuses a node that is not home or whose lease expired, and the database refuses a writer on a stale epoch. |
| `test_replication_is_ordered_idempotent_and_buffers_gaps` | `replication_is_ordered_idempotent_and_buffers_gaps` | An entry out of order is buffered and drains behind its predecessor, redelivery is a duplicate, and a relay that forgot its position applies nothing twice. |
| `test_crash_between_legs_is_recovered_exactly_once` | `crash_between_legs_is_recovered_exactly_once` | A transfer left after its first half is completed once by recovery, and a second recovery finds nothing. |
| `test_credit_to_unreachable_home_stays_pending_then_completes` | `credit_to_unreachable_home_stays_pending_then_completes` | Across a cut link the credit cannot complete and the journal stays pending, then recovery completes it once. |
| `test_failover_async_tail_is_quarantined_then_reconciled` | `failover_async_tail_is_quarantined_then_reconciled` | The drill: the standby is behind, three entries are quarantined, the old home is fenced, and the books end where the Python drill ends. |
| `test_failover_with_strong_tier_loses_nothing` | `failover_with_strong_tier_loses_nothing` | The same drill with the tail acknowledged first: the standby is level, and only the zombie's write is quarantined. |
| `test_random_concurrent_workload_converges` | `random_concurrent_workload_converges` | Twelve concurrent workers move money between six accounts homed on three cells, and every cell ends with the same balances and a clean invariant check. |
| `test_healthy_home_keeps_its_lease_through_idle_periods` | `healthy_home_keeps_its_lease_through_idle_periods` | A home that heartbeats keeps its lease through a long idle spell, a lease expired on purpose is not revived, and a home that stops renewing loses it. |

## Where the Rust test has to differ

- **No API layer yet.** The Python API checks the lease and then writes. The Rust kernel has the
  parts: the control plane decides who is home and whether the lease holds, and the database
  refuses a stale epoch. The lease scenarios test both. The two meet in the API, which is a later
  task.
- **The failover drills use the orchestrator.** Promotion is `Orchestrator::promote` with a Postgres
  cell for each node. The old home is left out of the cells the orchestrator can reach, as an
  isolated node would be, and gets the fence when it rejoins.
- **Reconciliation is done by the test.** The Python drill ends by re-posting quarantined entries.
  The Rust reconciliation job is a later task, so the drill re-posts each quarantined entry under
  the current epoch, keyed by its original journal id and with a new entry id, as
  [`design.md`](design.md#failover) describes. A quarantined entry is never merged back, and the
  database refuses it if the old entry id is reused.
- **Heartbeats are in the control plane.** The idle-lease scenario runs on the in-memory control
  plane with a manual clock. The same cases run against etcd in `crates/control-plane/tests`.

## Network faults

`crates/conformance/tests/network_faults.rs` runs the same cells and a real etcd through Toxiproxy
(`make up` starts it, with a proxy in front of each Postgres node on 15431 to 15433 and one in
front of etcd on 12379). Peer traffic goes through the proxies and a node's own clients go direct,
so a cut link leaves a cell reachable to the people next to it, as a partition does. Each scenario
ends by settling, reconciling where entries were quarantined, and running the invariant check on
every cell.

```
cargo test -p conformance --features etcd --test network_faults -- --ignored
```

| Scenario | Fault | What it shows |
| --- | --- | --- |
| `a_home_cut_from_its_peers_and_the_control_plane_is_replaced_and_comes_back_fenced` | The home's proxy and the etcd proxy are cut | From the home the control plane is *unavailable*, which differs from a lost lease. The lease lapses at etcd, the orchestrator promotes the standby and names the cut home as not fenced, the isolated home takes a write at its old epoch, and on rejoin the fence quarantines the tail and the zombie write. Nothing is lost and the old home is refused. |
| `serving_continues_on_valid_leases_when_only_the_control_plane_is_cut` | Only etcd is cut | Cross-cell and local writes carry on, a promotion attempt is refused and changes nothing, and after the heal the lease is still valid. The node's lease guard serves on its last confirmation while cut, as the data path does. Cut for longer than the ttl, the guard stops on its own timing before the lease lapses, as *unavailable* and never as a lost lease. Only once etcd is reachable again does the control plane say the lease was lost. |
| `two_hundred_milliseconds_of_latency_slows_everything_and_loses_nothing` | 200 ms on every node link and on etcd | Six transfers take at least six times 200 ms, the control plane answers slowly, and nothing is lost or left unfinished. |
| `dropped_connections_are_retried_to_exactly_once_and_recovered` | A third of the connections to every node are reset part-way | Twenty transfers, each retried under the same journal id, all land exactly once. Recovery finishes any that were left half done. |

TCP does not lose packets: it resets, stalls or retransmits. Dropped packets are therefore tested
as reset connections, which is what an application sees when a path drops enough of them.

### What this does not prove yet

The data path never talks to etcd: writes go to Postgres, and the database refuses a stale epoch.
The lease guard (`LeaseGuard` in `crates/control-plane`) is what a node asks before it serves an
account, and the scenario above asserts it against a real etcd with the ttl scaled to 6 seconds
(serving window 4.8 seconds). Wiring the guard into the node's request path belongs to the node
agent and the command API, not to this suite.

## Randomised commands and faults

`crates/conformance/tests/randomised.rs` runs the same cells under a seeded random mix of commands
and faults, then checks the invariants. One seed fixes the plan: the number of steps (8 to 17) and
each step. The steps are:

- a transfer, directly or through Toxiproxy, between two of six customer accounts (some across
  cells, some within one), with a time limit so a stalled link gives an unknown outcome;
- a crash after the first half of a cross-cell transfer;
- a partial shipment between two cells;
- a network fault: a cut link, 100 ms of latency, or a third of connections reset;
- a recovery pass for one payer cell;
- a failover of one account through the orchestrator, sometimes with the old home cut off from
  the fence and followed by a write at the old epoch (a zombie write).

At the end every link is healed, every fence reaches every cell, unfinished transfers are
recovered, everything is shipped, each quarantined entry is re-posted at the account's current
home under its current epoch, and the cells settle again. Then it checks that:

1. every cell holds the same balance for every account;
2. every journal's entries sum to zero, and customer balances sum to the opening balances;
3. no customer balance is negative;
4. no transfer is unfinished or sent without being received;
5. every transfer that was acknowledged is in the log once, with both sides;
6. the cell checks (`check_cell`) are clean once reconciled.

A failure prints the seed, the message and the step trace.

**Repeat a seed.** After `make up`:

```
make replay SEED=123
```

**Run a range.** `LEDGER_RUNS=1000 LEDGER_SEED_START=1 cargo test -p conformance --test randomised -- --ignored --nocapture`.
`LEDGER_FAILURES_FILE=path` writes the failing seeds and the trace of the first.

**Nightly.** `.github/workflows/nightly.yml` runs 1,000 seeds every night, starting where the
night before stopped. If any seed fails it opens an issue with the failing seeds, the
`make replay` command and the trace, and keeps the same text as a build artefact. A pull request
runs the first 20 seeds as part of the `integration` job.

**What it does not do.** It uses the in-memory control plane (etcd under faults is
`network_faults.rs`), and it does not run against Neon, because it needs Toxiproxy. It does not
fail over an account while a transfer to it is still in flight: the transfer would be finished
at the cell that was its home when it was sent (task K-09). Faults are injected by the proxy, so
a run is repeatable in what it does but not to the millisecond: timing can change which unknown
outcomes happen, and the checks hold for all of them.

## Does the suite fail when it should?

`scripts/mutation_check.sh` breaks one guarantee at a time in the migrations, runs the suite and
expects it to fail. It restores the migrations afterwards and exits non-zero if the suite passes
with a guarantee removed. CI runs it in the `integration` job.

| Mutation | Caught by |
| --- | --- |
| A stale epoch is accepted on write (fencing) | both failover drills, and the lease scenario |
| The sweep never quarantines an entry a fence voids (fencing) | both failover drills |
| `post_leg` reads the account without locking it (row lock) | the concurrent debits and the random workload |

The row lock shows up as a race, so the script repeats the run (`MUTATION_RUNS`, default 3) and
counts any failing run as a catch.
