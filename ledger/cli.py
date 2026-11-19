"""python -m ledger.cli <command>

  init    create the ledger_lab schema on every node
  demo    create accounts, run cross-region transfers from every region, verify invariants
  drill   failover drill: home dies with acked writes in flight, standby promoted, zombie fenced
  probe   latency from THIS machine to each node, plus transfer / strong-tier / replication cost
"""
from __future__ import annotations

import argparse
import os
import random
import statistics
import sys
import time
import uuid

from . import checks
from .cluster import Cluster
from .config import is_local, load_dsns
from .scenarios import failover_drill


def _cluster() -> Cluster:
    dsns = load_dsns()
    if not is_local(dsns):
        print("Nodes: " + ", ".join(f"{n} ({d.split('@')[-1].split('/')[0]})" for n, d in dsns.items()))
    return Cluster(dsns)


def _confirm_reset(cl: Cluster) -> None:
    if not is_local(load_dsns()) and os.environ.get("LEDGER_ALLOW_REMOTE") != "1":
        sys.exit("This empties the ledger_lab schema on every node. Re-run with "
                 "LEDGER_ALLOW_REMOTE=1 to confirm. Nothing outside that schema is touched.")


def cmd_init(args):
    cl = _cluster()
    cl.init_schema()
    print(f"schema ready on {', '.join(cl.names)}")


def cmd_demo(args):
    cl = _cluster()
    _confirm_reset(cl)
    cl.reset()
    a, b, c = cl.names[:3]
    cl.create_account("TREASURY", a, allow_negative=True)
    for acct, home in (("alice", a), ("bob", b), ("carol", c)):
        cl.create_account(acct, home)
        cl.apis[a].fund(acct, 10_000)
    cl.settle()
    rnd = random.Random(7)
    lat: dict[str, list[float]] = {n: [] for n in cl.names}
    people = ["alice", "bob", "carol"]
    for i in range(args.transfers):
        region = cl.names[i % 3]
        src, dst = rnd.sample(people, 2)
        t0 = time.perf_counter()
        cl.apis[region].transfer(uuid.uuid4(), src, dst, rnd.randint(1, 50))
        lat[region].append((time.perf_counter() - t0) * 1000)
    cl.settle()
    print(f"\n{args.transfers} transfers, requests landing round-robin on each region")
    for n in cl.names:
        print(f"  request landed at {n:>8}: median {statistics.median(lat[n]):7.1f} ms")
    print("\nbalances (identical on every node):")
    for n in cl.names:
        rows = cl.nodes[n].q("select id, balance from account order by id")
        print(f"  {n:>8}: " + "  ".join(f"{r['id']}={r['balance']}" for r in rows))
    rep = checks.report(cl)
    print("\ninvariants:", "ALL HOLD" if checks.healthy(rep) else rep)
    sys.exit(0 if checks.healthy(rep) else 1)


def cmd_drill(args):
    cl = _cluster()
    _confirm_reset(cl)
    print(f"--- failover drill, strong tier for the tail: {args.strong} ---")
    obs = failover_drill(cl, strong_tail=args.strong, say=lambda *a: print(" ", *a))
    print("\nobservations")
    for k in ("legit_write_on_isolated", "zombie_after_rejoin", "new_epoch", "balances"):
        print(f"  {k}: {obs[k]}")
    print(f"  quarantined and reconciled: {obs['quarantined']}")
    rep = checks.report(cl)
    print("\ninvariants after reconcile:", "ALL HOLD" if checks.healthy(rep) else rep)
    sys.exit(0 if checks.healthy(rep) else 1)


def _timed(fn, n):
    out = []
    for _ in range(n):
        t0 = time.perf_counter()
        fn()
        out.append((time.perf_counter() - t0) * 1000)
    out.sort()
    return statistics.median(out), out[max(0, int(len(out) * 0.95) - 1)]


def cmd_probe(args):
    cl = _cluster()
    _confirm_reset(cl)
    cl.reset()
    n = args.samples
    names = cl.names
    print(f"client -> node, {n} samples each (this includes the path from THIS machine; run it "
          f"from a VM in one of the regions for region-to-region numbers)\n")
    print(f"{'node':>10} {'connect':>9} {'select 1 p50':>13} {'p95':>8} {'commit p50':>11} {'p95':>8}")
    for name in names:
        node = cl.nodes[name]
        t0 = time.perf_counter()
        node.conn()
        conn_ms = (time.perf_counter() - t0) * 1000
        rtt = _timed(lambda: node.q("select 1"), n)

        def commit():
            tid = uuid.uuid4()
            node.run("insert into settlement (transfer_id) values (%s)", (tid,))
        cm = _timed(commit, n)
        print(f"{name:>10} {conn_ms:8.0f}ms {rtt[0]:11.1f}ms {rtt[1]:6.1f}ms {cm[0]:9.1f}ms {cm[1]:6.1f}ms")

    a, b, c = names[:3]
    cl.create_account("TREASURY", a, allow_negative=True)
    for nm in names[:3]:
        cl.create_account(f"p_{nm}", nm)
        cl.create_account(f"q_{nm}", nm)
        cl.apis[a].fund(f"p_{nm}", 1_000_000)
    cl.settle()
    api = cl.apis[a]
    print("\ntransfer latency as seen by a client of the first node")
    same = _timed(lambda: api.transfer(uuid.uuid4(), f"p_{a}", f"q_{a}", 1), n)
    print(f"  both accounts homed on {a}:             p50 {same[0]:7.1f} ms   p95 {same[1]:7.1f} ms")
    cross = _timed(lambda: api.transfer(uuid.uuid4(), f"p_{a}", f"q_{b}", 1), n)
    print(f"  debit on {a}, credit homed on {b}:      p50 {cross[0]:7.1f} ms   p95 {cross[1]:7.1f} ms")
    strong = _timed(lambda: api.transfer(uuid.uuid4(), f"p_{a}", f"q_{b}", 1, strong=True), n)
    print(f"  same, strong tier (remote apply ack): p50 {strong[0]:7.1f} ms   p95 {strong[1]:7.1f} ms")
    for _ in range(n):
        api.transfer(uuid.uuid4(), f"p_{a}", f"q_{a}", 1)
    t0 = time.perf_counter()
    shipped = cl.ship(a, b)
    ms = (time.perf_counter() - t0) * 1000
    print(f"\nreplication: shipping {a} -> {b} applied {shipped['applied']} entries in {ms:.0f} ms")
    cl.settle()
    rep = checks.report(cl)
    print("invariants:", "ALL HOLD" if checks.healthy(rep) else rep)


def main(argv=None):
    p = argparse.ArgumentParser(prog="ledger")
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("init").set_defaults(fn=cmd_init)
    d = sub.add_parser("demo")
    d.add_argument("--transfers", type=int, default=30)
    d.set_defaults(fn=cmd_demo)
    dr = sub.add_parser("drill")
    dr.add_argument("--strong", action="store_true", help="ack the tail on a remote node first")
    dr.set_defaults(fn=cmd_drill)
    pr = sub.add_parser("probe")
    pr.add_argument("--samples", type=int, default=15)
    pr.set_defaults(fn=cmd_probe)
    args = p.parse_args(argv)
    args.fn(args)


if __name__ == "__main__":
    main()
