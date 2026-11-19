"""The region guard: a configuration using a region outside its geo must fail."""
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "check_uk_regions.py"

spec = importlib.util.spec_from_file_location("check_uk_regions", SCRIPT)
guard = importlib.util.module_from_spec(spec)
sys.modules["check_uk_regions"] = guard  # dataclasses look the module up by name
spec.loader.exec_module(guard)

POLICY, POLICY_ERRORS = guard.load_policy(ROOT / "infra" / "geos.json")
ALLOWED = POLICY.allowed("uk")


def problems(text: str, allowed=ALLOWED):
    return guard.scan_text(text, allowed)


def run(*paths):
    return subprocess.run([sys.executable, str(SCRIPT), *map(str, paths)], capture_output=True, text=True)


@pytest.mark.parametrize(
    "line",
    [
        'provider "aws" { region = "eu-west-2" }',
        'availability_zone = "eu-west-2a"',
        'zones = ["eu-west-2a", "eu-west-2b", "eu-west-2c"]',
        'location = "uksouth"',
        'location = "UK South"',
        'location = "ukwest"',
        'location = "UK West"',
        'region   = "europe-west2"',
        'zone     = "europe-west2-b"',
        'location = "global"',
        'region = "${var.region}"',
        'region = var.region',
        'name = "payments-ledger"',
        'description = "primary in London, standby in Frankfurt"',
        '# region = "us-east-1"',
        '// location = "westeurope"',
        '/* region = "us-east-1" */',
        'region = "us-east-1" # uk-guard: ignore a test fixture for the guard itself',
        # Names that only contain the letters "location" are not regions.
        'allocation_method = "Static"',
        'private_ip_address_allocation = "Dynamic"',
        'relocation_policy = "Manual"',
        'allocation_policy = "Standard"',
        'dislocation = "Somewhere"',
    ],
)
def test_uk_and_unrelated_values_pass(line):
    assert problems(line) == [], line


@pytest.mark.parametrize(
    "line",
    [
        'provider "aws" { region = "us-east-1" }',
        'region = "eu-west-1"',  # Ireland, in Europe but not the UK
        'availability_zone = "eu-central-1a"',
        'location = "westeurope"',
        'location = "West Europe"',
        'location = "northeurope"',
        'location = "EastUS2"',
        'location = "swedencentral"',  # caught by the attribute rule or the list
        'location = "EU"',  # a Google multi-region is not the UK
        'region = "europe-west1"',
        'zone = "europe-west4-a"',
        'zones = ["uksouth", "westeurope"]',
        'default = "us-central1"',  # a variable default, any attribute name
        'default = "ap-southeast-2"',
        'location = "someplace-new"',  # unknown, region-shaped: refused rather than guessed
        # Every attribute whose name is, or has a word that is, region or location.
        'source_region = "someplace-new"',
        'gateway_location = "someplace-new"',
        'gateway-location = "someplace-new"',
        'gatewayLocation = "someplace-new"',
        'region_name = "someplace-new"',
        'primary.location = "someplace-new"',
        'Location = "someplace-new"',
        'source_region = "westeurope"',
        'gateway_location = "us-east-1"',
    ],
)
def test_regions_outside_the_uk_fail(line):
    assert problems(line), line


def test_the_two_interconnect_exemptions_are_gone_and_the_guard_still_passes():
    text = (ROOT / "infra" / "modules" / "interconnect" / "azure.tf").read_text()
    assert "uk-guard" not in text
    assert "allocation_method" in text and "private_ip_address_allocation" in text
    assert run(ROOT / "infra" / "modules" / "interconnect").returncode == 0


@pytest.mark.parametrize(
    "name, expected",
    [
        ("location", True),
        ("region", True),
        ("source_region", True),
        ("gateway_location", True),
        ("gatewayLocation", True),
        ("LOCATION", True),
        ("regions", True),
        ("allocation_method", False),
        ("private_ip_address_allocation", False),
        ("relocation", False),
        ("dislocation", False),
        ("regional_settings", False),
    ],
)
def test_which_attribute_names_count_as_regions(name, expected):
    assert guard.names_a_region(name) is expected


def test_line_numbers_and_files_are_reported(tmp_path):
    (tmp_path / "main.tf").write_text('# fine\nprovider "aws" {\n  region = "us-west-2"\n}\n')
    r = run(tmp_path)
    assert r.returncode == 1
    assert "main.tf:3:" in r.stderr and "us-west-2" in r.stderr


def test_tfvars_and_nested_directories_are_scanned(tmp_path):
    d = tmp_path / "modules" / "x"
    d.mkdir(parents=True)
    (d / "prod.tfvars").write_text('location = "westeurope"\n')
    assert run(tmp_path).returncode == 1


def test_a_clean_tree_passes(tmp_path):
    (tmp_path / "main.tf").write_text('provider "aws" {\n  region = "eu-west-2"\n}\n')
    r = run(tmp_path)
    assert r.returncode == 0, r.stderr


def test_json_configuration_is_refused_because_it_is_not_scanned(tmp_path):
    (tmp_path / "main.tf.json").write_text("{}")
    assert run(tmp_path).returncode == 1


def test_the_real_infra_tree_is_clean():
    r = run()
    assert r.returncode == 0, r.stderr
    assert not POLICY_ERRORS


# The real policy, as a person must change it: these assertions force a reviewed edit to
# enable another geo or to change what the UK geo allows.
def test_the_uk_and_europe_geos_are_enabled_and_allow_exactly_their_regions():
    assert POLICY.enabled == ["uk", "europe"]
    assert POLICY.geos == {
        "uk": {"aws": {"eu-west-2"}, "azure": {"uksouth", "ukwest"}, "gcp": {"europe-west2"}},
        # North Europe, for proving the platform. No real customer or ledger data (infra/README.md).
        "europe": {"aws": {"eu-north-1"}, "azure": {"northeurope"}, "gcp": {"europe-north1"}},
    }


