"""I-01b: the address plan and the three network modules, checked without a cloud account.

OpenTofu itself (fmt, validate, tflint) runs in CI. These tests cover what that cannot: that the
plan's ranges never overlap, and that the modules have no way to be reached from the internet.
"""
import ipaddress
import json
import re
from itertools import combinations
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
INFRA = ROOT / "infra"
MODULES = INFRA / "modules"
PLAN = json.loads((INFRA / "address_plan.json").read_text())
GEOS = json.loads((INFRA / "geos.json").read_text())
LAYOUT = PLAN["layout"]
CLOUDS = ("aws", "azure", "gcp")
NETWORK_MODULES = ("network-aws", "network-azure", "network-gcp")


def cidrsubnet(prefix: str, newbits: int, number: int):
    """OpenTofu's cidrsubnet(), so the modules' layout can be checked here."""
    return list(ipaddress.ip_network(prefix).subnets(prefixlen_diff=newbits))[number]


def gateway_for(cidr: str):
    return cidrsubnet(cidr, LAYOUT["gateway_newbits"], LAYOUT["gateway_number"])


def layout_for(cidr: str):
    return {
        tier: [cidrsubnet(cidr, LAYOUT["subnet_newbits"], i * LAYOUT["tier_stride"] + z) for z in range(LAYOUT["zones"])]
        for i, tier in enumerate(LAYOUT["tiers"])
    }


def code(path: Path) -> str:
    return "\n".join(l for l in path.read_text().splitlines() if not l.lstrip().startswith("#"))


# ---- the shared address plan ---------------------------------------------------------------


def test_the_layout_is_three_tiers_of_three_zones_and_fits_in_each_network():
    assert LAYOUT["tiers"] == ["cell", "control_plane", "api"]
    assert LAYOUT["zones"] == 3
    assert LAYOUT["tier_stride"] >= LAYOUT["zones"], "tiers would share subnets"
    assert len(LAYOUT["tiers"]) * LAYOUT["tier_stride"] <= 2 ** LAYOUT["subnet_newbits"], "layout does not fit"


def test_every_enabled_geo_has_a_plan_for_all_three_clouds_and_no_unknown_geo_has_one():
    assert set(PLAN["geos"]) <= set(GEOS["geos"])
    for geo in GEOS["enabled"]:
        assert set(PLAN["geos"][geo]) == set(CLOUDS), geo
    for geo, clouds in PLAN["geos"].items():
        assert set(clouds) == set(CLOUDS), geo


def all_networks():
    return [(geo, cloud, ipaddress.ip_network(cidr)) for geo, clouds in PLAN["geos"].items() for cloud, cidr in clouds.items()]


def test_networks_are_private_ipv4_and_never_overlap_across_clouds_or_geos():
    nets = all_networks()
    assert nets
    for geo, cloud, net in nets:
        assert net.version == 4 and net.is_private, (geo, cloud)
    for (g1, c1, n1), (g2, c2, n2) in combinations(nets, 2):
        assert not n1.overlaps(n2), f"{g1}/{c1} {n1} overlaps {g2}/{c2} {n2}"


def test_subnets_sit_inside_their_network_and_never_overlap():
    every = []
    for geo, cloud, net in all_networks():
        tiers = layout_for(str(net))
        assert list(tiers) == LAYOUT["tiers"]
        for tier, subnets in tiers.items():
            assert len(subnets) == 3, (geo, cloud, tier)
            for s in subnets:
                assert s.subnet_of(net), f"{geo}/{cloud}/{tier}: {s} is outside {net}"
                every.append((f"{geo}/{cloud}/{tier}", s))
    for (a, s1), (b, s2) in combinations(every, 2):
        assert not s1.overlaps(s2), f"{a} {s1} overlaps {b} {s2}"


def test_each_network_has_a_gateway_subnet_that_no_tier_uses():
    assert 0 <= LAYOUT["gateway_number"] < 2 ** LAYOUT["gateway_newbits"]
    for geo, cloud, net in all_networks():
        gateway = gateway_for(str(net))
        assert gateway.subnet_of(net), (geo, cloud)
        assert gateway.num_addresses >= 32, "Azure needs at least a /27 for GatewaySubnet"
        for tier, subnets in layout_for(str(net)).items():
            for s in subnets:
                assert not gateway.overlaps(s), f"{geo}/{cloud}: gateway {gateway} overlaps {tier} {s}"


def test_subnets_are_big_enough_for_cloud_reserved_addresses_and_growth():
    for _, _, net in all_networks():
        for subnets in layout_for(str(net)).values():
            for s in subnets:
                assert s.num_addresses >= 1024, s  # Azure and AWS reserve five addresses each


# ---- the modules ---------------------------------------------------------------------------


@pytest.mark.parametrize("module", ("address-plan",) + NETWORK_MODULES)
def test_modules_have_the_standard_files(module):
    for name in ("main.tf", "variables.tf", "outputs.tf", "versions.tf"):
        assert (MODULES / module / name).is_file(), f"{module}/{name}"


@pytest.mark.parametrize("module", NETWORK_MODULES)
def test_modules_take_ranges_from_the_plan_and_never_write_one(module):
    cidr = re.compile(r"\b\d{1,3}(\.\d{1,3}){3}/\d{1,2}\b")
    for tf in (MODULES / module).glob("*.tf"):
        assert not cidr.search(code(tf)), f"{tf.name} hard-codes an address range; take it from the plan"
    assert 'variable "plan"' in code(MODULES / module / "variables.tf")


