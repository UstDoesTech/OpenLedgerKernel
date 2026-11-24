"""K-23a: the edge tier and its way in from the internet (D-22), checked without a cloud account.

What matters here is what does not change. No address that exists moves, the private route table
stays private, nothing but the edge module can make an internet gateway, and Azure and Google Cloud
get nothing. OpenTofu itself (fmt, validate) runs in CI."""
import ipaddress
import json
import re
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
INFRA = ROOT / "infra"
PLAN = json.loads((INFRA / "address_plan.json").read_text())
LAYOUT = PLAN["layout"]
GEOS = json.loads((INFRA / "geos.json").read_text())


def read(*parts):
    text = (INFRA.joinpath(*parts)).read_text()
    return re.sub(r"(?m)^\s*#.*$", "", text)  # code only, so a comment cannot satisfy or break a test


def sub(prefix, number):
    net = ipaddress.ip_network(prefix)
    return list(net.subnets(prefixlen_diff=LAYOUT["subnet_newbits"]))[number]


def tier_subnets(cidr, index):
    return [sub(cidr, index * LAYOUT["tier_stride"] + z) for z in range(LAYOUT["zones"])]


def enabled_geos():
    return [g for g, v in PLAN["geos"].items() if g in GEOS and GEOS[g].get("enabled", True)] or list(PLAN["geos"])


def test_the_shared_tiers_are_the_three_they_were_and_edge_is_numbered_after_them():
    assert LAYOUT["tiers"] == ["cell", "control_plane", "api"]
    assert LAYOUT["aws_only_tiers"] == ["edge"]


def test_no_address_that_exists_moves():
    # Pinned as text on purpose: a change to any of these is a change to a running cell's network.
    expected = {
        ("europe", "aws"): {
            "cell": ["10.11.0.0/20", "10.11.16.0/20", "10.11.32.0/20"],
            "control_plane": ["10.11.64.0/20", "10.11.80.0/20", "10.11.96.0/20"],
            "api": ["10.11.128.0/20", "10.11.144.0/20", "10.11.160.0/20"],
        },
        ("europe", "azure"): {"api": ["10.21.128.0/20", "10.21.144.0/20", "10.21.160.0/20"]},
        ("europe", "gcp"): {"api": ["10.31.128.0/20", "10.31.144.0/20", "10.31.160.0/20"]},
    }
    for (geo, cloud), tiers in expected.items():
        cidr = PLAN["geos"][geo][cloud]
        for tier, want in tiers.items():
            got = [str(s) for s in tier_subnets(cidr, LAYOUT["tiers"].index(tier))]
            assert got == want, (geo, cloud, tier)


def test_the_edge_tier_is_three_subnets_after_the_api_tier_and_clear_of_everything():
    index = len(LAYOUT["tiers"])
    for geo, clouds in PLAN["geos"].items():
        vpc = ipaddress.ip_network(clouds["aws"])
        edge = tier_subnets(clouds["aws"], index)
        assert len(edge) == 3
        others = [s for i in range(index) for s in tier_subnets(clouds["aws"], i)]
        gateway = list(vpc.subnets(prefixlen_diff=LAYOUT["gateway_newbits"]))[LAYOUT["gateway_number"]]
        for s in edge:
            assert s.subnet_of(vpc), (geo, s)
            assert s.num_addresses >= 256, "a load balancer needs room in each zone"
            assert not any(s.overlaps(o) for o in others), (geo, s)
            assert not s.overlaps(gateway), (geo, s)
        assert len({str(s) for s in edge}) == 3
    assert [str(s) for s in tier_subnets(PLAN["geos"]["europe"]["aws"], index)] == [
        "10.11.192.0/20", "10.11.208.0/20", "10.11.224.0/20",
    ]


def test_only_aws_has_the_edge_tier():
    plan = read("modules", "address-plan", "main.tf")
    assert re.search(r'cloud == "aws" \? try\(local\.layout\.aws_only_tiers, \[\]\) : \[\]', plan)
    assert "tiers[cloud]" in plan
    for cloud in ("azure", "gcp"):
        for path in (INFRA / "modules" / f"network-{cloud}").glob("*.tf"):
            assert not re.search(r"\bedge\b", path.read_text()), path
    # Nothing the three clouds share is told to expect it.
    for path in (INFRA / "modules" / "interconnect").glob("*.tf"):
        assert '"edge"' not in path.read_text(), path


