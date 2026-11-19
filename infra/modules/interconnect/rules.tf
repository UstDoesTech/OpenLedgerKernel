# Only the flows in ../../flows.json are allowed between tiers, on the listed port. Everything
# else stays denied by the default-deny rules each network module creates. Every rule is built
# from that file, so no port is written by hand here.
#
# AWS and Azure restrict both ends: the destination tier accepts the flow from the source
# tier's subnets, and the source tier may send it only to the destination tier's subnets.
# Google cannot restrict an egress rule by source subnet, so there the destination tier
# accepts the flow from the source tier's subnets and an egress rule limits where it may go.
locals {
  aws_ingress = merge([
    for name, flow in local.flows : {
      for cidr in local.tier_cidrs[flow.source] : "${name}/${cidr}" => { group = var.aws.security_group_ids[flow.destination], cidr = cidr, port = flow.port, flow = name }
    }
  ]...)

  aws_egress = merge([
    for name, flow in local.flows : {
      for cidr in local.tier_cidrs[flow.destination] : "${name}/${cidr}" => { group = var.aws.security_group_ids[flow.source], cidr = cidr, port = flow.port, flow = name }
    }
  ]...)
}

resource "aws_vpc_security_group_ingress_rule" "flow" {
  for_each = local.aws_ingress

  security_group_id = each.value.group
  cidr_ipv4         = each.value.cidr
  ip_protocol       = local.flow_file.protocol
  from_port         = each.value.port
  to_port           = each.value.port
  description       = "Allow ${each.value.flow}"
}

resource "aws_vpc_security_group_egress_rule" "flow" {
  for_each = local.aws_egress

  security_group_id = each.value.group
  cidr_ipv4         = each.value.cidr
  ip_protocol       = local.flow_file.protocol
  from_port         = each.value.port
  to_port           = each.value.port
  description       = "Allow ${each.value.flow}"
}

resource "azurerm_network_security_rule" "allow_in" {
  for_each = local.flows

  name                         = "allow-${each.key}-in"
  resource_group_name          = var.azure.resource_group_name
  network_security_group_name  = var.azure.network_security_group_names[each.value.destination]
  priority                     = 200 + each.value.index
  direction                    = "Inbound"
  access                       = "Allow"
  protocol                     = "Tcp"
  source_port_range            = "*"
  destination_port_range       = tostring(each.value.port)
  source_address_prefixes      = local.tier_cidrs[each.value.source]
  destination_address_prefixes = var.plans.azure.subnets[each.value.destination]
}

resource "azurerm_network_security_rule" "allow_out" {
  for_each = local.flows

  name                         = "allow-${each.key}-out"
  resource_group_name          = var.azure.resource_group_name
  network_security_group_name  = var.azure.network_security_group_names[each.value.source]
  priority                     = 200 + each.value.index
  direction                    = "Outbound"
  access                       = "Allow"
  protocol                     = "Tcp"
  source_port_range            = "*"
  destination_port_range       = tostring(each.value.port)
  source_address_prefixes      = var.plans.azure.subnets[each.value.source]
  destination_address_prefixes = local.tier_cidrs[each.value.destination]
}

# Google denies all ingress unless a rule allows it. Egress is open by default, so this denies
# egress to every platform range at a low priority and the rules above it open the flows.
resource "google_compute_firewall" "allow_in" {
  for_each = local.flows

  name               = "${var.name}-allow-${each.key}-in"
  network            = var.gcp.network_name
  direction          = "INGRESS"
  priority           = 1000
  source_ranges      = local.tier_cidrs[each.value.source]
  destination_ranges = var.plans.gcp.subnets[each.value.destination]

  allow {
    protocol = local.flow_file.protocol
    ports    = [tostring(each.value.port)]
  }
}

resource "google_compute_firewall" "allow_out" {
  for_each = local.flows

  name               = "${var.name}-allow-${each.key}-out"
  network            = var.gcp.network_name
  direction          = "EGRESS"
  priority           = 1000
  destination_ranges = local.tier_cidrs[each.value.destination]

  allow {
    protocol = local.flow_file.protocol
    ports    = [tostring(each.value.port)]
  }
}

resource "google_compute_firewall" "deny_out" {
  name               = "${var.name}-deny-all-out"
  network            = var.gcp.network_name
  direction          = "EGRESS"
  priority           = 65000
  destination_ranges = [for plan in values(var.plans) : plan.cidr]

  deny {
    protocol = "all"
  }
}
