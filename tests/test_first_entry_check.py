"""K-20g: scripts/first_entry_check.sh (steps 4 to 6 of the first-entry runbook) against stand-in
APIs. It must check health first and write nothing if a cell is not ready, give each cell its own
token, and print counts only: this repository is public."""
import json
import os
import subprocess
import threading
from http.server import HTTPServer
from pathlib import Path

import pytest

from test_smoke_write import Api

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "first_entry_check.sh"


def make_server(token, shared, health=200, status="ok"):
    # The cells share one ledger here, as a transfer across cells needs both accounts to be
    # visible. Each has its own token, and records the accounts that were opened at it.
    class Handler(Api):
        broken = False
        opened = 0

        def do_GET(self):
            if self.path == "/healthz":
                return self.reply(health, {"cell": "cell_secret_name", "status": status})
            return super().do_GET()

        def do_POST(self):
            if self.path == "/v1/accounts" and self.headers.get("Authorization") == f"Bearer {self.token}":
                type(self).opened += 1
            return super().do_POST()

    Handler.token = token
    Handler.accounts = shared["accounts"]
    Handler.journals = shared["journals"]
    httpd = HTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    return Handler, httpd, f"http://127.0.0.1:{httpd.server_port}"


@pytest.fixture
def cells():
    made = []

    def build(n=3, per_cell=None):
        per_cell = per_cell or {}
        shared = {"accounts": {}, "journals": {}}
        out = []
        for i in range(n):
            token = f"tok_cell_{i}_must_never_print"
            handler, httpd, url = make_server(token, shared, **per_cell.get(i, {}))
            made.append(httpd)
            out.append((handler, url, token))
        return out

    yield build
    for httpd in made:
        httpd.shutdown()


def run(cells_made, tmp_path, **env):
    files = []
    for i, (_, _, token) in enumerate(cells_made):
        f = tmp_path / f"token{i}"
        f.write_text(token + "\n")
        files.append(str(f))
    return subprocess.run(
        ["bash", str(SCRIPT)], capture_output=True, text=True, timeout=120,
        env={
            **os.environ,
            "LEDGER_API_URLS": " ".join(url for _, url, _ in cells_made),
            "LEDGER_API_TOKEN_FILES": " ".join(files),
            **env,
        },
    )


def test_three_cells_pass_every_step_and_only_counts_are_printed(cells, tmp_path):
    made = cells(3)
    result = run(made, tmp_path)
    out = result.stdout + result.stderr
    assert result.returncode == 0, out
    # three health checks, one first entry (9 steps) and two cross-cell transfers (9 steps each)
    assert out.strip().splitlines()[-1] == "steps: 30, failed: 0"
    assert "step 4: each cell's health" in out
    assert "step 5: a first entry at cell 1" in out
    assert "step 6: a transfer from cell 1 to cell 2" in out
    assert "step 6: a transfer from cell 1 to cell 3" in out
    for secret in ("tok_cell_", "acc_", "jr_", "127.0.0.1", "cell_secret_name", "-100"):
        assert secret not in out, secret


def test_a_cell_that_is_not_ready_stops_the_run_before_anything_is_written(cells, tmp_path):
    made = cells(3, {1: {"health": 503, "status": "leases_missing"}})
    result = run(made, tmp_path)
    out = result.stdout + result.stderr
    assert result.returncode == 1
    assert "cell 2: failed (status class 5xx, leases_missing)" in out
    assert "nothing was written" in out
    assert all(handler.opened == 0 for handler, _, _ in made)


def test_a_status_word_that_is_not_in_the_fixed_list_is_not_repeated(cells, tmp_path):
    made = cells(2, {0: {"health": 503, "status": "an_address_10.1.2.3"}})
    out = run(made, tmp_path).stdout
    assert "10.1.2.3" not in out and "unknown" in out


def test_each_cell_is_used_with_its_own_token(cells, tmp_path):
    # Cell 2 refuses cell 1's token, so a run that sent one token everywhere would fail at step 6.
    made = cells(2)
    result = run(made, tmp_path)
    assert result.returncode == 0, result.stdout
    # Step 5 opens two accounts at cell 1 and step 6 opens the payer there, and the payee at cell 2.
    assert made[0][0].opened == 3
    assert made[1][0].opened == 1


def test_a_wrong_token_fails_step_6_only_as_a_status_class(cells, tmp_path):
    made = cells(2)
    bad = tmp_path / "bad"
    bad.write_text("tok_wrong\n")
    good = tmp_path / "good"
    good.write_text(made[0][2] + "\n")
    result = subprocess.run(
        ["bash", str(SCRIPT)], capture_output=True, text=True, timeout=120,
        env={**os.environ, "LEDGER_API_URLS": f"{made[0][1]} {made[1][1]}",
             "LEDGER_API_TOKEN_FILES": f"{good} {bad}"},
    )
    out = result.stdout + result.stderr
    assert result.returncode == 1
    assert "status class 4xx" in out and "tok_wrong" not in out


def test_the_apis_file_gives_the_cells_in_key_order_over_https(tmp_path):
    # A stand-in curl records the addresses it is asked for and never connects.
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    log = tmp_path / "asked"
    (bin_dir / "curl").write_text(f'#!/usr/bin/env bash\necho "${{@: -1}}" >> {log}\necho 000\nexit 1\n')
    (bin_dir / "curl").chmod(0o755)
    apis = tmp_path / "apis.json"
    apis.write_text(json.dumps({
        "gcp": {"address": "10.0.0.3", "port": 8443},
        "aws": {"address": "10.0.0.1", "port": 8443},
        "azure": {"address": "10.0.0.2", "port": 8443},
    }))
    files = []
    for n in range(3):
        f = tmp_path / f"t{n}"
        f.write_text("tok\n")
        files.append(str(f))
    result = subprocess.run(
        ["bash", str(SCRIPT)], capture_output=True, text=True, timeout=30,
        env={**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}", "LEDGER_APIS_FILE": str(apis),
             "LEDGER_API_TOKEN_FILES": " ".join(files)},
    )
    assert log.read_text().split() == [
        "https://10.0.0.1:8443/healthz", "https://10.0.0.2:8443/healthz", "https://10.0.0.3:8443/healthz",
    ]
    out = result.stdout + result.stderr
    assert "10.0.0." not in out, "an address is never printed"
    assert result.returncode == 1


def test_it_needs_one_token_file_for_each_cell(cells, tmp_path):
    made = cells(2)
    result = subprocess.run(
        ["bash", str(SCRIPT)], capture_output=True, text=True, timeout=30,
        env={**os.environ, "LEDGER_API_URLS": f"{made[0][1]} {made[1][1]}",
             "LEDGER_API_TOKEN_FILES": str(tmp_path / "only_one")},
    )
    assert result.returncode == 2 and "one token file for each cell" in result.stdout


def test_it_never_prints_or_passes_a_token_on_a_command_line():
    text = SCRIPT.read_text()
    assert "Bearer" not in text
    assert "--header" not in text
