"""Replacing one machine through infra-apply: the address is checked before tofu sees it, the plan
is checked before anyone approves it, and neither prints a name into a public log."""
import importlib.util
import json
import re
import subprocess
import sys
from pathlib import Path

import pytest
import yaml

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "replace_guard.py"
spec = importlib.util.spec_from_file_location("replace_guard", SCRIPT)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)

GOOD = [
    "aws_instance.node[0]",
    "module.cell.aws_instance.node[2]",
    'module.cells["ten_x"].module.postgres.azurerm_linux_virtual_machine.node[1]',
    "module.control_plane.module.members.google_compute_instance.member[0]",
    'module.cp.aws_instance.member["etcd-n1"]',
]
BAD = [
    "",
    "aws_instance.node",  # no index: that would be every machine
    "aws_instance.node[*]",
    "aws_ebs_volume.data[0]",
    "aws_instance.other[0]",
    "module.x.aws_instance.node[0] -target=module.y",
    "aws_instance.node[0]; echo",
    "aws_instance.node[0] aws_instance.node[1]",
    "$(id)aws_instance.node[0]",
    "module.x.aws_instance.node[0]\nmodule.y.aws_instance.node[1]",
]


@pytest.mark.parametrize("address", GOOD)
def test_one_machine_address_is_accepted(address):
    assert guard.valid_address(address)


@pytest.mark.parametrize("address", BAD)
def test_anything_else_is_refused(address):
    assert not guard.valid_address(address)


def change(address, kind, *actions):
    return {"address": address, "type": kind, "change": {"actions": list(actions)}}


MACHINE = "module.c.aws_instance.node[1]"
REPLACE = change(MACHINE, "aws_instance", "delete", "create")
ATTACH = change("module.c.aws_volume_attachment.data[1]", "aws_volume_attachment", "delete", "create")


def test_a_plan_that_replaces_the_machine_and_its_attachment_is_accepted():
    assert guard.check_plan(MACHINE, {"resource_changes": [REPLACE, ATTACH]}) is None


def test_the_machine_may_be_replaced_with_the_plan_order_create_then_delete():
    r = change(MACHINE, "aws_instance", "create", "delete")
    assert guard.check_plan(MACHINE, {"resource_changes": [r, ATTACH]}) is None


@pytest.mark.parametrize("disk", ["aws_ebs_volume", "azurerm_managed_disk", "google_compute_disk"])
def test_a_plan_that_deletes_a_data_disk_is_refused(disk):
    plan = {"resource_changes": [REPLACE, change("d.x", disk, "delete", "create")]}
    assert guard.check_plan(MACHINE, plan)


def test_a_plan_that_replaces_another_machine_too_is_refused():
    other = change("module.c.aws_instance.node[2]", "aws_instance", "delete", "create")
    assert guard.check_plan(MACHINE, {"resource_changes": [REPLACE, other]})


def test_a_plan_that_replaces_a_different_machine_is_refused():
    assert guard.check_plan("module.c.aws_instance.node[0]", {"resource_changes": [REPLACE]})


def test_a_plan_that_does_not_replace_anything_is_refused():
    assert guard.check_plan(MACHINE, {"resource_changes": []})
    assert guard.check_plan(MACHINE, {"resource_changes": [change(MACHINE, "aws_instance", "update")]})


def test_a_plan_that_deletes_anything_else_is_refused():
    plan = {"resource_changes": [REPLACE, change("k.x", "aws_kms_key", "delete")]}
    assert guard.check_plan(MACHINE, plan)


def test_updates_and_creates_elsewhere_do_not_block_it():
    plan = {"resource_changes": [REPLACE, change("a.b", "terraform_data", "update"), change("c.d", "aws_x", "create")]}
    assert guard.check_plan(MACHINE, plan) is None


def run(*args):
    return subprocess.run([sys.executable, str(SCRIPT), *args], capture_output=True, text=True)


def test_the_script_prints_no_address_or_plan_content(tmp_path):
    secret = "module.cell_ten_secretname.aws_instance.node[7]"
    out = run("address", "bad " + secret)
    assert out.returncode == 1 and "secretname" not in out.stdout + out.stderr
    path = tmp_path / "plan.json"
    path.write_text(json.dumps({"resource_changes": [change(secret, "aws_instance", "delete", "create"),
                                                     change("d.secretdisk", "aws_ebs_volume", "delete", "create")]}))
    out = run("plan", secret, str(path))
    assert out.returncode == 1 and "secret" not in out.stdout + out.stderr
    path.write_text("not json secretname")
    out = run("plan", secret, str(path))
    assert out.returncode == 1 and "secretname" not in out.stdout + out.stderr


# The workflow side.
WORKFLOW = ROOT / ".github" / "workflows" / "infra-apply.yml"


def jobs():
    return yaml.safe_load(WORKFLOW.read_text())["jobs"]


