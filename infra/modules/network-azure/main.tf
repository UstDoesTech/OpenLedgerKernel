# A private network in one Azure region: three subnets per tier, one per availability zone,
# no public addresses, no default outbound access, and network security groups that deny
# everything in both directions. Later modules add the rules they need with a priority below
# 4096. Azure subnets span zones, so subnet N of a tier is where zone N+1
# resources go. That keeps addressing the same on all three clouds.
locals {
  tags         = merge(var.tags, { managed_by = "opentofu" })
  watcher_name = coalesce(var.network_watcher_name, "NetworkWatcher_${var.location}")

  subnets = merge([
    for tier, cidrs in var.plan.subnets : {
      for zone, cidr in cidrs : "${tier}-${zone}" => { tier = tier, zone = zone, cidr = cidr }
    }
  ]...)

  rules = {
    "deny-internet-inbound"  = { priority = 100, direction = "Inbound", source = "Internet", destination = "*" }
    "deny-internet-outbound" = { priority = 100, direction = "Outbound", source = "*", destination = "Internet" }
    "deny-all-inbound"       = { priority = 4096, direction = "Inbound", source = "*", destination = "*" }
    "deny-all-outbound"      = { priority = 4096, direction = "Outbound", source = "*", destination = "*" }
  }

  baseline_rules = {
    for pair in setproduct(keys(var.plan.subnets), keys(local.rules)) : "${pair[0]}/${pair[1]}" => merge(local.rules[pair[1]], { tier = pair[0], rule = pair[1] })
  }
}

resource "azurerm_resource_group" "this" {
  name     = var.name
  location = var.location
  tags     = local.tags
}

resource "azurerm_virtual_network" "this" {
  name                = var.name
  location            = azurerm_resource_group.this.location
  resource_group_name = azurerm_resource_group.this.name
  address_space       = [var.plan.cidr]
  tags                = local.tags
}

resource "azurerm_subnet" "this" {
  for_each = local.subnets

  name                            = each.key
  resource_group_name             = azurerm_resource_group.this.name
  virtual_network_name            = azurerm_virtual_network.this.name
  address_prefixes                = [each.value.cidr]
  default_outbound_access_enabled = false
}

# Network security groups keep no inline rules. Azure replaces a group's rules with its inline
# ones on every apply, so every rule here and in modules/interconnect is its own resource.
resource "azurerm_network_security_group" "tier" {
  for_each = toset(keys(var.plan.subnets))

  name                = "${var.name}-${each.key}"
  location            = azurerm_resource_group.this.location
  resource_group_name = azurerm_resource_group.this.name
  tags                = merge(local.tags, { tier = each.key })
}

# Everything is denied, in and out. Allow rules sit at a priority below 4096.
resource "azurerm_network_security_rule" "baseline" {
  for_each = local.baseline_rules

  name                        = each.value.rule
  resource_group_name         = azurerm_resource_group.this.name
  network_security_group_name = azurerm_network_security_group.tier[each.value.tier].name
  priority                    = each.value.priority
  direction                   = each.value.direction
  access                      = "Deny"
  protocol                    = "*"
  source_port_range           = "*"
  destination_port_range      = "*"
  source_address_prefix       = each.value.source
  destination_address_prefix  = each.value.destination
}

resource "azurerm_subnet_network_security_group_association" "this" {
  for_each = azurerm_subnet.this

  subnet_id                 = each.value.id
  network_security_group_id = azurerm_network_security_group.tier[local.subnets[each.key].tier].id
}

# Flow logs for the whole virtual network, written to a storage account that stays in the
# region (zone-redundant) and refuses public access.
resource "azurerm_storage_account" "flow" {
  name                            = "flow${substr(sha1(azurerm_resource_group.this.id), 0, 16)}"
  location                        = azurerm_resource_group.this.location
  resource_group_name             = azurerm_resource_group.this.name
  account_tier                    = "Standard"
  account_replication_type        = "ZRS"
  min_tls_version                 = "TLS1_2"
  https_traffic_only_enabled      = true
  allow_nested_items_to_be_public = false
  tags                            = local.tags

  network_rules {
    default_action = "Deny"
    bypass         = ["AzureServices"]
  }
}

data "azurerm_network_watcher" "this" {
  name                = local.watcher_name
  resource_group_name = var.network_watcher_resource_group_name
}

resource "azurerm_network_watcher_flow_log" "this" {
  name                 = "${var.name}-vnet"
  network_watcher_name = data.azurerm_network_watcher.this.name
  resource_group_name  = data.azurerm_network_watcher.this.resource_group_name
  target_resource_id   = azurerm_virtual_network.this.id
  storage_account_id   = azurerm_storage_account.flow.id
  enabled              = true
  version              = 2
  tags                 = local.tags

  retention_policy {
    enabled = true
    days    = var.flow_log_retention_days
  }
}
