from __future__ import annotations

import threading
import time

from .errors import LeaseExpired, NotHome


class Control:
    """In-process stand-in for the consensus store (etcd, Consul, ...).

    Per account it holds the current home, the epoch and a lease. Every promotion appends
    a fence record that the cluster pushes to the data nodes. Swap this class for an
    etcd-backed one in production; the interface is the whole contract.
    """

    def __init__(self, lease_ttl: float = 30.0):
        self.lease_ttl = lease_ttl
        self._lock = threading.RLock()
        self._acct: dict[str, dict] = {}

    def register(self, account: str, home: str) -> None:
        with self._lock:
            self._acct[account] = {
                "home": home, "epoch": 1, "lease_until": time.monotonic() + self.lease_ttl,
                "forced": False,  # True after expire(): the heartbeat must not revive it
                "fences": [{"account_id": account, "epoch": 1, "home": home,
                            "prev_epoch": 0, "prev_cutoff_seq": 0}],
            }

    def home(self, account: str) -> tuple[str, int]:
        with self._lock:
            a = self._acct[account]
            return a["home"], a["epoch"]

    def accounts(self) -> list[str]:
        with self._lock:
            return sorted(self._acct)

    def homed_at(self, node: str) -> list[str]:
        with self._lock:
            return sorted(k for k, a in self._acct.items() if a["home"] == node)

    def check_lease(self, account: str, node: str) -> int:
        """Return the epoch if `node` holds a valid lease for `account`, renewing it."""
        with self._lock:
            a = self._acct[account]
            if a["home"] != node:
                raise NotHome(f"{node} is not home for {account} "
                              f"(home={a['home']}, epoch={a['epoch']})")
            now = time.monotonic()
            if now > a["lease_until"]:
                raise LeaseExpired(f"lease for {account} at {node} expired")
            a["lease_until"] = now + self.lease_ttl
            return a["epoch"]

    def expire(self, account: str) -> None:
        """Simulate missed heartbeats: the lease stays expired until renew() is called."""
        with self._lock:
            self._acct[account]["lease_until"] = 0.0
            self._acct[account]["forced"] = True

    def renew(self, account: str) -> None:
        with self._lock:
            self._acct[account]["lease_until"] = time.monotonic() + self.lease_ttl
            self._acct[account]["forced"] = False

    def heartbeat(self, unreachable: set[str]) -> int:
        """What healthy homes do continuously: renew their leases. Homes that cannot reach the
        control plane, and leases expired on purpose, are left to run out."""
        n = 0
        with self._lock:
            now = time.monotonic()
            for a in self._acct.values():
                if a["home"] not in unreachable and not a["forced"]:
                    a["lease_until"] = now + self.lease_ttl
                    n += 1
        return n

    def promote(self, account: str, new_home: str, prev_epoch: int, prev_cutoff_seq: int) -> dict:
        """Move the home. The new home reports its watermark; that becomes the cutoff."""
        with self._lock:
            a = self._acct[account]
            a["epoch"] += 1
            a["home"] = new_home
            a["lease_until"] = time.monotonic() + self.lease_ttl
            a["forced"] = False
            fence = {"account_id": account, "epoch": a["epoch"], "home": new_home,
                     "prev_epoch": prev_epoch, "prev_cutoff_seq": prev_cutoff_seq}
            a["fences"].append(fence)
            return dict(fence)

    def fences(self) -> list[dict]:
        with self._lock:
            return [dict(f) for a in self._acct.values() for f in a["fences"]]