def test_the_replace_input_is_optional_and_a_string():
    wf = yaml.safe_load(WORKFLOW.read_text())
    inputs = wf.get("on", wf.get(True))["workflow_dispatch"]["inputs"]
    assert inputs["replace"]["type"] == "string" and inputs["replace"]["required"] is False
    assert inputs["replace"]["default"] == ""


def test_both_jobs_check_the_address_and_pass_it_through_the_environment_only():
    assert "${{ inputs.replace }}" in WORKFLOW.read_text()
    for name, job in jobs().items():
        assert job["env"]["INPUT_REPLACE"] == "${{ inputs.replace }}", name
        check = next(s for s in job["steps"] if s.get("name") == "Check the inputs")["run"]
        assert 'replace_guard.py addresses "$INPUT_REPLACE"' in check, name


def test_both_plans_use_the_same_replace_argument():
    for name, key in (("plan", "Plan"), ("apply", "Plan again and check it is the plan that was shown")):
        run = next(s for s in jobs()[name]["steps"] if s.get("name") == key)["run"]
        assert 'first="${INPUT_REPLACE%%,*}"' in run, name
        assert '-replace=$first' in run and '"${replace_args[@]}"' in run, name


def test_the_plan_is_checked_before_it_is_fingerprinted():
    run = next(s for s in jobs()["plan"]["steps"] if s.get("name") == "Plan")["run"]
    guard_at = run.index('replace_guard.py plan "$first" "$RUNNER_TEMP/plan.json"')
    assert run.index("plan-json") < guard_at < run.index("sha256sum")


def test_the_owner_reviews_the_guard():
    owners = (ROOT / ".github" / "CODEOWNERS").read_text()
    assert re.search(r"(?m)^/scripts/replace_guard\.py\s+@UstDoesTech$", owners)


# Rolling several machines.
CELL = "module.cell_aws.module.aws_self_hosted[0]"


def test_one_to_three_machines_of_one_cell_are_accepted():
    for n in (1, 2, 3):
        assert guard.valid_addresses(",".join(f"{CELL}.aws_instance.node[{i}]" for i in range(n)))


@pytest.mark.parametrize("text", [
    "",
    ",",
    f"{CELL}.aws_instance.node[0],{CELL}.aws_instance.node[0]",  # repeated
    ",".join(f"{CELL}.aws_instance.node[{i}]" for i in range(4)),  # four
    f"{CELL}.aws_instance.node[0],module.other.aws_instance.node[1]",  # two cells
    f"{CELL}.aws_instance.node[0], {CELL}.aws_instance.node[1]",  # a space
    f"{CELL}.aws_instance.node[0],aws_ebs_volume.data[1]",
    f"{CELL}.aws_instance.node[0],{CELL}.aws_instance.node",
])
def test_other_lists_are_refused(text):
    assert not guard.valid_addresses(text)


def test_the_rolling_steps_are_in_the_workflow():
    apply = {s["name"]: s for s in jobs()["apply"]["steps"] if "name" in s}
    plan = {s["name"]: s for s in jobs()["plan"]["steps"] if "name" in s}
    later = plan["Plan the rest of the roll"]
    assert later["if"] == "contains(env.INPUT_REPLACE, ',')"
    assert 'replace_guard.py plan "$a"' in later["run"]
    assert jobs()["plan"]["outputs"]["later_shas"] == "${{ steps.later.outputs.later_shas }}"
    roll = apply["Roll the remaining machines, one at a time"]["run"]
    # Each later machine: plan again, compare with what was shown, check, apply, wait for health.
    order = ['"replan-$n"', 'replace_guard.py plan "$a"', 'if [ "$sha" != "${reviewed[$((n - 1))]}" ]', '"apply-$n"', "roll_gate.py"]
    positions = [roll.index(x) for x in order]
    assert positions == sorted(positions)
    assert "exit 1" in roll
    first = apply["Wait for the machine to be healthy"]["run"]
    assert "roll_gate.py" in first
    names = list(apply)
    assert names.index("Apply the saved plan") < names.index("Wait for the machine to be healthy") < names.index(
        "Roll the remaining machines, one at a time") < names.index("Keep the logs in the plan store")


def test_the_settle_time_is_a_small_whole_number_checked_before_use():
    wf = yaml.safe_load(WORKFLOW.read_text())
    assert wf.get("on", wf.get(True))["workflow_dispatch"]["inputs"]["settle_minutes"]["default"] == "10"
    check = next(s for s in jobs()["apply"]["steps"] if s.get("name") == "Check the inputs")["run"]
    assert '[[ "$INPUT_SETTLE" =~ ^[0-9]{1,2}$ ]]' in check and '-le 30' in check
    assert jobs()["apply"]["env"]["INPUT_SETTLE"] == "${{ inputs.settle_minutes }}"
