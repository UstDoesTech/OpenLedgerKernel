"""The cluster: nodes, control plane, fault injection and the replication relay.

Faults are injected at the application layer (isolate/rejoin). That exercises the logic
against real databases but does not cut real network links.
"""
from __future__ import annotations

import threading
from collections import Counter
from pathlib import Path

from .api import Ledger
from .control import Control
from .errors import DurabilityNotMet, LeaseExpired, LedgerError, Unreachable
from .node import Node

SCHEMA_SQL = Path(__file__).resolve().parent.parent / "schema.sql"
TABLES = "account, epoch_fence, entry, quarantine, settlement"


class Cluster:
    def __init__(self, dsns: dict[str, str], schema: str | None = None, lease_ttl: float = 30.0):
        self.lease_ttl = lease_ttl
        self.nodes = {n: Node(n, d, schema) for n, d in dsns.items()}
        self.apis = {n: Ledger(self.nodes[n], self) for n in self.nodes}
        self.control = Control(lease_ttl)
        self.isolated: set[str] = set()
        self._wm: dict[tuple[str, str, str], tuple[int, int]] = {}  # relay outbox watermarks
        # Healthy homes heartbeat their leases. Without this, a slow phase (real WAN latency)
        # lets a lease lapse with nothing wrong, which is not what the design models.
        self._hb_stop = threading.Event()
        self._hb = threading.Thread(target=self._heartbeat, daemon=True, name="lease-heartbeat")
        self._hb.start()

    def _heartbeat(self) -> None:
        while not self._hb_stop.wait(max(0.5, self.lease_ttl / 3)):
            try:
                self.control.heartbeat(set(self.isolated))
            except Exception:
                pass  # a missed beat is retried on the next tick

    @property
    def names(self) -> list[str]:
        return sorted(self.nodes)

    def close(self) -> None:
        self._hb_stop.set()
        for n in self.nodes.values():
            n.close()

    # -- setup --
    def init_schema(self) -> None:
        sql = SCHEMA_SQL.read_text()
        for n in self.nodes.values():
            schema = n.schema
            n.run(sql.replace("ledger_lab", schema) if schema != "ledger_lab" else sql)

    def reset(self) -> None:
        """Empty the harness tables (only the harness schema) and start a fresh control plane."""
        self.init_schema()
        for n in self.nodes.values():
            n.run(f"truncate {TABLES}")
        self.control = Control(self.lease_ttl)
        self.isolated.clear()
        self._wm.clear()
        for api in self.apis.values():
            api._granted.clear()

    def create_account(self, account: str, home: str, allow_negative: bool = False) -> None:
        for n in self.nodes.values():
            c = n.conn()
            with c.transaction():
                c.execute("insert into account (id, allow_negative) values (%s, %s) "
                          "on conflict do nothing", (account, allow_negative))
                c.execute("insert into epoch_fence (account_id, epoch, home) values (%s, 1, %s) "
                          "on conflict do nothing", (account, home))
        self.control.register(account, home)

    # -- fault injection --
    def isolate(self, node: str) -> None:
        """Cut a node off from its peers and from the control plane (crash or partition)."""
        self.isolated.add(node)

    def rejoin(self, node: str) -> None:
        self.isolated.discard(node)

    def reachable(self, a: str, b: str) -> bool:
        return a == b or (a not in self.isolated and b not in self.isolated)

    def require_reach(self, a: str, b: str) -> None:
        if not self.reachable(a, b):
            raise Unreachable(f"{a} cannot reach {b}")

    def lease(self, account: str, node: str) -> int:
        if node in self.isolated:
            raise LeaseExpired(f"{node} cannot reach the control plane to hold {account}")
        return self.control.check_lease(account, node)

    # -- failover --
    def failover(self, account: str, new_home: str) -> int:
        """Promote `new_home`: it reports its watermark, the control plane issues epoch+1 with
        that watermark as the cutoff, and the fence is pushed to every reachable node."""
        if new_home in self.isolated:
            raise Unreachable(f"cannot promote isolated node {new_home}")
        nh = self.nodes[new_home]
        he, hs = nh.hwm(account)
        fence = self.control.promote(account, new_home, he, hs)
        for n in sorted(self.nodes, key=lambda x: x != new_home):
            if n not in self.isolated:
                self.nodes[n].install_fence(fence)
        nh.begin_epoch(account)
        return fence["epoch"]

    def sync_control(self) -> None:
        """Push every known fence to every reachable node (what a rejoining node would pull)."""
        fences = self.control.fences()
        for name, n in self.nodes.items():
            if name not in self.isolated:
                for f in fences:
                    n.install_fence(f)

    # -- replication relay: pull-based outbox, per-account order, idempotent apply --
    def ship(self, src: str, dst: str, account: str | None = None) -> Counter:
        self.require_reach(src, dst)
        s, d = self.nodes[src], self.nodes[dst]
        accounts = [account] if account else [r["id"] for r in s.q("select id from account order by id")]
        out: Counter = Counter()
        for a in accounts:
            key = (src, dst, a)
            rows = s.q("select * from entry where account_id=%s and (epoch, seq) > (%s, %s) "
                       "order by epoch, seq", (a, *self._wm.get(key, (0, 0))))
            for r in rows:
                res = d.apply_entry(r)
                out[res] += 1
                if res in ("gap", "deferred"):
                    break  # buffer: wait for the missing piece, never skip it
                self._wm[key] = (r["epoch"], r["seq"])
        return out

    def ack_strong(self, src: str, account: str, entry: dict, min_acks: int = 1) -> None:
        """Strong tier: return only once `min_acks` other nodes have durably applied the entry."""
        pos, acks = (entry["epoch"], entry["seq"]), 0
        for dst in self.names:
            if dst == src or not self.reachable(src, dst):
                continue
            try:
                self.ship(src, dst, account)
                if self.nodes[dst].hwm(account) >= pos:
                    acks += 1
            except LedgerError:
                continue
            if acks >= min_acks:
                return
        raise DurabilityNotMet(f"{account} {pos}: {acks}/{min_acks} remote acks")

    def settle(self, rounds: int = 12) -> int:
        """Run control sync, recovery and replication until nothing moves."""
        total = 0
        for _ in range(rounds):
            self.sync_control()
            progress = sum(api.recover() for api in self.apis.values())
            for s in self.names:
                for d in self.names:
                    if s != d and self.reachable(s, d):
                        c = self.ship(s, d)
                        progress += c["applied"] + c["quarantined"]
            total += progress
            if not progress:
                break
        return total

    # -- quarantine --
    def unresolved_quarantine(self) -> list[dict]:
        seen: dict = {}
        for n in self.nodes.values():
            for r in n.q("select * from quarantine where not resolved"):
                seen.setdefault(r["id"], r)
        return sorted(seen.values(), key=lambda r: (r["account_id"], r["epoch"], r["seq"]))

    def resolve_quarantine(self) -> list[dict]:
        """Explicit reconciliation: re-post each voided entry under the current epoch, keyed by
        its original transfer id (so a client retry that already succeeded is not duplicated).
        Debits are forced: the customer was already told yes."""
        rows = self.unresolved_quarantine()
        for r in rows:
            home, _ = self.control.home(r["account_id"])
            self.apis[home]._leg(r["transfer_id"], r["account_id"], r["kind"], r["amount"],
                                 r["counterparty"], force=True)
        ids = [r["id"] for r in rows]
        if ids:
            for n in self.nodes.values():
                n.run("update quarantine set resolved = true where id = any(%s)", (ids,))
        return rows
