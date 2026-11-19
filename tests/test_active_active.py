import os
import random
import threading
import time
import uuid
from collections import Counter

import pytest

from ledger import checks
from ledger.cluster import Cluster
from ledger.config import load_dsns
from ledger.errors import (Fenced, IdempotencyConflict, InsufficientFunds, LeaseExpired, NotHome,
                           Unreachable)
from ledger.scenarios import failover_drill

THREADS = int(os.environ.get("LEDGER_TEST_THREADS", "12"))


def setup_accounts(cl, abc, balances):
    a, b, c = abc
    cl.create_account("TREASURY", a, allow_negative=True)
    homes = {n: abc[i % 3] for i, n in enumerate(balances)}
    for name, home in homes.items():
        cl.create_account(name, home)
    for name, amt in balances.items():
        if amt:
            cl.apis[a].fund(name, amt)
    cl.settle()
    return homes


def assert_healthy(cl):
    rep = checks.report(cl)
    assert checks.healthy(rep), rep


def test_transfer_is_idempotent(cl, abc):
    a, b, _ = abc
    setup_accounts(cl, abc, {"P": 100, "Q": 0})
    tid = uuid.uuid4()
    assert cl.apis[a].transfer(tid, "P", "Q", 30) == "settled"
    assert cl.apis[b].transfer(tid, "P", "Q", 30) == "settled"  # retry via another region
    cl.settle()
    assert cl.nodes[a].q("select balance from account where id='P'")[0]["balance"] == 70
    assert cl.nodes[a].q("select balance from account where id='Q'")[0]["balance"] == 30
    with pytest.raises(IdempotencyConflict):
        cl.apis[a].transfer(tid, "P", "Q", 31)
    assert_healthy(cl)


def test_concurrent_debits_never_overdraw(cl, abc):
    """20 racing debits of 10 against 100, from every region: exactly 10 win."""
    setup_accounts(cl, abc, {"P": 100, "Q": 0})
    results, lock = [], threading.Lock()

    def worker(i):
        api = cl.apis[abc[i % 3]]
        try:
            r = api.transfer(uuid.uuid4(), "P", "Q", 10)
        except InsufficientFunds:
            r = "declined"
        with lock:
            results.append(r)

    ts = [threading.Thread(target=worker, args=(i,)) for i in range(20)]
    [t.start() for t in ts]
    [t.join() for t in ts]
    cl.settle()
    assert Counter(results) == {"settled": 10, "declined": 10}
    assert cl.nodes[abc[0]].q("select balance from account where id='P'")[0]["balance"] == 0
    assert_healthy(cl)


def test_only_the_home_with_a_valid_lease_can_debit(cl, abc):
    a, b, _ = abc
    setup_accounts(cl, abc, {"P": 100, "Q": 0})
    home = cl.control.home("P")[0]
    other = b if home != b else a
    with pytest.raises(NotHome):
        cl.apis[other]._leg(uuid.uuid4(), "P", "debit", -1, None)
    cl.control.expire("P")
    with pytest.raises(LeaseExpired):
        cl.apis[home].debit(uuid.uuid4(), "P", 1)
    cl.control.renew("P")
    cl.apis[home].debit(uuid.uuid4(), "P", 1)


def test_replication_is_ordered_idempotent_and_buffers_gaps(cl, abc):
    a, b, _ = abc
    cl.create_account("TREASURY", a, allow_negative=True)
    cl.create_account("P", a)
    for _ in range(3):
        cl.apis[a].debit(uuid.uuid4(), "TREASURY", 1)
    rows = cl.nodes[a].q("select * from entry where account_id='TREASURY' order by seq")
    dst = cl.nodes[b]
    assert dst.apply_entry(rows[1]) == "gap"       # out of order: buffered, not applied
    assert dst.apply_entry(rows[0]) == "applied"
    assert dst.apply_entry(rows[1]) == "applied"
    assert dst.apply_entry(rows[1]) == "dup"       # redelivery is harmless
    assert dst.apply_entry(rows[2]) == "applied"
    cl._wm.clear()                                  # relay forgets its position and re-ships all
    assert cl.ship(a, b)["applied"] == 0
    assert checks.chain(cl.nodes[a], "TREASURY") == checks.chain(dst, "TREASURY")
    assert not checks.projection_mismatches(dst)


def test_crash_between_legs_is_recovered_exactly_once(cl, abc):
    a, b, c = abc
    setup_accounts(cl, abc, {"P": 100, "Q": 0})
    tid = uuid.uuid4()
    cl.apis[a].debit(tid, "P", 40, counterparty="Q")   # coordinator dies after the debit leg
    q_home = cl.control.home("Q")[0]
    assert not cl.nodes[q_home].q("select 1 from entry where transfer_id=%s and account_id='Q'", (tid,))
    p_home = cl.control.home("P")[0]
    assert cl.apis[p_home].recover() == 1
    assert cl.apis[p_home].recover() == 0              # nothing left to do
    cl.settle()
    credits = cl.nodes[q_home].q("select 1 from entry where transfer_id=%s and account_id='Q'", (tid,))
    assert len(credits) == 1
    assert_healthy(cl)


