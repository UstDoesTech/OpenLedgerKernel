# The public port, 8443, from the named client ranges and nowhere else. With no ranges, no rule
# is made. The ports between the platform's own tiers come from infra/flows.json, applied by the
# interconnect layer, so none is written here.
locals {
  client_port   = 8443
  internal_port = jsondecode(file("${path.module}/../../../flows.json")).ports.api_internal
}

resource "aws_vpc_security_group_ingress_rule" "clients" {
  for_each = toset(var.client_cidrs)

  security_group_id = local.network.aws.security_group_ids["api"]
  cidr_ipv4         = each.value
  ip_protocol       = "tcp"
  from_port         = local.client_port
  to_port           = local.client_port
  description       = "Allow the public port of the API from a named client range"
}

resource "azurerm_network_security_rule" "clients" {
  count = length(var.client_cidrs) > 0 ? 1 : 0

  name                         = "allow-api-clients-in"
  resource_group_name          = local.network.azure.resource_group_name
  network_security_group_name  = local.network.azure.network_security_group_names["api"]
  priority                     = 300
  direction                    = "Inbound"
  access                       = "Allow"
  protocol                     = "Tcp"
  source_port_range            = "*"
  destination_port_range       = tostring(local.client_port)
  source_address_prefixes      = var.client_cidrs
  destination_address_prefixes = local.plan.azure.subnets["api"]
}

resource "google_compute_firewall" "clients" {
  count = length(var.client_cidrs) > 0 ? 1 : 0

  name               = "${local.slug}-allow-api-clients-in"
  network            = local.network.gcp.network_name
  direction          = "INGRESS"
  priority           = 1000
  source_ranges      = var.client_cidrs
  destination_ranges = local.plan.gcp.subnets["api"]

  allow {
    protocol = "tcp"
    ports    = [tostring(local.client_port)]
  }
}
