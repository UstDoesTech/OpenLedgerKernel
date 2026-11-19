#!/usr/bin/env python3
"""Wait until a replaced machine is up, with its data disk attached, then let it settle.

  roll_gate.py <address> <state.json> [--settle-minutes N] [--timeout-minutes M]

Reads the machine's identity from `tofu show -json` (the state), asks its cloud for the machine's
status and exits 0 when it is healthy for the settle time, or 1 if it is not healthy in time.

What this proves: the machine is running, the cloud's own checks pass (AWS) and the data disk is
attached. It does NOT prove the node's service is running or has caught up with its peers. The
runners cannot reach the private networks. The settle time and the person who approved the roll
carry that until the nodes publish their own health.

The state and the cloud's answers hold identifiers and this repository's logs are public, so the
script prints a short reason and nothing it read.
"""
import argparse
import json
import subprocess
import sys
import time


def find(values: dict, address: str):
    def walk(module):
        for r in module.get("resources", []):
            if r.get("address") == address:
                return r
        for child in module.get("child_modules", []):
            found = walk(child)
            if found:
                return found
        return None

    return walk(values.get("values", {}).get("root_module", {}))


def cli(args: list, run=subprocess.run):
    out = run(args, capture_output=True, text=True, timeout=120)
    if out.returncode != 0:
        return None
    return out.stdout.strip()


def aws_ok(values: dict, run=subprocess.run) -> bool:
    iid = values["id"]
    raw = cli(["aws", "ec2", "describe-instance-status", "--instance-ids", iid, "--include-all-instances", "--output", "json"], run)
    if raw is None:
        return False
    statuses = json.loads(raw).get("InstanceStatuses", [])
    if len(statuses) != 1:
        return False
    s = statuses[0]
    if s["InstanceState"]["Name"] != "running" or s["InstanceStatus"]["Status"] != "ok" or s["SystemStatus"]["Status"] != "ok":
        return False
    disks = cli(["aws", "ec2", "describe-volumes", "--filters", f"Name=attachment.instance-id,Values={iid}",
                 "Name=attachment.device,Values=/dev/sdf", "Name=attachment.status,Values=attached",
                 "--query", "Volumes[].VolumeId", "--output", "json"], run)
    return disks is not None and len(json.loads(disks)) == 1


def azure_ok(values: dict, run=subprocess.run) -> bool:
    base = ["--name", values["name"], "--resource-group", values["resource_group_name"], "--output", "json"]
    raw = cli(["az", "vm", "get-instance-view", *base, "--only-show-errors"], run)
    if raw is None:
        return False
    view = json.loads(raw)
    powered = any(s.get("code") == "PowerState/running" for s in view.get("instanceView", {}).get("statuses", []))
    attached = any(d.get("lun") == 0 for d in view.get("storageProfile", {}).get("dataDisks", []))
    return powered and attached


def gcp_ok(values: dict, run=subprocess.run) -> bool:
    raw = cli(["gcloud", "compute", "instances", "describe", values["name"], "--zone", values["zone"],
               "--project", values["project"], "--format", "json"], run)
    if raw is None:
        return False
    d = json.loads(raw)
    return d.get("status") == "RUNNING" and sum(1 for x in d.get("disks", []) if not x.get("boot")) == 1


CHECKS = {"aws_instance": aws_ok, "azurerm_linux_virtual_machine": azure_ok, "google_compute_instance": gcp_ok}


def wait(address, state, settle, timeout, run=subprocess.run, sleep=time.sleep, now=time.monotonic):
    resource = find(state, address)
    if not resource or resource.get("type") not in CHECKS:
        return "the machine is not in the state"
    check = CHECKS[resource["type"]]
    deadline = now() + timeout
    while True:
        try:
            healthy = check(resource["values"], run)
        except Exception:  # an unexpected answer is not healthy; never echo it
            healthy = False
        if healthy:
            break
        if now() >= deadline:
            return "the machine was not healthy in time"
        sleep(15)
    sleep(settle)
    try:
        if not check(resource["values"], run):
            return "the machine was not healthy after the settle time"
    except Exception:
        return "the machine was not healthy after the settle time"
    return None


def main(argv: list) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("address")
    p.add_argument("state")
    p.add_argument("--settle-minutes", type=int, default=10)
    p.add_argument("--timeout-minutes", type=int, default=20)
    try:
        a = p.parse_args(argv[1:])
        with open(a.state, encoding="utf-8") as f:
            state = json.load(f)
        reason = wait(a.address, state, a.settle_minutes * 60, a.timeout_minutes * 60)
    except SystemExit:
        return 2
    except Exception:
        reason = "could not check the machine"
    if reason:
        print(f"::error::{reason}. The roll stopped here. Later machines were not touched.")
        return 1
    print("The machine is healthy.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
