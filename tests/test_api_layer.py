"""K-16: the api layer of the Europe environment. OpenTofu validates it in CI; these hold what
validation cannot: the layer reads the right state, the image is the manifest's, nothing is
public and no secret is a value here."""
import json
import re
from pathlib import Path

from test_envs import ENVS, blocks, text

ROOT = Path(__file__).resolve().parent.parent
LAYER = ENVS / "europe" / "api"
CELL_FIRST = ENVS / "europe" / "cell-first"
FLOWS = json.loads((ROOT / "infra" / "flows.json").read_text())


def src():
    return text(LAYER)


def test_one_machine_next_to_each_cell_from_the_api_module():
    mods = dict(blocks(src(), "module"))
    for cloud in ("aws", "azure", "gcp"):
        body = mods[f"api_{cloud}"]
        assert f'source = "../../../modules/api/{cloud}"' in body
        assert f"cell  " in body and f"local.names.{cloud}" in body
        assert re.search(rf'name\s+= "\${{local\.names\.{cloud}}}-api"', body)


def test_it_reads_the_earlier_layers_and_the_cell_names_match_the_cells_layer():
    s = src()
    for layer in ("network", "private_access", "control_plane", "cell_first"):
        assert f'data "terraform_remote_state" "{layer}"' in s, layer
    cells = (ENVS / "europe" / "cells" / "cells.tf").read_text()
    for cloud in ("aws", "azure", "gcp"):
        assert f'name                 = "${{local.slug}}-{cloud}-001"' in cells
        assert f'{cloud}   = "${{local.slug}}-{cloud}-001"' in s or f'{cloud} = "${{local.slug}}-{cloud}-001"' in s
    assert "local.cell_first.tenant" in s and not re.search(r"ten_[0-9a-z]+", s.replace('trimprefix(local.tenant, "ten_")', ""))


def test_the_machines_go_in_the_api_tier_and_nowhere_else():
    s = src()
    assert 'subnet_ids["api"]' in s
    assert 'security_group_ids["api"]' in s
    assert 'subnet_ids["cell"]' not in s and 'security_group_ids["cell"]' not in s


def test_the_image_must_be_the_manifests_before_any_machine_is_made():
    s = src()
    body = re.search(r'resource "terraform_data" "image" \{(.*?)\n\}', s, re.S).group(1)
    for cloud in ("aws", "azure", "gcp"):
        assert f"endswith(local.cell_first.api.{cloud}.image" in body
        assert f"terraform_data.image.output.{cloud}" in s
    assert "@${local.api.digest}" in body and "local.images.api" in s


def test_cell_first_builds_the_api_image_address_from_the_manifest_with_no_variable():
    s = text(CELL_FIRST)
    assert 'jsondecode(file("${path.module}/../../../images.json")).api.digest' in s
    assert not re.search(r'variable "api_image_digest"', s)
    for name in ("aws_ecr_repository", "google_artifact_registry_repository"):
        assert f'resource "{name}" "api"' in s
    body = re.search(r'resource "aws_ecr_repository" "api" \{(.*?)\n\}', s, re.S).group(1)
    assert 'image_tag_mutability = "IMMUTABLE"' in body
    assert "immutable_tags = true" in s


def test_the_public_port_opens_only_to_named_private_ranges_and_never_by_default():
    s = src()
    var = dict(blocks(s, "variable"))["client_cidrs"]
    assert re.search(r"default\s+= \[\]", var)
    assert not re.search(r"(?<![0-9.])0\.0\.0\.0/0", s) and "::/0" not in s
    for resource in ("aws_vpc_security_group_ingress_rule", "azurerm_network_security_rule", "google_compute_firewall"):
        assert f'resource "{resource}" "clients"' in s
    assert "local.client_port" in s and re.search(r"client_port\s+= 8443", s)
    # No rule is made without a range.
    assert s.count("length(var.client_cidrs) > 0") == 2 and "for_each = toset(var.client_cidrs)" in s


def test_the_ports_between_tiers_are_flows_not_written_here():
    flow = next(f for f in FLOWS["flows"] if f["name"] == "api-to-api-internal")
    assert (flow["source"], flow["destination"], FLOWS["ports"][flow["port"]]) == ("api", "api", 8444)
    assert not re.search(r"\b(5432|2379|2380|8444)\b", src())
    assert "ports.api_internal" in src(), "the internal port is read from flows.json"


def test_nothing_secret_is_a_value_here_and_the_one_variable_is_named_as_a_secret():
    s = src()
    assert not re.search(r"password|secret_version|BEGIN|postgres://", s, re.I)
    variables = {name for name, _ in blocks(s, "variable")}
    assert variables == {"azure_subscription_id", "state_bucket", "azure_flatcar_image_id", "administrator_ssh_public_key", "client_cidrs", "edge_enabled",
                         "front_domain_name", "certificate_validated", "edge_client_cidrs", "open_to_internet", "migrate_database", "create_database_role"}


def test_outputs_say_where_each_secret_is_and_hold_no_contents():
    out = (LAYER / "outputs.tf").read_text()
    for field in ("secret_ref", "secret_files", "address", "tls_terminates", "cell"):
        assert field in out
    assert not re.search(r"secret_string|secret_value|\.value\b", out)
