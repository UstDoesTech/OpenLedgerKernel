"""I-01c: the private links between the clouds and the rules across them.

OpenTofu runs in CI. These tests cover what it cannot: the link numbering, the flow policy, and
that the module has no way to leak a key, weaken the encryption or open a port by hand.
"""
import ipaddress
import json
import re
from itertools import combinations
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
INFRA = ROOT / "infra"
MODULE = INFRA / "modules" / "interconnect"
LINKS = json.loads((INFRA / "links.json").read_text())
FLOWS = json.loads((INFRA / "flows.json").read_text())
PLAN = json.loads((INFRA / "address_plan.json").read_text())
TIERS = PLAN["layout"]["tiers"]
PAIRS = {"aws-azure", "aws-gcp", "azure-gcp"}


def code(name: str) -> str:
    path = MODULE / name
    return "\n".join(l for l in path.read_text().splitlines() if not l.lstrip().startswith("#"))


def all_code() -> str:
    return "\n".join(code(p.name) for p in sorted(MODULE.glob("*.tf")))


# ---- links.json: BGP numbers and tunnel addressing ------------------------------------------


def test_every_pair_of_networks_is_joined_by_at_least_two_tunnels():
    assert set(LINKS["tunnels"]) == PAIRS
    for pair, cidrs in LINKS["tunnels"].items():
        assert len(cidrs) >= 2, f"{pair} would depend on one tunnel"
    # The module's shape: one AWS connection carries two tunnels, Azure has one gateway address
    # and Google two interfaces, so these counts are fixed by the vendors.
    assert {p: len(c) for p, c in LINKS["tunnels"].items()} == {"aws-azure": 2, "aws-gcp": 4, "azure-gcp": 2}


def test_asns_are_private_distinct_and_not_reserved_by_azure():
    asns = LINKS["asn"]
    assert set(asns) == {"aws", "azure", "gcp"}
    assert len(set(asns.values())) == 3
    for cloud, asn in asns.items():
        assert 64512 <= asn <= 65534, f"{cloud} must use a private ASN"
        assert not 65515 <= asn <= 65520, f"{cloud}: Azure reserves 65515 to 65520"


def test_tunnel_ranges_are_distinct_slash_30s_in_link_local_space():
    link_local = ipaddress.ip_network("169.254.0.0/16")
    aws_reserved = [ipaddress.ip_network(f"169.254.{i}.0/30") for i in range(6)] + [ipaddress.ip_network("169.254.169.252/30")]
    every = []
    for pair, cidrs in LINKS["tunnels"].items():
        for c in cidrs:
            n = ipaddress.ip_network(c)
            assert n.prefixlen == 30 and n.subnet_of(link_local), c
            assert not any(n.overlaps(r) for r in aws_reserved), f"{c} is reserved by AWS"
            every.append((pair, n))
    for (_, a), (_, b) in combinations(every, 2):
        assert not a.overlaps(b), f"{a} overlaps {b}"


def test_tunnels_that_end_in_azure_use_the_addresses_azure_accepts_for_bgp():
    azure_range = ipaddress.ip_network("169.254.21.0/24"), ipaddress.ip_network("169.254.22.0/24")
    for pair in ("aws-azure", "azure-gcp"):
        for c in LINKS["tunnels"][pair]:
            n = ipaddress.ip_network(c)
            assert any(n.subnet_of(r) for r in azure_range), f"{pair} {c} is outside Azure's APIPA range"


# ---- flows.json: the only traffic allowed between tiers --------------------------------------


def test_the_flows_use_only_the_platform_ports_and_tcp():
    assert FLOWS["protocol"] == "tcp"
    ports = FLOWS["ports"]
    assert {k: v for k, v in ports.items() if k != "relay"} == {"postgres": 5432, "etcd_client": 2379, "etcd_peer": 2380}
    assert 1024 <= ports["relay"] <= 65535, "the relay port is a proposed default until R-01 fixes it"
    assert len(set(ports.values())) == len(ports)


def test_every_flow_joins_known_tiers_on_a_known_port_and_every_port_is_used():
    names = [f["name"] for f in FLOWS["flows"]]
    assert len(names) == len(set(names))
    used = set()
    for f in FLOWS["flows"]:
        assert set(f) == {"name", "source", "destination", "port"}
        assert f["source"] in TIERS and f["destination"] in TIERS, f
        assert f["port"] in FLOWS["ports"], f
        assert re.fullmatch(r"[a-z]+(-[a-z]+)*", f["name"]), "names become rule names"
        used.add(f["port"])
    assert used == set(FLOWS["ports"]), "a port nothing uses should not be listed"
    assert len(FLOWS["flows"]) < 100, "rule priorities are 200 plus the flow's number"