def test_credit_to_unreachable_home_stays_pending_then_completes(cl, abc):
    a, b, c = abc
    setup_accounts(cl, abc, {"P": 100, "Q": 0})
    q_home = cl.control.home("Q")[0]
    p_home = cl.control.home("P")[0]
    if q_home == p_home:
        pytest.skip("need P and Q homed on different nodes")
    cl.isolate(q_home)
    assert cl.apis[p_home].transfer(uuid.uuid4(), "P", "Q", 25) == "pending"
    cl.rejoin(q_home)
    cl.settle()
    assert cl.nodes[a].q("select balance from account where id='Q'")[0]["balance"] == 25
    assert_healthy(cl)


def test_failover_async_tail_is_quarantined_then_reconciled(cl):
    obs = failover_drill(cl, strong_tail=False)
    # the isolated home cannot renew its lease, so even a well-behaved writer refuses
    assert obs["legit_write_on_isolated"] == "LeaseExpired"
    assert obs["tail_hwm_on_standby"] < obs["tail_hwm_on_home"]            # async: RPO > 0
    # two acked-but-unreplicated debits plus the zombie's write, nothing silently merged
    assert len(obs["quarantined"]) == 3
    assert all(k == "debit" for (_, _, _, k, _) in obs["quarantined"])
    assert obs["zombie_after_rejoin"] == "Fenced"                          # DB rejects stale epoch
    moved = obs["steady"] + 2 + 2 + 5                                      # warm-up + tail + after + zombie
    assert obs["balances"] == {"TREASURY": -1000, "X": 1000 - moved, "Y": moved}
    assert_healthy(cl)


def test_failover_with_strong_tier_loses_nothing(cl):
    obs = failover_drill(cl, strong_tail=True)
    assert obs["tail_hwm_on_standby"] == obs["tail_hwm_on_home"]           # RPO 0
    assert len(obs["quarantined"]) == 1                                    # only the zombie write
    assert obs["zombie_after_rejoin"] == "Fenced"
    moved = obs["steady"] + 2 + 2 + 5
    assert obs["balances"] == {"TREASURY": -1000, "X": 1000 - moved, "Y": moved}
    assert_healthy(cl)


def test_random_concurrent_workload_converges(cl, abc):
    start = {f"acct{i}": 1000 for i in range(6)}
    setup_accounts(cl, abc, start)
    names = list(start)
    expected = dict(start)
    lock = threading.Lock()

    def worker(seed):
        rnd = random.Random(seed)
        api = cl.apis[abc[seed % 3]]
        for _ in range(15):
            src, dst = rnd.sample(names, 2)
            amt = rnd.randint(1, 40)
            try:
                api.transfer(uuid.uuid4(), src, dst, amt)
            except InsufficientFunds:
                continue
            with lock:
                expected[src] -= amt
                expected[dst] += amt

    ts = [threading.Thread(target=worker, args=(i,)) for i in range(THREADS)]
    [t.start() for t in ts]
    [t.join() for t in ts]
    cl.settle()
    for node in cl.nodes.values():
        got = {r["id"]: r["balance"] for r in node.q("select id, balance from account")}
        assert {k: got[k] for k in names} == expected, node.name
    assert_healthy(cl)


def test_healthy_home_keeps_its_lease_through_idle_periods():
    """Found on real Neon: a slow phase let a healthy home's lease lapse. Healthy homes
    heartbeat; an isolated home or a deliberately expired lease still runs out."""
    cl2 = Cluster(load_dsns(), lease_ttl=1.5)
    try:
        cl2.reset()
        a = cl2.names[0]
        cl2.create_account("TREASURY", a, allow_negative=True)
        cl2.create_account("P", a)
        cl2.apis[a].fund("P", 10)
        time.sleep(4)                                     # well past the ttl, no writes
        cl2.apis[a].debit(uuid.uuid4(), "P", 1)           # heartbeat kept the lease alive
        cl2.control.expire("P")
        time.sleep(2.5)                                   # heartbeat must not revive it
        with pytest.raises(LeaseExpired):
            cl2.apis[a].debit(uuid.uuid4(), "P", 1)
        cl2.control.renew("P")
        cl2.isolate(a)
        with pytest.raises(LeaseExpired):                 # cannot renew while cut off
            cl2.apis[a].debit(uuid.uuid4(), "P", 1)
    finally:
        cl2.close()
