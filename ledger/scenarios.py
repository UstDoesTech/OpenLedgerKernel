"""Scripted scenarios used by both the CLI drill and the tests."""
from __future__ import annotations

import os
import uuid

from .errors import Fenced, LedgerError


def _say(*a):
    pass


def failover_drill(cl, *, strong_tail: bool = False, steady: int | None = None, say=_say) -> dict:
    """Home dies with acknowledged writes in flight, a standby is promoted, and the old home
    comes back still believing it is home.

    Account X is homed on node a, Y on node c; b is the standby that gets promoted.
    With strong_tail=False the last two transfers exist only on a (async RPO > 0);
    with strong_tail=True they were acked by b first (RPO 0).
    `steady` transfers of 1 run first (env LEDGER_DRILL_STEADY, default 50); keep it small
    over a WAN, where every transfer costs hundreds of milliseconds.
    """
    steady = steady if steady is not None else int(os.environ.get("LEDGER_DRILL_STEADY", "50"))
    a, b, c = cl.names[:3]
    cl.reset()
    cl.create_account("TREASURY", a, allow_negative=True)
    cl.create_account("X", a)
    cl.create_account("Y", c)
    api_a, api_b = cl.apis[a], cl.apis[b]
    obs: dict = {"nodes": (a, b, c), "steady": steady}

    api_a.fund("X", 1000)
    for _ in range(steady):
        api_a.transfer(uuid.uuid4(), "X", "Y", 1)
    cl.settle()
    say(f"steady state: X homed on {a}, {steady} transfers X->Y replicated everywhere")

    for _ in range(2):
        api_a.transfer(uuid.uuid4(), "X", "Y", 1, strong=strong_tail)
    obs["tail_hwm_on_home"] = cl.nodes[a].hwm("X")
    obs["tail_hwm_on_standby"] = cl.nodes[b].hwm("X")
    say(f"2 more transfers acked to clients (strong={strong_tail}); home hwm "
        f"{obs['tail_hwm_on_home']}, standby hwm {obs['tail_hwm_on_standby']}")

    cl.isolate(a)
    say(f"{a} isolated from peers and control plane")
    try:
        api_a.debit(uuid.uuid4(), "X", 5)
        obs["legit_write_on_isolated"] = "allowed"
    except LedgerError as e:
        obs["legit_write_on_isolated"] = type(e).__name__

    obs["new_epoch"] = cl.failover("X", b)
    say(f"failover: {b} is home for X at epoch {obs['new_epoch']}, "
        f"cutoff = its hwm {obs['tail_hwm_on_standby']}")
    for _ in range(2):
        api_b.transfer(uuid.uuid4(), "X", "Y", 1)

    zombie_tid = uuid.uuid4()
    z = cl.apis[a]._leg(zombie_tid, "X", "debit", -5, "Y", ignore_lease=True)
    obs["zombie_entry"] = (z["epoch"], z["seq"])
    say(f"zombie {a} ignored its lease and wrote {obs['zombie_entry']} locally")

    cl.rejoin(a)
    cl.settle()
    q = cl.unresolved_quarantine()
    obs["quarantined"] = [(r["account_id"], r["epoch"], r["seq"], r["kind"], r["amount"]) for r in q]
    say(f"rejoined: {len(q)} entries quarantined, history converged on new epoch")

    try:
        cl.apis[a]._leg(uuid.uuid4(), "X", "debit", -1, None, ignore_lease=True)
        obs["zombie_after_rejoin"] = "allowed"
    except Fenced:
        obs["zombie_after_rejoin"] = "Fenced"

    cl.resolve_quarantine()
    cl.settle()
    say("quarantine resolved: voided entries re-posted under the current epoch")

    home_x = cl.control.home("X")[0]
    obs["balances"] = {acct: cl.nodes[home_x].q("select balance from account where id=%s",
                                                (acct,))[0]["balance"]
                       for acct in ("TREASURY", "X", "Y")}
    return obs