def test_the_interconnect_links_only_tiers_every_cloud_has():
    main = read("modules", "interconnect", "main.tf")
    assert "contains(keys(plan.subnets), tier)" in main
    assert "tiers = keys(var.plans.aws.subnets)" not in main


def test_the_private_route_table_leaves_the_edge_subnets_out_and_gets_no_route_to_the_internet():
    net = read("modules", "network-aws", "main.tf")
    assert re.search(r'if local\.subnets\[key\]\.tier != "edge"', net)
    assert 'resource "aws_route_table" "private"' in net
    for forbidden in ("aws_internet_gateway", "aws_route\"", "0.0.0.0/0", "aws_nat_gateway"):
        assert forbidden not in net, forbidden
    assert re.search(r"map_public_ip_on_launch\s*=\s*false", net)
    assert "map_public_ip_on_launch = true" not in net


def test_the_edge_tier_keeps_its_default_deny_group_and_this_task_adds_no_rule():
    net = read("modules", "network-aws", "main.tf")
    assert 'resource "aws_security_group" "tier"' in net
    assert "ingress" not in net and "egress" not in net
    edge = read("modules", "edge-aws", "main.tf")
    assert "security_group" not in edge


def test_the_edge_module_makes_one_gateway_one_route_and_puts_only_the_edge_subnets_on_it():
    main = read("modules", "edge-aws", "main.tf")
    assert main.count('resource "aws_internet_gateway"') == 1
    assert main.count('resource "aws_route_table" "edge"') == 1
    assert main.count('resource "aws_route"') == 1
    assert main.count("0.0.0.0/0") == 1
    assert re.search(r'route_table_id\s*=\s*aws_route_table\.edge\.id', main)
    # The subnets it associates are the ones it is given, never found by looking.
    assert "var.subnet_ids[count.index]" in main
    assert "data \"aws_subnet" not in main and "aws_subnet." not in main
    for forbidden in ("aws_nat_gateway", "aws_egress_only", "aws_eip", "::/0", "map_public_ip"):
        assert forbidden not in main, forbidden
    variables = read("modules", "edge-aws", "variables.tf")
    assert "length(var.subnet_ids) == 3" in variables
    for name in ("main.tf", "variables.tf", "outputs.tf", "versions.tf"):
        assert (INFRA / "modules" / "edge-aws" / name).is_file(), name


def test_no_other_module_or_layer_can_make_a_way_to_the_internet():
    allowed = INFRA / "modules" / "edge-aws" / "main.tf"
    pattern = re.compile(r'resource "(aws_internet_gateway|aws_egress_only_internet_gateway|aws_nat_gateway|aws_eip)"')
    for path in INFRA.rglob("*.tf"):
        if path == allowed or ".terraform" in path.parts:
            continue
        assert not pattern.search(read(*path.relative_to(INFRA).parts)), path


def test_the_api_layer_makes_the_edge_only_when_asked_and_gives_it_only_the_edge_subnets():
    main = read("envs", "europe", "api", "main.tf")
    block = main[main.index('module "edge_aws"'):]
    block = block[: block.index("\n}\n") + 3]
    assert "count  = var.edge_enabled ? 1 : 0" in block
    assert 'subnet_ids = local.network.aws.subnet_ids["edge"]' in block
    assert "local.network.aws.vpc_id" in block
    variables = read("envs", "europe", "api", "variables.tf")
    declared = variables[variables.index('variable "edge_enabled"'):]
    assert re.search(r"type\s*=\s*bool", declared) and re.search(r"default\s*=\s*false", declared)


def test_the_network_layer_hands_on_the_vpc_and_every_tier_including_edge():
    out = read("envs", "europe", "network", "outputs.tf")
    assert "vpc_id" in out and "module.network_aws.subnet_ids" in out
    outputs = read("modules", "network-aws", "outputs.tf")
    assert "keys(var.plan.subnets)" in outputs, "every tier in the plan is handed on, edge included"


@pytest.mark.parametrize("name", ["network-aws", "address-plan", "edge-aws", "interconnect"])
def test_the_changed_modules_still_have_their_standard_files(name):
    for f in ("main.tf", "variables.tf", "versions.tf"):
        if name == "address-plan" and f == "versions.tf":
            continue
        assert (INFRA / "modules" / name / f).exists() or name in ("address-plan", "interconnect"), (name, f)
