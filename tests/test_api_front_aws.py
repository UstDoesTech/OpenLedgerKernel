"""K-23b: the load balancer in front of the AWS API (D-22), checked without a cloud account.

OpenTofu's own checks (fmt, validate) run in CI. These hold the rules a plan would otherwise have to
be read for: HTTPS only, the health check, who may connect, what the groups allow, the rate limit,
and what the layer prints."""
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MODULE = ROOT / "infra" / "modules" / "api-front-aws"
LAYER = ROOT / "infra" / "envs" / "europe" / "api"


def code(path):
    return re.sub(r"(?m)^\s*#.*$", "", path.read_text())


def resource(text, kind, name):
    m = re.search(rf'resource "{kind}" "{name}" \{{', text)
    assert m, f"{kind}.{name} is not made"
    depth, i = 1, m.end()
    while depth:
        depth += {"{": 1, "}": -1}.get(text[i], 0)
        i += 1
    return text[m.start():i]


def variable(text, name):
    return resource(text.replace(f'variable "{name}"', f'resource "variable" "{name}"'), "variable", name)


MAIN = code(MODULE / "main.tf")
VARS = code(MODULE / "variables.tf")


def test_the_module_has_the_standard_files():
    for f in ("main.tf", "variables.tf", "outputs.tf", "versions.tf"):
        assert (MODULE / f).is_file(), f


def test_the_load_balancer_is_internet_facing_in_the_edge_subnets_and_takes_https_on_443_only():
    lb = resource(MAIN, "aws_lb", "this")
    assert 'load_balancer_type         = "application"' in lb
    assert "internal                   = false" in lb
    assert "subnets                    = var.edge_subnet_ids" in lb
    assert "security_groups            = [var.edge_security_group_id]" in lb
    assert "drop_invalid_header_fields = true" in lb
    listeners = re.findall(r'resource "aws_lb_listener" "(\w+)"', MAIN)
    assert listeners == ["https"], "there is no listener on port 80, and no other"
    listener = resource(MAIN, "aws_lb_listener", "https")
    assert re.search(r"port\s+= 443", listener) and re.search(r'protocol\s+= "HTTPS"', listener)
    assert not re.search(r"\b80\b", MAIN)
    assert not re.search(r'protocol\s+= "HTTP"', MAIN)
    # A current policy: TLS 1.3 with 1.2 as the floor.
    assert 'ssl_policy        = "ELBSecurityPolicy-TLS13-1-2-Res-2021-06"' in listener
    for old in ("ELBSecurityPolicy-2016", "TLS-1-0", "TLS-1-1", "FS-2018"):
        assert old not in MAIN


def test_the_certificate_is_from_acm_for_the_given_name_and_the_listener_waits_for_it_to_be_issued():
    cert = resource(MAIN, "aws_acm_certificate", "this")
    assert "domain_name       = var.domain_name" in cert and 'validation_method = "DNS"' in cert
    assert "create_before_destroy = true" in cert
    validation = resource(MAIN, "aws_acm_certificate_validation", "this")
    assert "certificate_arn = aws_acm_certificate.this.arn" in validation
    listener = resource(MAIN, "aws_lb_listener", "https")
    assert "certificate_arn   = aws_acm_certificate_validation.this[0].certificate_arn" in listener


def test_the_name_is_an_input_with_no_default_and_is_not_in_the_repository():
    name = variable(VARS, "domain_name")
    assert "default" not in name
    assert "can(regex(" in name and "wildcard" in name
    layer = code(LAYER / "variables.tf")
    assert 'default     = ""' in variable(layer, "front_domain_name")


