import os
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from ledger.cluster import Cluster  # noqa: E402
from ledger.config import is_local, load_dsns  # noqa: E402


@pytest.fixture(scope="session")
def _cluster():
    dsns = load_dsns()
    if not is_local(dsns) and os.environ.get("LEDGER_ALLOW_REMOTE") != "1":
        pytest.exit("Remote DSNs configured. The tests truncate the ledger_lab schema on every "
                    "node. Set LEDGER_ALLOW_REMOTE=1 to confirm.", returncode=2)
    cl = Cluster(dsns)
    yield cl
    cl.close()


@pytest.fixture
def cl(_cluster):
    _cluster.reset()
    return _cluster


@pytest.fixture
def abc(cl):
    return cl.names[:3]