def allowed(source: str, destination: str):
    return {FLOWS["ports"][f["port"]] for f in FLOWS["flows"] if (f["source"], f["destination"]) == (source, destination)}


def test_the_allowed_matrix_is_what_the_platform_needs_and_nothing_else():
    assert allowed("api", "cell") == {5432, 8443}
    assert allowed("cell", "cell") == {5432, 8443}
    assert allowed("api", "control_plane") == {2379}
    assert allowed("cell", "control_plane") == {2379}
    assert allowed("control_plane", "control_plane") == {2380}
    # Denied by omission: the control plane never initiates to cells or the API, nothing
    # reaches the API tier, and the API never talks to itself.
    for pair in (("control_plane", "cell"), ("control_plane", "api"), ("cell", "api"), ("api", "api")):
        assert allowed(*pair) == set(), pair


# ---- the module ------------------------------------------------------------------------------


def test_the_module_has_the_standard_files_and_all_three_providers_pinned():
    for name in ("main.tf", "variables.tf", "outputs.tf", "versions.tf", "aws.tf", "azure.tf", "gcp.tf", "rules.tf"):
        assert (MODULE / name).is_file(), name
    versions = code("versions.tf")
    pins = json.loads((INFRA / "providers.json").read_text())
    for name in ("aws", "azurerm", "google"):  # I-01d added databricks, which only Postgres uses
        pin = pins[name]
        assert pin["source"] in versions and pin["version"] in versions, name


def test_the_mechanism_is_a_setting_that_defaults_to_ipsec_vpn():
    block = re.search(r'variable "mechanism" \{(.*?)\n\}', code("variables.tf"), re.S).group(1)
    assert re.search(r'default\s*=\s*"ipsec_vpn"', block)
    assert "validation" in block
    assert "var.mechanism" in code("main.tf") and "var.mechanism" in code("outputs.tf")


def test_every_tunnel_runs_bgp_over_ikev2_with_a_strong_suite():
    text = all_code()
    assert re.search(r"static_routes_only\s*=\s*false", text)
    assert text.count("bgp_enabled") >= 3 and not re.search(r"bgp_enabled\s*=\s*false", text)
    assert "enable_bgp" not in text, "deprecated in azurerm 4 and removed in 5"
    assert 'ike_versions    = ["ikev2"]' in code("main.tf")
    assert re.search(r'ike_version\s*=\s*2', code("gcp.tf"))
    assert 'connection_protocol' in code("azure.tf") and '"IKEv2"' in code("azure.tf")
    main, azure = code("main.tf"), code("azure.tf")
    assert 'encryption      = ["AES256"]' in main and 'integrity       = ["SHA2-256"]' in main and "dh_groups       = [14]" in main
    for want in ('"DHGroup14"', '"AES256"', '"SHA256"', '"PFS14"'):
        assert want in azure, want
    weak = r"\b(3DES|DES3|DES|MD5|SHA1|ikev1|IKEv1|DHGroup1|DHGroup2|PFS1|PFS2)\b"
    assert not re.search(weak, text), "weak or deprecated IPsec setting"


def test_pre_shared_keys_are_a_sensitive_input_with_no_default_and_never_written_down():
    block = re.search(r'variable "preshared_keys" \{(.*?)\n\}', code("variables.tf"), re.S).group(1)
    assert re.search(r"sensitive\s*=\s*true", block)
    assert not re.search(r"\bdefault\b", block), "a default key would live in the repository"
    text = all_code()
    for attribute in ("tunnel1_preshared_key", "tunnel2_preshared_key", "shared_key", "shared_secret"):
        for line in re.findall(rf"^\s*{attribute}\s*=\s*(.*)$", text, re.M):
            assert line.strip().startswith("var.preshared_keys["), f"{attribute} must come from the preshared_keys input"
    assert not re.search(r'(?i)(preshared_key|shared_key|shared_secret|password)\w*\s*=\s*"', text)
    assert re.search(r'output "required_preshared_key_names"', code("outputs.tf"))
    assert 'resource "terraform_data" "preshared_key_check"' in code("main.tf")