def test_it_forwards_to_the_api_port_over_https_and_checks_healthz_so_a_cell_not_ready_takes_no_traffic():
    group = resource(MAIN, "aws_lb_target_group", "api")
    assert 'protocol         = "HTTPS"' in group and "port             = var.api_port" in group
    assert 'target_type      = "instance"' in group
    health = group[group.index("health_check"):]
    assert 'protocol            = "HTTPS"' in health and 'path                = "/healthz"' in health
    assert 'matcher             = "200"' in health
    action = resource(MAIN, "aws_lb_listener", "https")
    assert 'type             = "forward"' in action
    attach = resource(MAIN, "aws_lb_target_group_attachment", "api")
    assert "target_id        = var.target_instance_id" in attach and "port             = var.api_port" in attach
    assert re.search(r"api_port\"?\s*\{[^}]*default\s+= 8443", VARS + "") or "8443" in VARS
    layer = code(LAYER / "main.tf")
    assert "target_instance_id     = module.api_aws.instance_id" in layer
    assert "api_port               = local.client_port" in layer


def test_it_takes_connections_only_from_the_listed_ranges_and_none_means_no_rule():
    clients = resource(MAIN, "aws_vpc_security_group_ingress_rule", "clients")
    assert "for_each = var.certificate_validated ? toset(var.edge_client_cidrs) : toset([])" in clients
    assert "cidr_ipv4         = each.value" in clients
    assert re.search(r"from_port\s+= 443", clients) and re.search(r"to_port\s+= 443", clients)
    cidrs = variable(VARS, "edge_client_cidrs")
    assert re.search(r"default\s+= \[\]", cidrs)


def test_the_whole_internet_is_refused_as_a_range_and_only_the_deliberate_switch_opens_it():
    cidrs = variable(VARS, "edge_client_cidrs")
    assert "validation" in cidrs and "!strcontains(c, \":\")" in cidrs, "::/0 and every IPv6 range are refused"
    assert 'tonumber(split("/", c)[1]) >= 8' in cidrs, "0.0.0.0/0 and any range wider than /8 are refused"
    assert "can(cidrhost(c, 0))" in cidrs
    # The range is written once, in the rule the switch makes, and nowhere else in the module.
    assert MAIN.count("0.0.0.0/0") == 1 and "::/0" not in MAIN
    openrule = resource(MAIN, "aws_vpc_security_group_ingress_rule", "open")
    assert "count = var.certificate_validated && var.open_to_internet ? 1 : 0" in openrule
    assert re.search(r"default\s+= false", variable(VARS, "open_to_internet"))
    assert re.search(r"default\s+= false", variable(code(LAYER / "variables.tf"), "open_to_internet"))
    # The layer refuses the range too, by passing the list through the module's validation.
    assert "edge_client_cidrs      = var.edge_client_cidrs" in code(LAYER / "main.tf")


def test_when_the_whole_internet_is_taken_the_service_is_told_which_ranges_are_the_load_balancers():
    layer = code(LAYER / "main.tf")
    assert 'trusted_proxy_cidrs       = local.plan.aws.subnets["edge"]' in layer
    boot = code(ROOT / "infra" / "modules" / "api-boot" / "main.tf")
    assert 'LEDGER_TRUSTED_PROXIES = join(",", var.trusted_proxy_cidrs)' in boot
    assert "merge(local.environment, local.proxy_environment)" in boot
    # Only the AWS machine has a load balancer, so only it is told.
    for cloud in ("azure", "gcp"):
        assert "trusted_proxy_cidrs" not in code(ROOT / "infra" / "modules" / "api" / cloud / "main.tf")
        assert "trusted_proxy_cidrs" not in layer.split(f'module "api_{cloud}"')[1].split("\nmodule")[0]


def test_the_load_balancers_egress_is_the_api_port_to_the_api_group_and_nothing_else():
    egress = re.findall(r'resource "aws_vpc_security_group_egress_rule" "(\w+)"', MAIN)
    assert egress == ["to_api"]
    rule = resource(MAIN, "aws_vpc_security_group_egress_rule", "to_api")
    assert "security_group_id            = var.edge_security_group_id" in rule
    assert "referenced_security_group_id = var.api_security_group_id" in rule
    assert "from_port                    = var.api_port" in rule and "to_port                      = var.api_port" in rule
    assert "cidr_ipv4" not in rule and "cidr_ipv6" not in rule
    assert "aws_security_group_rule" not in MAIN and 'resource "aws_security_group"' not in MAIN


