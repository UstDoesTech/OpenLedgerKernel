# What a machine with no internet route needs in order to pull its image and use its secret:
# private endpoints for the container registry and the Key Vault, with private DNS so the
# usual names resolve to private addresses inside the network. Nothing here is public.
locals {
  targets = {
    registry = {
      resource_id = var.registry_id
      subresource = "registry"
      dns_zone    = "privatelink.azurecr.io"
    }
    vault = {
      resource_id = var.key_vault_id
      subresource = "vault"
      dns_zone    = "privatelink.vaultcore.azure.net"
    }
  }
}

resource "azurerm_private_dns_zone" "this" {
  for_each = local.targets

  name                = each.value.dns_zone
  resource_group_name = var.resource_group_name
  tags                = var.tags
}

resource "azurerm_private_dns_zone_virtual_network_link" "this" {
  for_each = local.targets

  name                  = "${var.name}-${each.key}"
  resource_group_name   = var.resource_group_name
  private_dns_zone_name = azurerm_private_dns_zone.this[each.key].name
  virtual_network_id    = var.virtual_network_id
  tags                  = var.tags
}

resource "azurerm_private_endpoint" "this" {
  for_each = local.targets

  name                = "${var.name}-${each.key}"
  location            = var.location
  resource_group_name = var.resource_group_name
  subnet_id           = var.subnet_id
  tags                = var.tags

  private_service_connection {
    name                           = each.key
    private_connection_resource_id = each.value.resource_id
    subresource_names              = [each.value.subresource]
    is_manual_connection           = false
  }

  private_dns_zone_group {
    name                 = each.key
    private_dns_zone_ids = [azurerm_private_dns_zone.this[each.key].id]
  }
}

# The network denies everything in and out unless a rule allows it, so the machines cannot reach
# the endpoints without these. Each client tier may send HTTPS to the endpoints' subnets and
# nowhere else, and the endpoints' subnets accept HTTPS from those tiers only. The names and
# ranges come from the networks layer, and the port is the one the registry and the vault use.
resource "azurerm_network_security_rule" "to_endpoints" {
  for_each = var.client_groups

  name                         = "allow-private-endpoints-out"
  resource_group_name          = var.resource_group_name
  network_security_group_name  = each.value.network_security_group_name
  priority                     = 400
  direction                    = "Outbound"
  access                       = "Allow"
  protocol                     = "Tcp"
  source_port_range            = "*"
  destination_port_range       = "443"
  source_address_prefixes      = each.value.cidrs
  destination_address_prefixes = var.endpoint_cidrs
}

resource "azurerm_network_security_rule" "from_clients" {
  count = length(var.client_groups) > 0 ? 1 : 0

  name                         = "allow-private-endpoints-in"
  resource_group_name          = var.resource_group_name
  network_security_group_name  = var.endpoint_network_security_group_name
  priority                     = 400
  direction                    = "Inbound"
  access                       = "Allow"
  protocol                     = "Tcp"
  source_port_range            = "*"
  destination_port_range       = "443"
  source_address_prefixes      = distinct(flatten([for group in var.client_groups : group.cidrs]))
  destination_address_prefixes = var.endpoint_cidrs
}