def test_the_module_creates_the_tunnels_each_pair_needs():
    aws, azure, gcp = code("aws.tf"), code("azure.tf"), code("gcp.tf")
    # AWS and Azure: one connection of two tunnels; AWS and Google: two connections of two.
    assert re.search(r'resource "aws_vpn_connection" "azure"', aws)
    assert re.search(r'resource "aws_vpn_connection" "gcp" \{\s*count\s*=\s*2', aws)
    assert re.search(r'resource "google_compute_vpn_tunnel" "aws" \{\s*count\s*=\s*4', gcp)
    assert re.search(r'resource "google_compute_vpn_tunnel" "azure" \{\s*count\s*=\s*2', gcp)
    assert re.search(r'resource "azurerm_virtual_network_gateway_connection" "aws" \{\s*for_each\s*=\s*local\.azure_aws_tunnels', azure)
    assert re.search(r'resource "azurerm_virtual_network_gateway_connection" "gcp" \{\s*for_each\s*=\s*local\.azure_gcp_tunnels', azure)
    assert "length(azurerm_virtual_network_gateway_connection.aws)" in code("outputs.tf")
    assert "length(google_compute_vpn_tunnel.aws)" in code("outputs.tf") and "length(google_compute_vpn_tunnel.azure)" in code("outputs.tf")
    assert "aws_vpn_gateway_route_propagation" in aws, "AWS must learn the other clouds' routes"


def test_the_vpn_gateway_is_the_only_public_address_and_workloads_get_none():
    text = all_code()
    assert text.count('resource "azurerm_public_ip"') == 1
    for pattern in (r"\baws_eip\b", r"\bgoogle_compute_address\b", r"\baccess_config\b", r"\baws_internet_gateway\b", r"\baws_nat_gateway\b"):
        assert not re.search(pattern, text), pattern
    assert not re.search(r"0\.0\.0\.0/0|::/0", text)


def test_rules_come_from_the_flow_file_and_no_port_is_written_by_hand():
    rules = code("rules.tf")
    assert not re.search(r"\b(5432|2379|2380|8443)\b", all_code()), "ports live in flows.json"
    assert not re.search(r"port\w*\s*=\s*\"?\d", rules), "a literal port"
    assert not re.search(r"\b(from|to)_port\s*=\s*(0|-1|65535)\b", rules)
    # Every allow rule is built from a flow.
    for resource in ("azurerm_network_security_rule\" \"allow_in", "azurerm_network_security_rule\" \"allow_out", "google_compute_firewall\" \"allow_in", "google_compute_firewall\" \"allow_out"):
        assert re.search(rf'resource "{re.escape(resource)}" \{{\s*for_each\s*=\s*local\.flows', rules), resource
    assert re.search(r'resource "aws_vpc_security_group_ingress_rule" "flow" \{\s*for_each\s*=\s*local\.aws_ingress', rules)
    assert re.search(r'resource "aws_vpc_security_group_egress_rule" "flow" \{\s*for_each\s*=\s*local\.aws_egress', rules)
    assert re.search(r"ip_protocol\s*=\s*local\.flow_file\.protocol", rules)
    assert 'protocol                     = "Tcp"' in rules


def test_nothing_is_allowed_beyond_the_flows():
    rules = code("rules.tf")
    allows = len(re.findall(r'access\s*=\s*"Allow"', rules)) + len(re.findall(r"\ballow\s*\{", rules))
    assert allows == 4, "Azure in and out, Google in and out; AWS rules are all flow rules"
    assert not re.search(r'protocol\s*=\s*"(\*|all|-1)"', rules.split('"deny_out"')[0]), "no allow-all rule"
    assert re.search(r'resource "google_compute_firewall" "deny_out"', rules) and re.search(r'priority\s*=\s*65000', rules)
    assert "local.flows" in rules and rules.count("allow-${each.key}") >= 4


def test_allow_rules_sit_below_the_baseline_deny_in_priority():
    # Azure: baseline denies are at 4096, flow rules are 200 plus the flow number.
    azure_net = (INFRA / "modules" / "network-azure" / "main.tf").read_text()
    assert "priority = 4096" in azure_net
    assert 200 + len(FLOWS["flows"]) < 4096
    assert re.search(r"priority\s*=\s*200 \+ each\.value\.index", code("rules.tf"))
    # Google: flow rules at 1000 beat the deny at 65000 (lower number wins).
    assert re.search(r"priority\s*=\s*1000", code("rules.tf"))


def test_the_azure_gateway_has_the_apipa_addresses_its_connections_use():
    azure = code("azure.tf")
    assert 'name                 = "GatewaySubnet"' in azure
    assert "apipa_addresses       = local.azure_apipa_addresses" in azure
    assert azure.count("custom_bgp_addresses") >= 2
    assert azure.count("cidrhost(tunnel.inside, 2)") == 2 and azure.count("cidrhost(each.value.inside, 2)") == 2
