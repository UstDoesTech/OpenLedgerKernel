"""The proving environment's pause and resume script, run against stub cloud commands."""
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "proving_power.sh"

# Each stub records its arguments. Listing commands answer with what a stopped or running
# environment would, depending on STATE; every other call is a change.
STUB = r"""#!/usr/bin/env bash
echo "$(basename "$0") $*" >> "$CALLS"
state="${STATE:-running}"
case "$(basename "$0") $*" in
  "aws ec2 describe-instances"*)
    if [[ "$*" == *"instance-state-name,Values=$state"* ]]; then echo "i-1 i-2 i-3"; fi ;;
  "aws rds describe-db-clusters"*) [[ "$state" == running ]] && echo available || echo stopped ;;
  "az vm list"*) [[ "$state" == running ]] && printf 'rg\tbankofmumanddad-azure-001-n1\n' || printf 'rg\tbankofmumanddad-azure-001-n1\n' ;;
  "az postgres flexible-server list"*) printf 'rg\tbankofmumanddad-azure-001\tZoneRedundant\n' ;;
  "gcloud compute instances list"*) echo "bankofmumanddad-gcp-001-n1 europe-north1-a" ;;
  "gcloud sql instances describe"*) [[ "$state" == running ]] && echo ALWAYS || echo NEVER ;;
esac
exit 0
"""
READS = ("describe", " list", "status")


def run(tmp_path, *args, state="running"):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir(exist_ok=True)
    for name in ("aws", "az", "gcloud"):
        stub = bin_dir / name
        stub.write_text(STUB)
        stub.chmod(0o755)
    calls = tmp_path / "calls.txt"
    calls.write_text("")
    env = {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}", "CALLS": str(calls), "STATE": state}
    done = subprocess.run(["bash", str(SCRIPT), *args], env=env, capture_output=True, text=True)
    changes = [c for c in calls.read_text().splitlines() if not any(r in c for r in READS)]
    return done, changes


def test_the_script_is_valid_bash():
    assert subprocess.run(["bash", "-n", str(SCRIPT)]).returncode == 0


def test_without_yes_it_changes_nothing_and_says_what_it_would_do(tmp_path):
    done, changes = run(tmp_path, "pause")
    assert done.returncode == 0, done.stderr
    assert changes == []
    assert "would run: aws ec2 stop-instances" in done.stdout
    assert "Dry run" in done.stdout


def test_pause_with_yes_stops_the_named_machines_and_databases(tmp_path):
    done, changes = run(tmp_path, "pause", "--yes")
    assert done.returncode == 0, done.stderr
    joined = "\n".join(changes)
    assert "aws ec2 stop-instances" in joined and "aws rds stop-db-cluster" in joined
    assert "az vm deallocate" in joined
    assert "gcloud compute instances stop" in joined and "--activation-policy NEVER" in joined
    # Azure cannot stop a zone-redundant server, so it must not try.
    assert "flexible-server stop" not in joined
    assert "high availability" in done.stdout
    # Nothing is ever deleted.
    assert "delete" not in joined and "terminate" not in joined


def test_resume_starts_only_what_is_stopped(tmp_path):
    done, changes = run(tmp_path, "resume", "--yes", state="stopped")
    assert done.returncode == 0, done.stderr
    joined = "\n".join(changes)
    assert "aws ec2 start-instances" in joined and "aws rds start-db-cluster" in joined
    assert "az vm start" in joined and "gcloud compute instances start" in joined and "--activation-policy ALWAYS" in joined
    assert "stop" not in joined.replace("start", "")


def test_scope_cells_leaves_the_control_plane_alone(tmp_path):
    done, changes = run(tmp_path, "pause", "--yes", "--scope", "cells")
    assert done.returncode == 0, done.stderr
    assert changes
    calls = (tmp_path / "calls.txt").read_text()
    assert "ledger-cp-m" not in calls and "bankofmumanddad-" in calls


def test_scope_control_plane_leaves_the_databases_alone(tmp_path):
    done, changes = run(tmp_path, "pause", "--yes", "--scope", "control-plane")
    assert done.returncode == 0, done.stderr
    joined = "\n".join(changes)
    assert "rds" not in joined and "sql" not in joined
    assert "ledger-cp-m" in (tmp_path / "calls.txt").read_text()


def test_bad_arguments_are_refused(tmp_path):
    for args in (["destroy"], ["pause", "--scope", "everything"], []):
        done, changes = run(tmp_path, *args)
        assert done.returncode != 0 and changes == []
