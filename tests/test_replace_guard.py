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
    # An API machine: one for each cell, so no index.
    "module.api_aws.aws_instance.api",
    "module.api_azure.azurerm_linux_virtual_machine.api",
    "module.api_gcp.google_compute_instance.api",
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
    # An API machine is one machine in a module, and nothing wider.
    "aws_instance.api",  # not in a module
    "module.api_aws.aws_instance.api[0]",
    "module.api_aws.aws_instance.api[*]",
    "module.api_aws.aws_instance.apis",
    "module.api_aws.aws_instance.api_extra",
    "module.api_aws.aws_ebs_volume.api",
    "module.api_aws.aws_iam_role.api",
    "module.api_aws.aws_instance.api -target=module.api_azure",
    "module.api_aws.aws_instance.api; echo",
    "module.api_aws.aws_instance.api module.api_azure.azurerm_linux_virtual_machine.api",
    "module.api_aws.aws_instance.api\nmodule.api_azure.azurerm_linux_virtual_machine.api",
    "$(id)module.api_aws.aws_instance.api",
    "module.api_aws.aws_instance.node",  # a node still needs its index
    "module.api_aws",
    "module.api_aws.aws_instance",
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


API = "module.api_aws.aws_instance.api"
API_REPLACE = change(API, "aws_instance", "delete", "create")


def test_a_plan_that_replaces_one_api_machine_is_accepted():
    assert guard.check_plan(API, {"resource_changes": [API_REPLACE]}) is None
    r = change(API, "aws_instance", "create", "delete")
    assert guard.check_plan(API, {"resource_changes": [r, change("t.image", "terraform_data", "update")]}) is None


def test_a_plan_that_replaces_another_api_machine_too_is_refused():
    other = change("module.api_azure.azurerm_linux_virtual_machine.api",
                   "azurerm_linux_virtual_machine", "delete", "create")
    assert guard.check_plan(API, {"resource_changes": [API_REPLACE, other]})
    assert guard.check_plan(API, {"resource_changes": [other]})


@pytest.mark.parametrize("kind", ["aws_ebs_volume", "azurerm_managed_disk", "google_compute_disk"])
def test_an_api_plan_that_deletes_a_disk_is_refused(kind):
    assert guard.check_plan(API, {"resource_changes": [API_REPLACE, change("d.x", kind, "delete")]})


@pytest.mark.parametrize("kind", ["aws_iam_role", "aws_iam_instance_profile", "aws_security_group",
                                  "aws_secretsmanager_secret", "aws_kms_key"])
def test_an_api_plan_that_deletes_the_cells_identity_secret_or_network_is_refused(kind):
    assert guard.check_plan(API, {"resource_changes": [API_REPLACE, change("k.x", kind, "delete", "create")]})


def test_an_api_plan_that_replaces_a_database_node_is_refused():
    node = change("module.c.aws_instance.node[1]", "aws_instance", "delete", "create")
    assert guard.check_plan(API, {"resource_changes": [API_REPLACE, node]})


def test_the_script_accepts_an_api_address_and_prints_nothing_about_it(tmp_path):
    out = run("address", API)
    assert out.returncode == 0 and out.stdout == "" and out.stderr == ""
    path = tmp_path / "plan.json"
    path.write_text(json.dumps({"resource_changes": [API_REPLACE]}))
    out = run("plan", API, str(path))
    assert out.returncode == 0 and out.stdout == ""
    out = run("address", API + "[0]")
    assert out.returncode == 1 and "api_aws" not in out.stdout + out.stderr


# ---- K-20j: an Azure API machine's identity is new with the machine, so its role assignments are too

AZ = "module.api_azure.azurerm_linux_virtual_machine.api"
AZ_REPLACE = change(AZ, "azurerm_linux_virtual_machine", "delete", "create")
SECRET = change("module.api_azure.azurerm_role_assignment.secret", "azurerm_role_assignment", "delete", "create")
REGISTRY = change("module.api_azure.azurerm_role_assignment.registry", "azurerm_role_assignment", "create", "delete")


