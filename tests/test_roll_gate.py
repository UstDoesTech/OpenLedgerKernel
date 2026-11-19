"""The health gate between rolled machines. It asks the cloud, so these tests give it fake answers."""
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "roll_gate.py"
spec = importlib.util.spec_from_file_location("roll_gate", SCRIPT)
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)

ADDRESS = "module.c.aws_instance.node[1]"


def state(kind="aws_instance", values=None, address=ADDRESS):
    values = values or {"id": "i-secretid"}
    res = {"address": address, "type": kind, "values": values}
    return {"values": {"root_module": {"child_modules": [{"address": "module.c", "resources": [res]}]}}}


class Cloud:
    """A fake `run` that answers like the cloud. Status answers come in order (the last repeats);
    the disk answer is fixed."""

    def __init__(self, statuses, disks=None):
        self.statuses = list(statuses)
        self.disks = json.dumps(["vol-1"]) if disks is None else json.dumps(disks)

    def __call__(self, args, **kw):
        if "describe-volumes" in args:
            out = self.disks
        else:
            out = self.statuses.pop(0) if len(self.statuses) > 1 else self.statuses[0]
        return subprocess.CompletedProcess(args, 0 if out is not None else 1, out or "", "")


GOOD_STATUS = json.dumps({"InstanceStatuses": [{"InstanceState": {"Name": "running"}, "InstanceStatus": {"Status": "ok"}, "SystemStatus": {"Status": "ok"}}]})
INITIALIZING = json.dumps({"InstanceStatuses": [{"InstanceState": {"Name": "running"}, "InstanceStatus": {"Status": "initializing"}, "SystemStatus": {"Status": "ok"}}]})


class Clock:
    def __init__(self):
        self.t = 0.0
        self.slept = []

    def now(self):
        return self.t

    def sleep(self, s):
        self.slept.append(s)
        self.t += s


def wait(st, run, settle=600, timeout=1200, address=ADDRESS):
    c = Clock()
    return gate.wait(address, st, settle, timeout, run=run, sleep=c.sleep, now=c.now), c


def test_aws_healthy_means_running_checks_ok_and_the_data_disk_attached():
    cloud = Cloud([GOOD_STATUS])
    reason, c = wait(state(), cloud)
    assert reason is None and 600 in c.slept


def test_it_waits_while_the_cloud_checks_are_still_initialising_then_passes():
    cloud = Cloud([INITIALIZING, GOOD_STATUS])
    reason, c = wait(state(), cloud)
    assert reason is None and 15 in c.slept


def test_a_machine_that_never_becomes_healthy_stops_the_roll():
    reason, _ = wait(state(), Cloud([INITIALIZING]), timeout=60)
    assert reason == "the machine was not healthy in time"


def test_a_missing_data_disk_is_not_healthy():
    reason, _ = wait(state(), Cloud([GOOD_STATUS], disks=[]), timeout=30)
    assert reason


def test_a_cloud_error_is_not_healthy():
    reason, _ = wait(state(), Cloud([None]), timeout=30)
    assert reason


def test_a_machine_that_fails_after_the_settle_time_stops_the_roll():
    cloud = Cloud([GOOD_STATUS, INITIALIZING])
    reason, _ = wait(state(), cloud)
    assert reason == "the machine was not healthy after the settle time"


def test_an_address_not_in_the_state_is_refused():
    reason, _ = wait(state(), Cloud([GOOD_STATUS]), address="module.c.aws_instance.node[2]")
    assert reason == "the machine is not in the state"


def test_azure_and_google_checks():
    az = json.dumps({"instanceView": {"statuses": [{"code": "PowerState/running"}]}, "storageProfile": {"dataDisks": [{"lun": 0}]}})
    st = state("azurerm_linux_virtual_machine", {"name": "n", "resource_group_name": "g"}, "module.c.azurerm_linux_virtual_machine.node[1]")
    assert wait(st, Cloud([az]), address="module.c.azurerm_linux_virtual_machine.node[1]")[0] is None
    down = json.dumps({"instanceView": {"statuses": [{"code": "PowerState/deallocated"}]}, "storageProfile": {"dataDisks": [{"lun": 0}]}})
    assert wait(st, Cloud([down]), timeout=30, address="module.c.azurerm_linux_virtual_machine.node[1]")[0]
    gc = json.dumps({"status": "RUNNING", "disks": [{"boot": True}, {"boot": False}]})
    st = state("google_compute_instance", {"name": "n", "zone": "z", "project": "p"}, "module.c.google_compute_instance.node[1]")
    assert wait(st, Cloud([gc]), address="module.c.google_compute_instance.node[1]")[0] is None


def test_the_script_prints_no_identifier(tmp_path):
    path = tmp_path / "state.json"
    path.write_text(json.dumps(state(values={"id": "i-secretid"}, address="module.secretcell.aws_instance.node[1]")))
    out = subprocess.run([sys.executable, str(SCRIPT), "module.other.aws_instance.node[1]", str(path)], capture_output=True, text=True)
    assert out.returncode == 1 and "secret" not in out.stdout + out.stderr
    path.write_text("not json secretid")
    out = subprocess.run([sys.executable, str(SCRIPT), ADDRESS, str(path)], capture_output=True, text=True)
    assert out.returncode == 1 and "secret" not in out.stdout + out.stderr


def test_the_owner_reviews_the_gate():
    owners = (ROOT / ".github" / "CODEOWNERS").read_text()
    assert "/scripts/roll_gate.py" in owners
