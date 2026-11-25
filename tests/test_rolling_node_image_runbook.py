"""The node runbook gives the address of each machine to replace. Each must pass the guard the
workflow runs, and must name a module and a resource that exist in the layer it is given for."""
import importlib.util
import re
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
TEXT = (ROOT / "docs" / "runbooks" / "rolling-the-node-image.md").read_text()

spec = importlib.util.spec_from_file_location("replace_guard", ROOT / "scripts" / "replace_guard.py")
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)

ROWS = re.findall(r"^\| [^|]+ \| `([a-z-]+)` \| `([^`]+)` \|$", TEXT, re.M)


def test_the_runbook_lists_a_node_and_a_member_for_each_cloud():
    assert len(ROWS) == 6, ROWS


@pytest.mark.parametrize("layer,address", ROWS)
def test_each_address_passes_the_guard_the_workflow_runs(layer, address):
    assert guard.valid_address(address), address


@pytest.mark.parametrize("layer,address", ROWS)
def test_each_address_names_modules_and_a_resource_that_exist(layer, address):
    layer_dir = ROOT / "infra" / "envs" / "europe" / layer
    assert layer_dir.is_dir(), layer
    parts = re.findall(r"module\.([a-z0-9_]+)(?:\[\d+\])?", address)
    resource = re.search(r"\.([a-z_]+)\.(node|member)\[\d+\]$", address)
    assert parts and resource, address
    # The first module is called in the layer; each next one in the module before it.
    source_dir = layer_dir
    for name in parts:
        text = "\n".join(f.read_text() for f in sorted(source_dir.glob("*.tf")))
        found = re.search(rf'module "{name}" \{{[^}}]*?source\s*=\s*"([^"]+)"', text, re.S)
        assert found, f"{name} is not a module in {source_dir}"
        source_dir = (source_dir / found.group(1)).resolve()
    text = "\n".join(f.read_text() for f in sorted(source_dir.glob("*.tf")))
    assert re.search(rf'resource "{resource.group(1)}" "{resource.group(2)}"', text), address


def test_the_other_links_in_the_runbook_exist():
    for target in re.findall(r"\]\(([^)#]+\.md)\)", TEXT):
        assert (ROOT / "docs" / "runbooks" / target).exists(), target
