"""Rules for the root environments under infra/envs, checked without a cloud account."""
import json
import re
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
INFRA = ROOT / "infra"
ENVS = INFRA / "envs"
GEOS = json.loads((INFRA / "geos.json").read_text())


# The only secrets a layer may take, each as a sensitive variable with no default.
SECRET_VARIABLES = {("interconnect", "preshared_keys"), ("control-plane", "administrator_ssh_public_key"), ("cells", "administrator_ssh_public_key"), ("api", "administrator_ssh_public_key")}


def layers():
    return sorted(p.parent for p in ENVS.glob("*/*/main.tf"))


def text(layer: Path) -> str:
    return "\n".join(
        "\n".join(l for l in tf.read_text().splitlines() if not l.lstrip().startswith("#"))
        for tf in sorted(layer.glob("*.tf"))
    )


def blocks(src: str, kind: str):
    """Yield (name, body) for each top-level `kind "name" {` block, by brace matching."""
    for m in re.finditer(rf'^{kind}\s+"([^"]+)"\s*\{{', src, re.M):
        depth, i = 1, m.end()
        while depth:
            depth += {"{": 1, "}": -1}.get(src[i], 0)
            i += 1
        yield m.group(1), src[m.end():i - 1]


def required_inputs(module: Path):
    src = "\n".join(tf.read_text() for tf in module.glob("variables.tf"))
    return {name for name, body in blocks(src, "variable") if not re.search(r"^\s*default\s*=", body, re.M)}


def set_inputs(body: str):
    """The names set at the top level of a module block, not inside an object value."""
    names, depth = set(), 0
    for line in body.splitlines():
        if depth == 0:
            m = re.match(r"\s*([a-z_]+)\s*=", line)
            if m:
                names.add(m.group(1))
        depth += line.count("{") + line.count("[") + line.count("(") - line.count("}") - line.count("]") - line.count(")")
    return names


def test_europe_has_its_first_three_layers():
    for layer in ("network", "registries", "private-access", "interconnect", "control-plane"):
        assert (ENVS / "europe" / layer).is_dir()


# The registries layer makes repositories only, before anything that needs a network. The authority
# layer makes a key and a secret in AWS and needs no network.
NO_NETWORK = {"network", "registries", "authority"}


@pytest.mark.parametrize("layer", [l for l in layers() if l.name not in NO_NETWORK], ids=lambda p: f"{p.parent.name}/{p.name}")
def test_later_layers_read_the_networks_layer_from_its_state(layer):
    src = text(layer)
    assert 'data "terraform_remote_state" "network"' in src
    assert '"${local.geo}/network"' in src


@pytest.mark.parametrize("env", sorted(p for p in ENVS.iterdir() if p.is_dir()), ids=lambda p: p.name)
def test_each_environment_names_an_enabled_geo(env):
    geo = json.loads((env / "deployment.json").read_text())["geo"]
    assert geo in GEOS["enabled"]


@pytest.mark.parametrize("layer", layers(), ids=lambda p: f"{p.parent.name}/{p.name}")
def test_the_backend_block_is_empty_so_the_workflow_supplies_the_state(layer):
    src = text(layer)
    assert re.search(r'backend\s+"s3"\s*\{\s*\}', src)


@pytest.mark.parametrize("layer", layers(), ids=lambda p: f"{p.parent.name}/{p.name}")
def test_every_module_input_without_a_default_is_set(layer):
    src = text(layer)
    mods = list(blocks(src, "module"))
    assert mods
    for name, body in mods:
        source = re.search(r'source\s*=\s*"([^"]+)"', body).group(1)
        module = (layer / source).resolve()
        assert module.is_dir(), f"{name}: {source}"
        missing = required_inputs(module) - set_inputs(body)
        assert not missing, f"{name} does not set {sorted(missing)}"
        # count and for_each are meta-arguments, not inputs.
        unknown = set_inputs(body) - {"source", "count", "for_each"} - {
            n for n, _ in blocks("\n".join(tf.read_text() for tf in module.glob("variables.tf")), "variable")
        }
        assert not unknown, f"{name} sets inputs the module does not take: {sorted(unknown)}"


@pytest.mark.parametrize("layer", layers(), ids=lambda p: f"{p.parent.name}/{p.name}")
def test_regions_come_only_from_the_regions_module(layer):
    src = text(layer)
    for region in {r for g in GEOS["geos"].values() for rs in g.values() for r in rs}:
        assert region not in src, f"{region} is written by hand"
    for _, body in blocks(src, "provider"):
        for key in ("region", "location"):
            for value in re.findall(rf"^\s*{key}\s*=\s*(.+)$", body, re.M):
                assert "module.regions." in value, value
    assert 'geo' in src and "deployment.json" in src


@pytest.mark.parametrize("layer", layers(), ids=lambda p: f"{p.parent.name}/{p.name}")
def test_no_secret_is_a_variable_or_a_value_here(layer):
    src = text(layer)
    for name, body in blocks(src, "variable"):
        sensitive = bool(re.search(r"sensitive\s*=\s*true", body))
        looks_secret = bool(re.search(r"key|password|token|secret|credential", name, re.I))
        if (layer.name, name) in SECRET_VARIABLES:
            # Named secrets: no default, marked sensitive, and so supplied as TF_VAR_ secrets.
            assert sensitive and not re.search(r"^\s*default\s*=", body, re.M), name
        else:
            assert not sensitive and not looks_secret, name
    assert not list(layer.glob("*.tfvars")) and not list(layer.glob("*.auto.tfvars"))
    assert not re.search(r"AKIA|-----BEGIN|client_secret|access_key", src)


@pytest.mark.parametrize("layer", layers(), ids=lambda p: f"{p.parent.name}/{p.name}")
def test_azure_resource_providers_are_never_registered_by_a_run(layer):
    # A read-only plan identity cannot register them, and a plan must not need to.
    src = text(layer)
    azure = re.search(r'provider\s+"azurerm"\s*\{(.*?)\n\}', src, re.S)
    if azure:
        assert re.search(r'resource_provider_registrations\s*=\s*"none"', azure.group(1))


def test_the_control_plane_reads_its_image_repositories_from_the_registries_layer():
    src = text(ENVS / "europe" / "control-plane")
    assert 'data "terraform_remote_state" "registries"' in src
    for made_elsewhere in ("aws_ecr_repository", "google_artifact_registry_repository"):
        assert made_elsewhere not in src, made_elsewhere


def test_the_registries_layer_makes_immutable_repositories_and_no_secret():
    src = text(ENVS / "europe" / "registries")
    assert 'image_tag_mutability = "IMMUTABLE"' in src
    assert "immutable_tags = true" in src
    assert 'provider "azurerm"' not in src


def test_the_authority_layer_makes_a_signing_key_that_cannot_be_exported_and_holds_no_value():
    src = text(ENVS / "europe" / "authority")
    assert 'key_usage                = "SIGN_VERIFY"' in src
    assert 'customer_master_key_spec = "ECC_NIST_P256"' in src
    # Nothing writes a secret's value, so an apply can never overwrite what the workflow wrote.
    assert "aws_secretsmanager_secret_version" not in src
    assert not re.search(r"secret_string|secret_binary", src)
    assert "variable " not in src
