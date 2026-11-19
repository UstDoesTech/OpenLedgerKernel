"""scripts/plan_counts.py prints counts only, and never echoes the plan."""

import json
import subprocess
import sys
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "scripts" / "plan_counts.py"


def run(tmp_path, plan):
    p = tmp_path / "plan.json"
    p.write_text(json.dumps(plan))
    return subprocess.run([sys.executable, "-I", str(SCRIPT), str(p)], capture_output=True, text=True)


def test_counts_only(tmp_path):
    plan = {"resource_changes": [
        {"address": "aws_secret.acct_123456789012", "change": {"actions": ["create"], "after": {"password": "hunter2"}}},
        {"address": "x.a", "change": {"actions": ["update"]}},
        {"address": "x.b", "change": {"actions": ["delete"]}},
        {"address": "x.c", "change": {"actions": ["delete", "create"]}},
        {"address": "x.d", "change": {"actions": ["no-op"]}},
    ]}
    r = run(tmp_path, plan)
    assert r.returncode == 0
    assert r.stdout.strip() == "Plan: 1 to add, 1 to change, 1 to destroy, 1 to replace."
    assert "hunter2" not in r.stdout + r.stderr and "123456789012" not in r.stdout + r.stderr


def test_bad_input_fails_without_echoing_it(tmp_path):
    p = tmp_path / "plan.json"
    p.write_text("not json with secret-value-42")
    r = subprocess.run([sys.executable, "-I", str(SCRIPT), str(p)], capture_output=True, text=True)
    assert r.returncode != 0
    assert "secret-value-42" not in r.stdout + r.stderr