def test_the_api_group_takes_the_api_port_from_the_load_balancers_group_for_this_path():
    rule = resource(MAIN, "aws_vpc_security_group_ingress_rule", "from_load_balancer")
    assert "security_group_id            = var.api_security_group_id" in rule
    assert "referenced_security_group_id = var.edge_security_group_id" in rule
    assert "from_port                    = var.api_port" in rule and "cidr_ipv4" not in rule
    layer = code(LAYER / "main.tf")
    assert 'edge_security_group_id = local.network.aws.security_group_ids["edge"]' in layer
    assert 'api_security_group_id  = local.network.aws.security_group_ids["api"]' in layer
    assert 'edge_subnet_ids        = local.network.aws.subnet_ids["edge"]' in layer


def test_a_web_acl_with_a_rate_limit_for_each_address_is_attached():
    acl = resource(MAIN, "aws_wafv2_web_acl", "this")
    assert 'scope = "REGIONAL"' in acl
    assert "rate_based_statement" in acl and 'aggregate_key_type    = "IP"' in acl
    assert "limit                 = var.rate_limit" in acl
    assert "block {}" in acl and "allow {}" in acl
    assoc = resource(MAIN, "aws_wafv2_web_acl_association", "this")
    assert "resource_arn = aws_lb.this[0].arn" in assoc and "web_acl_arn  = aws_wafv2_web_acl.this[0].arn" in assoc


def test_there_is_never_an_internet_facing_load_balancer_without_the_gateway_made_for_it():
    lb = resource(MAIN, "aws_lb", "this")
    assert 'var.internet_gateway_id != ""' in lb
    layer = code(LAYER / "main.tf")
    assert 'internet_gateway_id    = try(module.edge_aws[0].internet_gateway_id, "")' in layer
    # The module makes no gateway or route itself: that stays in the edge module (K-23a).
    for kind in ("aws_internet_gateway", "aws_route", "aws_route_table", "aws_eip", "aws_nat_gateway"):
        assert f'resource "{kind}"' not in MAIN


def test_the_load_balancer_is_made_in_two_steps_so_the_name_can_be_proved_first():
    assert re.search(r"default\s+= false", variable(VARS, "certificate_validated"))
    live = [m for m in re.findall(r"count\s+= local\.live", MAIN)]
    assert len(live) >= 7
    for kind, name in (("aws_lb", "this"), ("aws_lb_listener", "https"), ("aws_wafv2_web_acl", "this")):
        assert "count = local.live" in resource(MAIN, kind, name)


def test_the_layer_prints_the_dns_record_and_no_account_identifier():
    outputs = code(LAYER / "outputs.tf")
    front = resource(outputs.replace('output "front"', 'resource "output" "front"'), "output", "front")
    assert "certificate_validation_records" in front and "load_balancer_dns_name" in front
    module_outputs = code(MODULE / "outputs.tf")
    assert set(re.findall(r'^output "(\w+)"', module_outputs, re.M)) == {
        "certificate_validation_records", "load_balancer_dns_name", "web_acl_attached",
    }
    records = resource(module_outputs.replace('output "certificate_validation_records"', 'resource "output" "r"'), "output", "r")
    assert "resource_record_name" in records and "resource_record_value" in records
    # Nothing that carries an account number: no ARN, no account id, no caller identity.
    for text in (front, module_outputs):
        assert not re.search(r"\.arn\b|account_id|caller_identity|owner_id", text)
    assert not re.search(r"\b\d{12}\b", MAIN + VARS + module_outputs)
    # The optional output is empty, not an error, when there is no name.
    assert "length(module.front_aws) == 0 ? null" in front


def test_the_layer_makes_the_load_balancer_only_when_given_a_name():
    layer = code(LAYER / "main.tf")
    block = layer[layer.index('module "front_aws"'):]
    block = block[: block.index("\n}\n")]
    assert 'count  = var.front_domain_name != "" ? 1 : 0' in block
    assert "certificate_validated  = var.certificate_validated" in block


def test_nothing_secret_or_account_specific_is_written_in_the_module():
    for path in MODULE.glob("*.tf"):
        text = code(path)
        assert not re.search(r"arn:aws", text), path
        assert not re.search(r"(?i)password|secret_key|access_key", text), path
