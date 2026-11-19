"""The ledger API. One instance runs in each region, bound to that region's node.

Rules it enforces:
  * only the account's home (valid lease, current epoch) may write entries for it;
  * every write is keyed by transfer_id, so retries and recovery are idempotent;
  * a transfer is two legs (debit at the payer's home, credit at the payee's home), no
    distributed transaction; recover() finishes anything interrupted between the legs;
  * `strong=True` waits for a remote node to apply the entry before returning.
"""
from __future__ import annotations

import uuid

from .errors import (DurabilityNotMet, Fenced, IdempotencyConflict, InsufficientFunds,
                     LeaseExpired, LedgerError, NotHome, Unreachable)

PENDING_ERRORS = (Unreachable, NotHome, LeaseExpired, Fenced, DurabilityNotMet)


class Ledger:
    def __init__(self, node, cluster):
        self.node, self.cluster = node, cluster
        self._granted: dict[str, int] = {}  # epoch this API last held a lease for

    @property
    def name(self) -> str:
        return self.node.name

    # -- routing: a request may land anywhere; writes are forwarded to the home --
    def _route(self, account: str) -> "Ledger":
        home, _ = self.cluster.control.home(account)
        if home == self.name:
            return self
        self.cluster.require_reach(self.name, home)
        return self.cluster.apis[home]

    # -- the write path (runs at the home) --
    def _leg(self, transfer_id, account: str, kind: str, amount: int, counterparty: str | None,
             *, strong: bool = False, force: bool = False, ignore_lease: bool = False) -> dict:
        """Write one entry. `ignore_lease` simulates a buggy/paused writer acting on a stale
        lease; it exists so tests can prove the database still fences it."""
        if ignore_lease:
            epoch = self._granted.get(account) or self.node.latest_epoch(account)
        else:
            epoch = self.cluster.lease(account, self.name)
            self._granted[account] = epoch
        c = self.node.conn()
        with c.transaction():
            row = c.execute("select balance, next_seq, allow_negative from account "
                            "where id=%s for update", (account,)).fetchone()
            if row is None:
                raise LedgerError(f"unknown account {account}")
            ex = c.execute("select * from entry where transfer_id=%s and account_id=%s and kind=%s",
                           (transfer_id, account, kind)).fetchone()
            if ex:
                if ex["amount"] != amount:
                    raise IdempotencyConflict(f"{transfer_id} already used with amount {ex['amount']}")
                entry = ex
            else:
                if self.node.latest_epoch(account, c) != epoch:
                    raise Fenced(f"{self.name} writes {account} at epoch {epoch} but the database "
                                 f"knows epoch {self.node.latest_epoch(account, c)}")
                if (kind == "debit" and not (force or row["allow_negative"])
                        and row["balance"] + amount < 0):
                    raise InsufficientFunds(f"{account} has {row['balance']}, needs {-amount}")
                entry = c.execute(
                    "insert into entry (id, transfer_id, account_id, epoch, seq, kind, amount, "
                    "counterparty, origin) values (%s,%s,%s,%s,%s,%s,%s,%s,%s) returning *",
                    (uuid.uuid4(), transfer_id, account, epoch, row["next_seq"], kind, amount,
                     counterparty, self.name)).fetchone()
                c.execute("update account set balance = balance + %s, next_seq = next_seq + 1 "
                          "where id=%s", (amount, account))
        if strong:
            self.cluster.ack_strong(self.name, account, entry)
        return entry

    @staticmethod
    def _positive(amount: int) -> int:
        if not isinstance(amount, int) or amount <= 0:
            raise LedgerError(f"amount must be a positive integer of minor units, got {amount!r}")
        return amount

    def debit(self, transfer_id, account, amount, counterparty=None, *, strong=False, force=False):
        return self._route(account)._leg(transfer_id, account, "debit", -self._positive(amount),
                                         counterparty, strong=strong, force=force)

    def credit(self, transfer_id, account, amount, counterparty=None, *, strong=False):
        return self._route(account)._leg(transfer_id, account, "credit", self._positive(amount),
                                         counterparty, strong=strong)

    def _settle(self, account: str, transfer_id) -> None:
        h = self._route(account)
        h.node.run("insert into settlement (transfer_id) values (%s) on conflict do nothing",
                   (transfer_id,))

    def transfer(self, transfer_id, src: str, dst: str, amount: int, *, strong: bool = False) -> str:
        """Returns 'settled', or 'pending' if a leg could not complete (recover() finishes it).
        Raises InsufficientFunds (nothing written) or DurabilityNotMet on the debit leg
        (committed locally; retry with the same transfer_id)."""
        self.debit(transfer_id, src, amount, counterparty=dst, strong=strong)
        try:
            self.credit(transfer_id, dst, amount, counterparty=src, strong=strong)
            self._settle(src, transfer_id)
        except PENDING_ERRORS:
            return "pending"
        return "settled"

    def fund(self, account: str, amount: int) -> str:
        return self.transfer(uuid.uuid4(), "TREASURY", account, amount)

    def recover(self) -> int:
        """Finish transfers whose debit is committed here but whose credit is not confirmed."""
        done = 0
        if self.name in self.cluster.isolated:
            return 0
        for account in self.cluster.control.homed_at(self.name):
            rows = self.node.q(
                "select e.* from entry e where e.account_id=%s and e.kind='debit' "
                "and e.counterparty is not null and not exists "
                "(select 1 from settlement s where s.transfer_id = e.transfer_id)", (account,))
            for r in rows:
                try:
                    self.credit(r["transfer_id"], r["counterparty"], -r["amount"],
                                counterparty=account)
                    self._settle(account, r["transfer_id"])
                    done += 1
                except PENDING_ERRORS:
                    continue
        return done
