"""Invariants. After cluster.settle() all of these must hold on every node."""
from __future__ import annotations

import hashlib


def projection_mismatches(node) -> list[dict]:
    """balance must equal the sum of entries on every node."""
    return node.q("select a.id, a.balance, coalesce(sum(e.amount), 0)::bigint as entries "
                  "from account a left join entry e on e.account_id = a.id "
                  "group by a.id having a.balance <> coalesce(sum(e.amount), 0)")


def chain(node, account: str) -> str:
    """Hash chain over the account's log in (epoch, seq) order."""
    h = hashlib.sha256()
    for r in node.q("select id, epoch, seq, amount from entry where account_id=%s "
                    "order by epoch, seq", (account,)):
        h.update(f"{r['id']}|{r['epoch']}|{r['seq']}|{r['amount']};".encode())
    return h.hexdigest()


def divergent_accounts(cluster) -> list[str]:
    bad = []
    first = cluster.nodes[cluster.names[0]]
    for r in first.q("select id from account order by id"):
        if len({chain(n, r["id"]) for n in cluster.nodes.values()}) > 1:
            bad.append(r["id"])
    return bad


def gaps(node) -> list[dict]:
    """Within an epoch, seq must run 1..n with no holes (epoch tails past a cutoff excepted)."""
    return node.q("select account_id, epoch, count(*) as n, max(seq) as top from entry "
                  "group by account_id, epoch having count(*) <> max(seq)")


def unbalanced_transfers(node) -> list[dict]:
    return node.q("select transfer_id, sum(amount)::bigint as net from entry "
                  "group by transfer_id having sum(amount) <> 0")


def total(node) -> int:
    return int(node.q("select coalesce(sum(amount), 0) as s from entry")[0]["s"])


def overdrawn(node) -> list[dict]:
    return node.q("select id, balance from account where balance < 0 and not allow_negative")


def report(cluster) -> dict:
    first = cluster.nodes[cluster.names[0]]
    return {
        "projection_mismatches": {n: projection_mismatches(v) for n, v in cluster.nodes.items()},
        "divergent_accounts": divergent_accounts(cluster),
        "gaps": {n: gaps(v) for n, v in cluster.nodes.items()},
        "unbalanced_transfers": len(unbalanced_transfers(first)),
        "total": total(first),
        "overdrawn": overdrawn(first),
        "quarantine_unresolved": len(cluster.unresolved_quarantine()),
    }


def healthy(rep: dict) -> bool:
    return (not any(rep["projection_mismatches"].values()) and not rep["divergent_accounts"]
            and not any(rep["gaps"].values()) and rep["unbalanced_transfers"] == 0
            and rep["total"] == 0 and not rep["overdrawn"] and rep["quarantine_unresolved"] == 0)