def test_replacing_an_azure_api_machine_may_recreate_its_two_role_assignments():
    assert guard.check_plan(AZ, {"resource_changes": [AZ_REPLACE, SECRET, REGISTRY]}) is None
    assert guard.check_plan(AZ, {"resource_changes": [AZ_REPLACE, SECRET, REGISTRY,
                                                      change("t.image", "terraform_data", "update")]}) is None


def test_a_role_assignment_that_is_only_deleted_is_refused():
    gone = change("module.api_azure.azurerm_role_assignment.secret", "azurerm_role_assignment", "delete")
    assert guard.check_plan(AZ, {"resource_changes": [AZ_REPLACE, gone]})


def test_a_role_assignment_in_another_module_is_refused():
    other = change("module.api_other.azurerm_role_assignment.secret", "azurerm_role_assignment", "delete", "create")
    assert guard.check_plan(AZ, {"resource_changes": [AZ_REPLACE, other]})
    # A module whose name only begins like the machine's is another module.
    near = change("module.api_azure_two.azurerm_role_assignment.secret", "azurerm_role_assignment", "delete", "create")
    assert guard.check_plan(AZ, {"resource_changes": [AZ_REPLACE, near]})
    top = change("azurerm_role_assignment.secret", "azurerm_role_assignment", "delete", "create")
    assert guard.check_plan(AZ, {"resource_changes": [AZ_REPLACE, top]})


def test_a_database_nodes_plan_may_not_recreate_a_role_assignment():
    node = "module.cell.azurerm_linux_virtual_machine.node[1]"
    plan = {"resource_changes": [
        change(node, "azurerm_linux_virtual_machine", "delete", "create"),
        change("module.cell.azurerm_role_assignment.x", "azurerm_role_assignment", "delete", "create"),
    ]}
    assert guard.check_plan(node, plan)


@pytest.mark.parametrize("kind", ["azurerm_role_definition", "azurerm_key_vault_secret", "azurerm_network_interface",
                                  "azurerm_user_assigned_identity", "azurerm_managed_disk", "azurerm_key_vault"])
def test_no_other_resource_type_may_be_recreated_with_an_azure_api_machine(kind):
    other = change(f"module.api_azure.{kind}.x", kind, "delete", "create")
    assert guard.check_plan(AZ, {"resource_changes": [AZ_REPLACE, other]})


def test_only_one_machine_is_replaced_even_with_role_assignments():
    other = change("module.api_aws.aws_instance.api", "aws_instance", "delete", "create")
    assert guard.check_plan(AZ, {"resource_changes": [AZ_REPLACE, SECRET, other]})
    assert guard.check_plan(AZ, {"resource_changes": [SECRET, REGISTRY]})


def test_the_address_error_says_an_api_machine_is_accepted_and_prints_nothing_it_was_given():
    out = run("address", "module.secretname.azure_instance.api")
    assert out.returncode == 1
    text = out.stdout + out.stderr
    assert "API machine" in text and "no index" in text
    assert "secretname" not in text and "azure_instance" not in text


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
        assert 'replace_guard.py address "$INPUT_REPLACE"' in check, name


def test_both_plans_use_the_same_replace_argument():
    for name, key in (("plan", "Plan"), ("apply", "Plan again and check it is the plan that was shown")):
        run = next(s for s in jobs()[name]["steps"] if s.get("name") == key)["run"]
        assert '-replace=$INPUT_REPLACE' in run and '"${replace_args[@]}"' in run, name


def test_the_plan_is_checked_before_it_is_fingerprinted():
    run = next(s for s in jobs()["plan"]["steps"] if s.get("name") == "Plan")["run"]
    guard_at = run.index('replace_guard.py plan "$INPUT_REPLACE" "$RUNNER_TEMP/plan.json"')
    assert run.index("plan-json") < guard_at < run.index("sha256sum")


def test_the_owner_reviews_the_guard():
    owners = (ROOT / ".github" / "CODEOWNERS").read_text()
    assert re.search(r"(?m)^/scripts/replace_guard\.py\s+@UstDoesTech$", owners)