# ---- policy file and several geos -------------------------------------------------------

UK = {"aws": ["eu-west-2"], "azure": ["uksouth", "ukwest"], "gcp": ["europe-west2"]}
EUROPE = {"aws": ["eu-west-1", "eu-central-1"], "azure": ["westeurope", "northeurope"], "gcp": ["europe-west1", "europe-west4"]}
US = {"aws": ["us-east-1"], "azure": ["eastus"], "gcp": ["us-central1"]}


def tree(tmp_path, enabled, geos=None, envs=None, modules=None):
    """Write a policy and environments: envs maps name -> (deployment geo or None, main.tf text)."""
    geos = geos if geos is not None else {"uk": UK, "europe": EUROPE, "us": US}
    (tmp_path / "geos.json").write_text(json.dumps({"enabled": enabled, "geos": geos}))
    for name, (geo, text) in (envs or {}).items():
        d = tmp_path / "envs" / name
        d.mkdir(parents=True)
        (d / "main.tf").write_text(text)
        if geo is not None:
            (d / "deployment.json").write_text(json.dumps({"geo": geo}))
    for name, text in (modules or {}).items():
        d = tmp_path / "modules" / name
        d.mkdir(parents=True)
        (d / "main.tf").write_text(text)
    return tmp_path


def test_another_geo_is_added_by_data_alone(tmp_path):
    t = tree(tmp_path, ["uk", "europe"], envs={
        "uk-prod": ("uk", 'region = "eu-west-2"\nlocation = "UK West"\n'),
        "eu-prod": ("europe", 'region = "eu-west-1"\nlocation = "West Europe"\nzone = "europe-west4-a"\n'),
    })
    assert run(t).returncode == 0, run(t).stderr


def test_an_environment_may_only_use_the_regions_of_its_own_geo(tmp_path):
    t = tree(tmp_path, ["uk", "europe"], envs={
        "uk-prod": ("uk", 'region = "eu-west-1"\n'),  # a European region in a UK environment
        "eu-prod": ("europe", 'location = "uksouth"\n'),  # and the other way round
    })
    r = run(t)
    assert r.returncode == 1
    assert "uk-prod" in r.stderr and "eu-prod" in r.stderr


def test_a_geo_that_is_defined_but_not_enabled_cannot_be_used(tmp_path):
    t = tree(tmp_path, ["uk"], envs={"us-prod": ("us", 'region = "us-east-1"\n')})
    r = run(t)
    assert r.returncode == 1 and "not enabled" in r.stderr


def test_regions_of_a_geo_that_is_not_enabled_are_refused_in_modules(tmp_path):
    t = tree(tmp_path, ["uk"], modules={"m": 'region = "us-east-1"\n'})
    assert run(t).returncode == 1


def test_modules_may_use_the_regions_of_any_enabled_geo(tmp_path):
    t = tree(tmp_path, ["uk", "europe"], modules={"m": 'region = "eu-west-1"\nlocation = "uksouth"\n'})
    assert run(t).returncode == 0, run(t).stderr


def test_an_environment_must_name_its_geo(tmp_path):
    t = tree(tmp_path, ["uk"], envs={"prod": (None, 'region = "eu-west-2"\n')})
    r = run(t)
    assert r.returncode == 1 and "deployment.json" in r.stderr


def test_an_environment_naming_an_unknown_geo_or_a_bad_file_fails(tmp_path):
    t = tree(tmp_path, ["uk"], envs={"prod": ("mars", 'region = "eu-west-2"\n')})
    assert run(t).returncode == 1
    (t / "envs" / "prod" / "deployment.json").write_text('{"geo": "uk", "extra": 1}')
    assert run(t).returncode == 1
    (t / "envs" / "prod" / "deployment.json").write_text("not json")
    assert run(t).returncode == 1


def test_configuration_loose_in_envs_names_no_geo_and_fails(tmp_path):
    t = tree(tmp_path, ["uk"])
    (t / "envs").mkdir()
    (t / "envs" / "main.tf").write_text('region = "eu-west-2"\n')
    assert run(t).returncode == 1


def test_multi_regions_are_refused_in_every_geo(tmp_path):
    t = tree(tmp_path, ["uk", "europe"], envs={"eu-prod": ("europe", 'location = "EU"\n')})
    assert run(t).returncode == 1


@pytest.mark.parametrize(
    "bad",
    [
        {"enabled": ["uk"], "geos": {}},
        {"enabled": [], "geos": {"uk": UK}},
        {"enabled": ["europe"], "geos": {"uk": UK}},  # enabled but not defined
        {"enabled": ["uk"], "geos": {"uk": {**UK, "aws": []}}},  # empty list
        {"enabled": ["uk"], "geos": {"uk": {"aws": UK["aws"], "azure": UK["azure"]}}},  # missing a cloud
        {"enabled": ["uk"], "geos": {"uk": {**UK, "aws": ["eu-west-2a"]}}},  # a zone, not a region
        {"enabled": ["uk"], "geos": {"uk": {**UK, "gcp": ["not a region"]}}},
        {"enabled": ["uk", "europe"], "geos": {"uk": UK, "europe": {**EUROPE, "aws": ["eu-west-2"]}}},  # in two geos
        {"enabled": ["uk"], "geos": {"UK!": UK}},
        {"enabled": ["uk"], "geos": {"uk": UK}, "extra": 1},
    ],
)
def test_a_bad_policy_file_fails(tmp_path, bad):
    (tmp_path / "geos.json").write_text(json.dumps(bad))
    assert run(tmp_path).returncode == 1, bad
