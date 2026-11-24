"""The runbook for rolling a new API image (K-20i) names real workflows, real inputs, real files and
real machine addresses, so that no step in it is one that cannot be done."""
import importlib.util
import re
from pathlib import Path

import pytest
import yaml

ROOT = Path(__file__).resolve().parent.parent
RUNBOOKS = ROOT / "docs" / "runbooks"
RUNBOOK = RUNBOOKS / "rolling-the-api-image.md"
TEXT = RUNBOOK.read_text()

spec = importlib.util.spec_from_file_location("replace_guard", ROOT / "scripts" / "replace_guard.py")
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)

# Each workflow the runbook sends a person to, and the inputs it tells them to set.
WORKFLOWS = {
    "publish-api-image": [],
    "publish-images": ["env", "image"],
    "infra-apply": ["env", "layer", "action", "replace"],
    "rotate-credentials": ["env", "kind", "proving"],
}


def inputs_of(name):
    workflow = yaml.safe_load((ROOT / ".github" / "workflows" / f"{name}.yml").read_text())
    trigger = workflow.get(True) or workflow["on"]
    return (trigger["workflow_dispatch"] or {}).get("inputs") or {}


@pytest.mark.parametrize("name", sorted(WORKFLOWS))
def test_each_workflow_it_names_exists_and_has_the_inputs_it_sets(name):
    assert f"**`{name}`**" in TEXT, f"the runbook should name {name}"
    have = inputs_of(name)
    for wanted in WORKFLOWS[name]:
        assert wanted in have, f"{name} has no input {wanted}"
        assert f"`{wanted}`" in TEXT


def test_it_sets_only_inputs_that_exist_and_values_that_are_allowed():
    assert "api" in inputs_of("publish-images")["image"]["options"]
    assert "all" in inputs_of("rotate-credentials")["kind"]["options"]
    assert {"plan", "apply"} <= set(inputs_of("infra-apply")["action"]["options"])
    # The workflow takes no input the runbook has not named: nothing for a person to guess at.
    assert inputs_of("publish-api-image") == {}


def test_the_layers_it_names_exist():
    for layer in ["cell-first", "api"]:
        assert (ROOT / "infra" / "envs" / "europe" / layer / "main.tf").is_file(), layer
    assert "`layer` = `cell-first`" in TEXT and "`layer` = `api`" in TEXT


def test_every_file_and_link_it_names_exists():
    paths = set(re.findall(r"`((?:infra|scripts|docs|crates|tests|\.github)/[A-Za-z0-9_./-]+)`", TEXT))
    assert "infra/images.json" in paths and "scripts/first_entry_check.sh" in paths
    for path in paths:
        assert (ROOT / path).exists(), path
    for target in re.findall(r"\]\(([^)#]+)(?:#[^)]*)?\)", TEXT):
        if target.startswith("http"):
            continue
        assert (RUNBOOKS / target).resolve().exists(), target


def test_the_anchor_it_links_to_exists():
    anchors = re.findall(r"\]\(first-entry\.md#([^)]+)\)", TEXT)
    headings = {
        re.sub(r"[^a-z0-9 -]", "", h.lower()).strip().replace(" ", "-")
        for h in re.findall(r"^#+ (.+)$", (RUNBOOKS / "first-entry.md").read_text(), re.M)
    }
    for anchor in anchors:
        assert anchor in headings, anchor


def test_every_machine_address_it_gives_is_one_the_workflow_will_accept():
    addresses = re.findall(r"`(module\.[^`]+)`", TEXT)
    assert len(addresses) == 3, "one for each cloud"
    for address in addresses:
        assert guard.valid_address(address), address
    # And each is the real resource: the module and the machine are named so in the api layer.
    layer = (ROOT / "infra" / "envs" / "europe" / "api" / "main.tf").read_text()
    for address in addresses:
        module = address.split(".")[1]
        assert f'module "{module}"' in layer, module
        kind = address.split(".")[2]
        cloud = module.removeprefix("api_")
        source = (ROOT / "infra" / "modules" / "api" / cloud / "main.tf").read_text()
        assert f'resource "{kind}" "api"' in source, address


def test_the_digest_is_recorded_where_the_layers_read_it_from():
    assert "`api` entry of `infra/images.json`" in TEXT
    assert (ROOT / "infra" / "envs" / "europe" / "cell-first" / "main.tf").read_text().count("images.json") >= 1
    message = "The cell-first layer holds a different API image digest from infra/images.json"
    assert message in (ROOT / "infra" / "envs" / "europe" / "api" / "main.tf").read_text()
    assert message in TEXT


def test_it_says_why_a_restart_is_not_enough_and_how_to_check_a_machine():
    for part in ["ignore_changes", "replaced", "one at a time", "/healthz", "/ui", "0 to change"]:
        assert part in TEXT, part
    for kind in ["user_data", "custom_data", 'metadata["user-data"]']:
        assert kind in TEXT


def test_the_machines_really_do_ignore_a_changed_boot_configuration():
    for cloud, setting in [("aws", "user_data"), ("azure", "custom_data"), ("gcp", 'metadata["user-data"]')]:
        source = (ROOT / "infra" / "modules" / "api" / cloud / "main.tf").read_text()
        lines = [line for line in source.splitlines() if "ignore_changes" in line]
        assert any(setting in line for line in lines), cloud


def test_the_node_image_runbook_no_longer_says_the_workflow_cannot_replace_a_machine():
    node = (RUNBOOKS / "rolling-the-node-image.md").read_text()
    assert "cannot replace one machine" not in node
    assert "One thing is not ready yet" not in node
    assert "rolling-the-api-image.md" in node


def test_the_runbook_is_in_the_index():
    assert "(rolling-the-api-image.md)" in (RUNBOOKS / "README.md").read_text()


def test_it_prints_no_secret_and_no_cell_name():
    assert not re.search(r"sha256:[0-9a-f]{64}", TEXT), "no real digest in the runbook"
    for word in ["bankofmumanddad", "kv-ledger"]:
        assert word not in TEXT


def test_it_says_an_azure_plan_shows_two_role_assignments_replaced_and_the_module_has_them():
    assert "two role assignments" in TEXT and "`secret`" in TEXT and "`registry`" in TEXT
    azure = (ROOT / "infra" / "modules" / "api" / "azure" / "main.tf").read_text()
    for name in ["secret", "registry"]:
        assert f'resource "azurerm_role_assignment" "{name}"' in azure
    assert "azurerm_linux_virtual_machine.api.identity[0].principal_id" in azure
