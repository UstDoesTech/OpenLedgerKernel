"""K-13: scripts/smoke_write.sh against a stand-in API. It must prove the steps, and it must
print counts only: this repository is public, so never an identifier, a token or a balance."""
import json
import os
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "smoke_write.sh"
TOKEN = "tok_secret_value_that_must_never_print"


class Api(BaseHTTPRequestHandler):
    accounts: dict
    journals: dict
    broken = False
    token = TOKEN

    def log_message(self, *args):
        pass

    def reply(self, status, payload, extra=None):
        data = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def read(self):
        return json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")

    def do_POST(self):
        if self.headers.get("Authorization") != f"Bearer {self.token}":
            return self.reply(401, {"code": "unauthenticated"})
        if self.broken:
            return self.reply(500, {"code": "home_unavailable"})
        body = self.read()
        key = self.headers["Idempotency-Key"]
        if self.path == "/v1/accounts":
            account = self.accounts.setdefault(key, {"account_id": f"acc_{len(self.accounts) + 1:04d}", "limit": body.get("overdraft_limit", {}).get("minor", 0), "posted": 0})
            return self.reply(201, {"account_id": account["account_id"]})
        if self.path == "/v1/transfers":
            by_id = {a["account_id"]: a for a in self.accounts.values()}
            if key in self.journals:
                return self.reply(201, {**self.journals[key], "replayed": True})
            by_id[body["payer_account_id"]]["posted"] -= body["amount"]["minor"]
            by_id[body["payee_account_id"]]["posted"] += body["amount"]["minor"]
            self.journals[key] = {"journal_id": f"jr_{len(self.journals) + 1:04d}", "status": "posted"}
            return self.reply(201, {**self.journals[key], "replayed": False})
        self.reply(404, {})

    def do_GET(self):
        account_id = self.path.split("/")[3]
        by_id = {a["account_id"]: a for a in self.accounts.values()}
        self.reply(200, {"account_id": account_id, "posted": {"minor": by_id[account_id]["posted"], "currency": "GBP"}})


@pytest.fixture
def server():
    handler = type("Handler", (Api,), {"accounts": {}, "journals": {}, "broken": False})
    httpd = HTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    yield handler, f"http://127.0.0.1:{httpd.server_port}"
    httpd.shutdown()


def run(url, tmp_path, token=TOKEN, **env):
    token_file = tmp_path / "token"
    token_file.write_text(token + "\n")
    return subprocess.run(
        ["bash", str(SCRIPT)], capture_output=True, text=True, timeout=60,
        env={**os.environ, "LEDGER_API_URL": url, "LEDGER_API_TOKEN_FILE": str(token_file), **env},
    )


def test_all_steps_pass_and_only_counts_are_printed(server, tmp_path):
    handler, url = server
    result = run(url, tmp_path)
    out = result.stdout + result.stderr
    assert result.returncode == 0, out
    assert out.strip().splitlines()[-1] == "steps: 9, failed: 0"
    assert out.count(": ok") == 9
    # Nothing that identifies or measures anything is printed.
    for secret in (TOKEN, "acc_", "jr_", "127.0.0.1", "100", "-100"):
        assert secret not in out, secret
    # The repeat of the transfer wrote nothing: one journal, payer down 100 and payee up 100 once.
    assert len(handler.journals) == 1


def test_a_failing_api_fails_the_script_without_printing_anything_it_said(server, tmp_path):
    handler, url = server
    handler.broken = True
    result = run(url, tmp_path)
    out = result.stdout + result.stderr
    assert result.returncode != 0
    assert "failed" in out
    assert "home_unavailable" not in out and TOKEN not in out


def test_a_wrong_token_is_reported_as_a_status_class_only(server, tmp_path):
    _, url = server
    result = run(url, tmp_path, token="not-the-token")
    out = result.stdout + result.stderr
    assert result.returncode != 0
    assert "status class 4xx" in out
    assert "not-the-token" not in out


def test_the_token_is_never_a_command_line_argument():
    lines = [l for l in SCRIPT.read_text().splitlines() if not l.lstrip().startswith("#")]
    # One header file for the payer's cell and one for the payee's cell, each written from a token
    # file, and nothing else that mentions the header.
    bearers = [l for l in lines if "Bearer" in l]
    assert len(bearers) == 2
    for bearer in bearers:
        assert bearer.startswith("printf") and bearer.endswith(('> "$work/auth"', '> "$work/auth_payee"')), "it goes to a file, not an argument"
    assert not any("curl" in l and "Authorization" in l for l in lines)
    assert any('--header "@$work/$auth"' in l for l in lines)
    assert not any('--header "$' in l and "token" in l for l in lines)


def test_it_refuses_to_run_without_a_token_file(tmp_path):
    result = subprocess.run(
        ["bash", str(SCRIPT)], capture_output=True, text=True,
        env={**os.environ, "LEDGER_API_URL": "http://127.0.0.1:1", "LEDGER_API_TOKEN_FILE": str(tmp_path / "missing")},
    )
    assert result.returncode == 2


def test_a_payee_at_another_cell_uses_its_own_token(server, tmp_path):
    first, url = server
    handler = type("Handler", (Api,), {"accounts": first.accounts, "journals": first.journals, "broken": False, "token": "tok_other_cell"})
    other = HTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=other.serve_forever, daemon=True).start()
    try:
        payee_token = tmp_path / "payee_token"
        payee_token.write_text("tok_other_cell\n")
        result = run(url, tmp_path, LEDGER_API_URL_PAYEE=f"http://127.0.0.1:{other.server_port}",
                     LEDGER_API_TOKEN_FILE_PAYEE=str(payee_token))
        out = result.stdout + result.stderr
        assert result.returncode == 0, out
        assert "tok_other_cell" not in out
        # Both accounts exist, so the payee was opened at the other cell with that cell's token:
        # the first cell would have refused it, and the other would have refused the first token.
        assert len(first.accounts) == 2
    finally:
        other.shutdown()


def test_a_payee_token_file_that_cannot_be_read_stops_it(server, tmp_path):
    _, url = server
    result = run(url, tmp_path, LEDGER_API_TOKEN_FILE_PAYEE=str(tmp_path / "nothing"))
    assert result.returncode == 2 and "LEDGER_API_TOKEN_FILE_PAYEE cannot be read" in result.stdout
