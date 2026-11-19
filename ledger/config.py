"""Node configuration.

Set one env var per node: LEDGER_DSN_<NAME>=<postgres dsn>. The suffix becomes the node
name (use the region, e.g. LEDGER_DSN_EUW2). With none set, three local nodes on
127.0.0.1:5431-5433 are assumed (see scripts/local_nodes.sh).
"""
from __future__ import annotations

import os
import re

LOCAL_DEFAULT = {
    "n1": "postgresql://ledger@127.0.0.1:5431/postgres",
    "n2": "postgresql://ledger@127.0.0.1:5432/postgres",
    "n3": "postgresql://ledger@127.0.0.1:5433/postgres",
}


def load_dsns() -> dict[str, str]:
    found = {k[len("LEDGER_DSN_"):].lower(): v
             for k, v in os.environ.items() if k.startswith("LEDGER_DSN_") and v}
    return dict(sorted(found.items())) if found else dict(LOCAL_DEFAULT)


def schema_name() -> str:
    name = os.environ.get("LEDGER_SCHEMA", "ledger_lab")
    if not re.fullmatch(r"[a-z_][a-z0-9_]*", name):
        raise ValueError(f"bad LEDGER_SCHEMA {name!r}")
    return name


def is_local(dsns: dict[str, str]) -> bool:
    return all("127.0.0.1" in d or "localhost" in d for d in dsns.values())
