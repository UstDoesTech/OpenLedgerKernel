# What the Azure module takes as inputs but does not make: the platform's container registry and
# Key Vault. Both are private (reached only through the private endpoints) and their names are
# made unique from the subscription id, because both are global names.
data "azurerm_client_config" "current" {}

locals {
  suffix = substr(replace(var.azure_subscription_id, "-", ""), 0, 6)
}

# A Premium registry, the tier that supports private endpoints.
resource "azurerm_container_registry" "this" {
  name                          = "ledger${local.geo}${local.suffix}"
  resource_group_name           = local.network.azure.resource_group_name
  location                      = module.regions.azure[0]
  sku                           = "Premium"
  admin_enabled                 = false
  public_network_access_enabled = false
  tags                          = { managed_by = "opentofu" }
}

resource "azurerm_key_vault" "this" {
  name                       = "kv-${local.name}-${local.suffix}"
  resource_group_name        = local.network.azure.resource_group_name
  location                   = module.regions.azure[0]
  tenant_id                  = data.azurerm_client_config.current.tenant_id
  sku_name                   = "premium"
  rbac_authorization_enabled = true
  purge_protection_enabled   = true
  soft_delete_retention_days = 90
  # The endpoint is public but the firewall is on Deny with no standing address. Each plan or
  # apply run adds its own runner address and removes it again (the infra-apply workflow), and
  # the machines use the private endpoint. The addresses are managed by the workflow, so a
  # plan ignores them.
  public_network_access_enabled = true
  tags                          = { managed_by = "opentofu" }

  network_acls {
    default_action = "Deny"
    bypass         = "AzureServices"
  }

  lifecycle {
    ignore_changes = [network_acls[0].ip_rules]
  }
}
