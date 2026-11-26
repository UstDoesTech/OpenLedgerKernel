# One small machine, on Flatcar Container Linux, that runs the API image next to a cell. It has a
# private address and no public one, and disks encrypted with the tenant's key. Its managed
# identity may read the cell's one API secret and pull the API image, and nothing else.
module "boot" {
  source = "../../api-boot"

  name             = var.name
  cell             = var.cell
  cloud            = "azure"
  image            = var.image
  secret           = azurerm_key_vault_secret.api.versionless_id
  etcd_endpoints   = var.etcd_endpoints
  azure_tenant_id  = var.tenant_id
  migrate_database = var.migrate_database
}

# The secret is made with an empty placeholder and OpenTofu never sets its contents: whoever
# issues the certificates and tokens puts the values in as a new version, so they are never in
# the state.
resource "azurerm_key_vault_secret" "api" {
  name         = "${var.name}-api"
  key_vault_id = var.key_vault_id
  value        = "{}"
  tags         = var.tags

  lifecycle {
    ignore_changes = [value]
  }
}

resource "azurerm_network_interface" "api" {
  name                = var.name
  location            = var.location
  resource_group_name = var.resource_group_name
  tags                = var.tags

  ip_configuration {
    name                          = "primary"
    subnet_id                     = var.subnet_id
    private_ip_address_allocation = "Dynamic" # uk-guard: ignore not a region, the IP allocation method
  }
}

resource "azurerm_linux_virtual_machine" "api" {
  name                            = var.name
  location                        = var.location
  resource_group_name             = var.resource_group_name
  size                            = var.vm_size
  zone                            = var.zone
  source_image_id                 = var.operating_system_image_id
  network_interface_ids           = [azurerm_network_interface.api.id]
  admin_username                  = "ledger"
  disable_password_authentication = true
  custom_data                     = base64encode(module.boot.ignition)
  encryption_at_host_enabled      = true
  tags                            = var.tags

  admin_ssh_key {
    username   = "ledger"
    public_key = var.administrator_ssh_public_key
  }

  identity {
    type = "SystemAssigned"
  }

  os_disk {
    caching                = "ReadWrite"
    storage_account_type   = "Premium_LRS"
    disk_encryption_set_id = var.disk_encryption_set_id
  }

  lifecycle {
    ignore_changes = [source_image_id, custom_data]
  }
}

# The machine may read the one secret, and nothing else: a custom role with one action, assigned
# on the one secret.
resource "azurerm_role_definition" "secret" {
  name        = "${var.name}-api-secret"
  scope       = var.key_vault_id
  description = "Read one secret. Nothing else."

  permissions {
    data_actions = ["Microsoft.KeyVault/vaults/secrets/getSecret/action"]
  }

  assignable_scopes = [var.key_vault_id]
}

resource "azurerm_role_assignment" "secret" {
  scope              = azurerm_key_vault_secret.api.resource_versionless_id
  role_definition_id = azurerm_role_definition.secret.role_definition_resource_id
  principal_id       = azurerm_linux_virtual_machine.api.identity[0].principal_id
}

# The machine may pull images from the registry, and nothing else there.
resource "azurerm_role_assignment" "registry" {
  scope                = var.registry_id
  role_definition_name = "AcrPull"
  principal_id         = azurerm_linux_virtual_machine.api.identity[0].principal_id
}
