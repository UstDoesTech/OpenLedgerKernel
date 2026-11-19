"""One data node: a Postgres database in one region/cloud.

All replication rules live here, enforced in SQL so every region applies them identically:
idempotent apply, per-account ordering, epoch fencing and quarantine.
"""
from __future__ import annotations

import threading
import time

import psycopg
from psycopg.rows import dict_row

from .config import schema_name
from .errors import LedgerError

# An entry is void if its epoch was superseded after it: seq beyond the cutoff the next
# home started from, or an epoch skipped entirely. `{t}` is the table alias to test.
VOID = ("exists (select 1 from epoch_fence f where f.account_id = {t}.account_id and ("
        "(f.prev_epoch = {t}.epoch and {t}.seq > f.prev_cutoff_seq) or "
        "(f.prev_epoch < {t}.epoch and {t}.epoch < f.epoch)))")

ENTRY_INSERT = ("insert into entry (id, transfer_id, account_id, epoch, seq, kind, amount, "
                "counterparty, origin, created_at) values (%(id)s, %(transfer_id)s, "
                "%(account_id)s, %(epoch)s, %(seq)s, %(kind)s, %(amount)s, %(counterparty)s, "
                "%(origin)s, %(created_at)s)")

QUARANTINE_INSERT = ("insert into quarantine (id, transfer_id, account_id, epoch, seq, kind, "
                     "amount, counterparty, origin, created_at, reason) values (%(id)s, "
                     "%(transfer_id)s, %(account_id)s, %(epoch)s, %(seq)s, %(kind)s, %(amount)s, "
                     "%(counterparty)s, %(origin)s, %(created_at)s, %(reason)s) "
                     "on conflict (id) do nothing")


class Node:
    def __init__(self, name: str, dsn: str, schema: str | None = None):
        self.name, self.dsn = name, dsn
        self.schema = schema or schema_name()
        self._tl = threading.local()
        self._all: list[psycopg.Connection] = []
        self._lock = threading.Lock()

    # -- connections (one per thread; direct, non-pooled, autocommit with explicit txns) --
    def conn(self) -> psycopg.Connection:
        c = getattr(self._tl, "c", None)
        if c is not None and not c.closed and not c.broken:
            return c
        last: Exception | None = None
        for i in range(4):  # serverless computes can take seconds to wake
            try:
                c = psycopg.connect(self.dsn, autocommit=True, row_factory=dict_row,
                                    prepare_threshold=None, connect_timeout=30)
                c.execute(f"set search_path to {self.schema}")
                with self._lock:
                    self._all.append(c)
                self._tl.c = c
                return c
            except psycopg.OperationalError as e:
                last = e
                time.sleep(1 + i)
        raise last  # type: ignore[misc]

    def close(self) -> None:
        with self._lock:
            for c in self._all:
                try:
                    c.close()
                except Exception:
                    pass
            self._all.clear()
        self._tl = threading.local()

    def q(self, sql: str, params=()) -> list[dict]:
        return self.conn().execute(sql, params).fetchall()

    def run(self, sql: str, params=()) -> None:
        self.conn().execute(sql, params)

    # -- positions --
    def latest_epoch(self, account: str, c: psycopg.Connection | None = None) -> int:
        c = c or self.conn()
        r = c.execute("select max(epoch) as e from epoch_fence where account_id=%s",
                      (account,)).fetchone()
        return r["e"] or 0

    def hwm(self, account: str, c: psycopg.Connection | None = None) -> tuple[int, int]:
        """Highest (epoch, seq) applied for the account on this node."""
        c = c or self.conn()
        r = c.execute("select epoch, seq from entry where account_id=%s "
                      "order by epoch desc, seq desc limit 1", (account,)).fetchone()
        return (r["epoch"], r["seq"]) if r else (0, 0)

    # -- fencing --
    def install_fence(self, f: dict) -> bool:
        """Record an epoch fence from the control plane, then quarantine anything it voids."""
        c = self.conn()
        with c.transaction():
            c.execute("select 1 from account where id=%s for update", (f["account_id"],))
            ins = c.execute(
                "insert into epoch_fence (account_id, epoch, home, prev_epoch, prev_cutoff_seq) "
                "values (%(account_id)s, %(epoch)s, %(home)s, %(prev_epoch)s, "
                "%(prev_cutoff_seq)s) on conflict do nothing returning 1", f).fetchone()
            if ins:
                self._sweep(c, f["account_id"])
            return bool(ins)

    def begin_epoch(self, account: str) -> None:
        self.run("update account set next_seq = 1 where id=%s", (account,))

    def _sweep(self, c: psycopg.Connection, account: str) -> None:
        """Move void entries into quarantine and reverse their effect on the projection.

        This is the one place entries leave the log: tail extraction from a stale home.
        Caller holds the account row lock.
        """
        c.execute(
            f"""with void as (
                  delete from entry e where e.account_id = %(a)s and {VOID.format(t='e')}
                  returning e.*
                ), q as (
                  insert into quarantine (id, transfer_id, account_id, epoch, seq, kind, amount,
                                          counterparty, origin, created_at, reason)
                  select id, transfer_id, account_id, epoch, seq, kind, amount, counterparty,
                         origin, created_at, 'void_after_fence' from void
                  on conflict (id) do nothing
                )
                update account set balance = balance - coalesce((select sum(amount) from void), 0)
                where id = %(a)s""", {"a": account})

    # -- replication apply --
    def apply_entry(self, e: dict) -> str:
        """Apply one replicated entry. Returns applied | dup | quarantined | gap | deferred."""
        c, a = self.conn(), e["account_id"]
        with c.transaction():
            if not c.execute("select 1 from account where id=%s for update", (a,)).fetchone():
                raise LedgerError(f"unknown account {a} on {self.name}")
            if c.execute("select 1 from entry where id=%s", (e["id"],)).fetchone():
                return "dup"
            self._sweep(c, a)
            void = c.execute(
                f"select {VOID.format(t='x')} as v from (select %s::text as account_id, "
                "%s::bigint as epoch, %s::bigint as seq) x",
                (a, e["epoch"], e["seq"])).fetchone()["v"]
            if void:
                c.execute(QUARANTINE_INSERT, {**e, "reason": "void_on_arrival"})
                return "quarantined"
            if e["epoch"] > self.latest_epoch(a, c):
                return "deferred"  # epoch not known here yet: never guess, wait for the fence
            he, hs = self.hwm(a, c)
            if e["epoch"] == he:
                if e["seq"] > hs + 1:
                    return "gap"
                if e["seq"] <= hs:
                    raise LedgerError(f"position ({a},{he},{e['seq']}) taken by a different entry")
            elif e["epoch"] > he:
                if e["seq"] != 1:
                    return "gap"
                cutoff = c.execute("select min(prev_cutoff_seq) as c from epoch_fence "
                                   "where account_id=%s and prev_epoch=%s", (a, he)).fetchone()["c"]
                if cutoff is None:
                    return "deferred"
                if hs < cutoff:
                    return "gap"  # missing the previous epoch's valid tail; buffer, never skip
            else:
                raise LedgerError(f"entry from epoch {e['epoch']} behind local epoch {he}")
            c.execute(ENTRY_INSERT, e)
            c.execute("update account set balance = balance + %s where id=%s", (e["amount"], a))
            return "applied"