@pytest.mark.parametrize("module", NETWORK_MODULES)
def test_modules_insist_on_all_three_tiers_with_three_subnets_each(module):
    variables = code(MODULES / module / "variables.tf")
    for tier in LAYOUT["tiers"]:
        assert f'"{tier}"' in variables, f"{module} must validate the {tier} tier"
    assert "== 3" in variables


def test_the_address_plan_module_reads_the_shared_file_and_the_same_layout():
    main = code(MODULES / "address-plan" / "main.tf")
    assert "address_plan.json" in main
    for key in ("tiers", "zones", "subnet_newbits", "tier_stride"):
        assert f"local.layout.{key}" in main, key
    assert "cidrsubnet(cidr, local.layout.subnet_newbits, tier_index * local.layout.tier_stride + zone)" in main


# Nothing in a network module may open a way in from the internet or give a way out to it.
FORBIDDEN = [
    r"0\.0\.0\.0/0",
    r"::/0",
    r"\baws_internet_gateway\b",
    r"\baws_egress_only_internet_gateway\b",
    r"\baws_nat_gateway\b",
    r"\baws_eip\b",
    r"\baws_security_group_rule\b",
    r"\baws_vpc_security_group_(ingress|egress)_rule\b",
    r"\bingress\s*\{",
    r"\begress\s*\{",
    r"\bazurerm_public_ip\b",
    r"\bazurerm_nat_gateway\b",
    r"\bazurerm_firewall\b",
    r"\bgoogle_compute_address\b",
    r"\bgoogle_compute_router_nat\b",
    r"\bgoogle_compute_global_address\b",
    r"\baccess_config\b",
    r"map_public_ip_on_launch\s*=\s*true",
    r"default_outbound_access_enabled\s*=\s*true",
    r'access\s*=\s*"Allow"',
]


@pytest.mark.parametrize("module", NETWORK_MODULES)
def test_no_module_opens_a_path_to_or_from_the_internet(module):
    text = "\n".join(code(tf) for tf in (MODULES / module).glob("*.tf"))
    for pattern in FORBIDDEN:
        assert not re.search(pattern, text), f"{module}: {pattern}"


def test_aws_subnets_have_no_public_addresses_and_use_three_zones():
    main = code(MODULES / "network-aws" / "main.tf")
    assert re.search(r"map_public_ip_on_launch\s*=\s*false", main)
    assert "slice(data.aws_availability_zones.available.names, 0, 3)" in main
    assert re.search(r'resource "aws_default_security_group"', main), "the default group must be emptied"
    assert re.search(r'resource "aws_security_group" "tier"', main)


def test_azure_subnets_have_no_default_outbound_access_and_every_tier_is_denied_by_default():
    main = code(MODULES / "network-azure" / "main.tf")
    assert re.search(r"default_outbound_access_enabled\s*=\s*false", main)
    for rule in ("deny-internet-inbound", "deny-internet-outbound", "deny-all-inbound", "deny-all-outbound"):
        assert f'"{rule}"' in main, rule
    # Azure overwrites a group's rules with its inline ones, which would wipe the allow rules
    # that modules/interconnect adds as separate resources. So no rule may be inline.
    assert not re.search(r"\bsecurity_rule\s*\{", main), "use azurerm_network_security_rule, not inline rules"
    assert 'resource "azurerm_network_security_rule" "baseline"' in main
    assert "azurerm_subnet_network_security_group_association" in main


def test_gcp_has_no_default_internet_route_and_private_google_access():
    main = code(MODULES / "network-gcp" / "main.tf")
    assert re.search(r"delete_default_routes_on_create\s*=\s*true", main)
    assert re.search(r"auto_create_subnetworks\s*=\s*false", main)
    assert re.search(r"private_ip_google_access\s*=\s*true", main)


def test_flow_logging_is_on_in_every_cloud():
    aws = code(MODULES / "network-aws" / "main.tf")
    assert re.search(r'resource "aws_flow_log" "this"', aws) and re.search(r'traffic_type\s*=\s*"ALL"', aws)
    azure = code(MODULES / "network-azure" / "main.tf")
    assert re.search(r'resource "azurerm_network_watcher_flow_log" "this"', azure)
    assert re.search(r"enabled\s*=\s*true", azure) and "target_resource_id" in azure
    gcp = code(MODULES / "network-gcp" / "main.tf")
    assert "log_config" in gcp and re.search(r"flow_sampling\s*=\s*1\.0", gcp)


def test_flow_log_storage_is_private_and_stays_in_the_region():
    azure = code(MODULES / "network-azure" / "main.tf")
    assert re.search(r'account_replication_type\s*=\s*"ZRS"', azure), "no geo-replication: data stays in the region"
    assert re.search(r"allow_nested_items_to_be_public\s*=\s*false", azure)
    assert re.search(r'default_action\s*=\s*"Deny"', azure)
    assert re.search(r'min_tls_version\s*=\s*"TLS1_2"', azure)


def test_tflint_loads_a_pinned_ruleset_for_each_cloud():
    text = code(INFRA / ".tflint.hcl")
    for name in ("aws", "azurerm", "google"):
        block = re.search(rf'plugin "{name}" \{{([^}}]*)\}}', text)
        assert block, name
        assert re.search(r'version\s*=\s*"\d+\.\d+\.\d+"', block.group(1)), f"{name} ruleset must be pinned"


def test_gcp_subnet_names_never_contain_an_underscore():
    # A tier is "control_plane", but Google names allow only letters, digits and hyphens
    # (^[a-z]([-a-z0-9]*[a-z0-9])?$). Found by the first plan against a real Google project.
    text_ = code(MODULES / "network-gcp" / "main.tf")
    assert 'name                     = "${var.name}-${replace(each.key, "_", "-")}"' in text_
