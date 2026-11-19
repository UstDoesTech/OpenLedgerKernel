# Azure: one zone-redundant VPN gateway with BGP. It needs a subnet called GatewaySubnet, which
# the address plan reserves. AWS inside addresses end .1 (AWS) and .2 (the customer side).
# Google's end in .1 on the Google side and .2 on the Azure side. Azure uses the .2 addresses,
# so they are the gateway's APIPA addresses.
locals {
  azure_aws_tunnels = {
    "0" = { outside = aws_vpn_connection.azure.tunnel1_address, inside = local.tunnels["aws-azure"][0], key = "aws-azure-0" }
    "1" = { outside = aws_vpn_connection.azure.tunnel2_address, inside = local.tunnels["aws-azure"][1], key = "aws-azure-1" }
  }

  azure_gcp_tunnels = {
    "0" = { outside = google_compute_ha_vpn_gateway.this.vpn_interfaces[0].ip_address, inside = local.tunnels["azure-gcp"][0], key = "azure-gcp-0" }
    "1" = { outside = google_compute_ha_vpn_gateway.this.vpn_interfaces[1].ip_address, inside = local.tunnels["azure-gcp"][1], key = "azure-gcp-1" }
  }

  azure_apipa_addresses = concat(
    [for tunnel in local.azure_aws_tunnels : cidrhost(tunnel.inside, 2)],
    [for tunnel in local.azure_gcp_tunnels : cidrhost(tunnel.inside, 2)],
  )
}

resource "azurerm_subnet" "gateway" {
  name                 = "GatewaySubnet"
  resource_group_name  = var.azure.resource_group_name
  virtual_network_name = var.azure.network_name
  address_prefixes     = [var.plans.azure.gateway_subnet]
}

resource "azurerm_public_ip" "vpn" {
  name                = "${var.name}-vpn"
  location            = var.azure.location
  resource_group_name = var.azure.resource_group_name
  allocation_method   = "Static"
  sku                 = "Standard"
  zones               = ["1", "2", "3"]
  tags                = local.tags
}

resource "azurerm_virtual_network_gateway" "this" {
  name                = "${var.name}-vpn"
  location            = var.azure.location
  resource_group_name = var.azure.resource_group_name
  type                = "Vpn"
  vpn_type            = "RouteBased"
  sku                 = "VpnGw1AZ"
  active_active       = false
  bgp_enabled         = true
  tags                = local.tags

  ip_configuration {
    name                          = "vnetGatewayConfig"
    public_ip_address_id          = azurerm_public_ip.vpn.id
    private_ip_address_allocation = "Dynamic"
    subnet_id                     = azurerm_subnet.gateway.id
  }

  bgp_settings {
    asn = local.asn.azure

    peering_addresses {
      ip_configuration_name = "vnetGatewayConfig"
      apipa_addresses       = local.azure_apipa_addresses
    }
  }
}

# To AWS: one local network gateway and connection for each AWS tunnel endpoint.
resource "azurerm_local_network_gateway" "aws" {
  for_each = local.azure_aws_tunnels

  name                = "${var.name}-aws-${each.key}"
  location            = var.azure.location
  resource_group_name = var.azure.resource_group_name
  gateway_address     = each.value.outside
  address_space       = ["${cidrhost(each.value.inside, 1)}/32"]
  tags                = local.tags

  bgp_settings {
    asn                 = local.asn.aws
    bgp_peering_address = cidrhost(each.value.inside, 1)
  }
}

resource "azurerm_virtual_network_gateway_connection" "aws" {
  for_each = local.azure_aws_tunnels

  name                       = "${var.name}-aws-${each.key}"
  location                   = var.azure.location
  resource_group_name        = var.azure.resource_group_name
  type                       = "IPsec"
  virtual_network_gateway_id = azurerm_virtual_network_gateway.this.id
  local_network_gateway_id   = azurerm_local_network_gateway.aws[each.key].id
  shared_key                 = var.preshared_keys[each.value.key]
  bgp_enabled                = true
  connection_protocol        = "IKEv2"
  tags                       = local.tags

  custom_bgp_addresses {
    primary = cidrhost(each.value.inside, 2)
  }

  ipsec_policy {
    dh_group         = "DHGroup14"
    ike_encryption   = "AES256"
    ike_integrity    = "SHA256"
    ipsec_encryption = "AES256"
    ipsec_integrity  = "SHA256"
    pfs_group        = "PFS14"
    sa_lifetime      = local.ipsec.phase2_lifetime
  }

  depends_on = [terraform_data.preshared_key_check]
}

# To Google: one local network gateway and connection for each Google interface.
resource "azurerm_local_network_gateway" "gcp" {
  for_each = local.azure_gcp_tunnels

  name                = "${var.name}-gcp-${each.key}"
  location            = var.azure.location
  resource_group_name = var.azure.resource_group_name
  gateway_address     = each.value.outside
  address_space       = ["${cidrhost(each.value.inside, 1)}/32"]
  tags                = local.tags

  bgp_settings {
    asn                 = local.asn.gcp
    bgp_peering_address = cidrhost(each.value.inside, 1)
  }
}

resource "azurerm_virtual_network_gateway_connection" "gcp" {
  for_each = local.azure_gcp_tunnels

  name                       = "${var.name}-gcp-${each.key}"
  location                   = var.azure.location
  resource_group_name        = var.azure.resource_group_name
  type                       = "IPsec"
  virtual_network_gateway_id = azurerm_virtual_network_gateway.this.id
  local_network_gateway_id   = azurerm_local_network_gateway.gcp[each.key].id
  shared_key                 = var.preshared_keys[each.value.key]
  bgp_enabled                = true
  connection_protocol        = "IKEv2"
  tags                       = local.tags

  custom_bgp_addresses {
    primary = cidrhost(each.value.inside, 2)
  }

  ipsec_policy {
    dh_group         = "DHGroup14"
    ike_encryption   = "AES256"
    ike_integrity    = "SHA256"
    ipsec_encryption = "AES256"
    ipsec_integrity  = "SHA256"
    pfs_group        = "PFS14"
    sa_lifetime      = local.ipsec.phase2_lifetime
  }

  depends_on = [terraform_data.preshared_key_check]
}
