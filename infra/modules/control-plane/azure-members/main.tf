# The etcd members that run on Azure, one machine each, on Flatcar Container Linux. Each runs etcd
# as a container from an image given by digest. They have fixed private addresses, no public
# address, and disks encrypted with a key. They pull the image from a private registry and read
# their certificates from a Key Vault, both over private endpoints. Each member's own identity may
# read its own certificate secret and nothing else.
locals {
  members = { for m in var.members : m.name => m }
}

module "boot" {
  source   = "../../etcd-boot"
  for_each = local.members

  name            = each.key
  cloud           = "azure"
  image           = var.image
  storage_gb      = var.storage_gb
  address         = each.value.address
  initial_cluster = var.initial_cluster
  cluster_id      = var.cluster_id
  tls_reference   = azurerm_key_vault_secret.tls[each.key].versionless_id
  azure_tenant_id = var.tenant_id
}

# One secret for each member's certificates and keys, as a JSON object with one entry for each
# file. OpenTofu makes the secret with an empty placeholder and never sets its contents: whoever
# issues the certificates puts the values in as a new version, so they are not in the state.
resource "azurerm_key_vault_secret" "tls" {
  for_each = local.members

  name         = "${each.key}-tls"
  key_vault_id = var.key_vault_id
  value        = "{}"
  tags         = var.tags

  lifecycle {
    ignore_changes = [value]
  }
}

resource "azurerm_network_interface" "member" {
  for_each = local.members

  name                = each.key
  location            = var.location
  resource_group_name = var.resource_group_name
  tags                = var.tags

  ip_configuration {
    name                          = "primary"
    subnet_id                     = var.subnet_ids[each.value.zone]
    private_ip_address_allocation = "Static" # uk-guard: ignore not a region, the IP allocation method
    private_ip_address            = each.value.address
  }
}

resource "azurerm_linux_virtual_machine" "member" {
  for_each = local.members

  name                            = each.key
  location                        = var.location
  resource_group_name             = var.resource_group_name
  size                            = var.vm_size
  zone                            = tostring(each.value.zone + 1)
  source_image_id                 = var.operating_system_image_id
  network_interface_ids           = [azurerm_network_interface.member[each.key].id]
  admin_username                  = "ledger"
  disable_password_authentication = true
  custom_data                     = base64encode(module.boot[each.key].ignition)
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

resource "azurerm_managed_disk" "data" {
  for_each = local.members

  name                   = "${each.key}-data"
  location               = var.location
  resource_group_name    = var.resource_group_name
  zone                   = tostring(each.value.zone + 1)
  storage_account_type   = "Premium_LRS"
  create_option          = "Empty"
  disk_size_gb           = var.storage_gb
  disk_encryption_set_id = var.disk_encryption_set_id
  tags                   = var.tags
}

resource "azurerm_virtual_machine_data_disk_attachment" "data" {
  for_each = local.members

  managed_disk_id    = azurerm_managed_disk.data[each.key].id
  virtual_machine_id = azurerm_linux_virtual_machine.member[each.key].id
  lun                = 0
  caching            = "ReadWrite"
}

# A member may read its own certificates, and no other member's: a custom role with one action,
# assigned on the one secret.
resource "azurerm_role_definition" "certificates" {
  name        = "${var.name}-member-certificates"
  scope       = var.key_vault_id
  description = "Read one secret. Nothing else."

  permissions {
    data_actions = ["Microsoft.KeyVault/vaults/secrets/getSecret/action"]
  }

  assignable_scopes = [var.key_vault_id]
}

resource "azurerm_role_assignment" "certificates" {
  for_each = local.members

  scope              = azurerm_key_vault_secret.tls[each.key].resource_versionless_id
  role_definition_id = azurerm_role_definition.certificates.role_definition_resource_id
  principal_id       = azurerm_linux_virtual_machine.member[each.key].identity[0].principal_id
}

# The members may pull images from the registry, and nothing else there.
resource "azurerm_role_assignment" "registry" {
  for_each = local.members

  scope                = var.registry_id
  role_definition_name = "AcrPull"
  principal_id         = azurerm_linux_virtual_machine.member[each.key].identity[0].principal_id
}
