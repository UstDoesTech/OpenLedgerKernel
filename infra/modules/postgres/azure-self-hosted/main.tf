# Three machines, one per zone, on Flatcar Container Linux. Each runs the node image, given by
# digest, as a container. They have fixed private addresses, no public address, and disks
# encrypted with the tenant's key. They pull the image from a private registry and use the Key
# Vault, both over private endpoints. The first node starts as the primary and commits when one
# of the other two has the change (ANY 1). Its first start creates the administrator's credential
# and stores it in the secret made here, using the machine's own identity, which may read and
# write that one secret and nothing else. There is no automatic failover in this form.
locals {
  addresses = [for cidr in var.subnet_cidrs : cidrhost(cidr, 10)]
  names     = [for i in range(3) : "${var.name}-n${i + 1}"]
  secret    = "${var.name}-administrator"
  secret_id = azurerm_key_vault_secret.administrator.versionless_id

  node_config = [for i in range(3) : jsonencode({
    tenant                    = var.tenant
    node                      = local.names[i]
    role                      = i == 0 ? "primary" : "standby"
    postgres_version          = 16
    primary_address           = local.addresses[0]
    synchronous_standby_names = i == 0 ? "ANY 1 (${join(", ", slice(local.names, 1, 3))})" : ""
    secret_id                 = local.secret_id
    secret_store              = { kind = "azure_key_vault" }
  })]
}

# The secret exists before the machines do, so the machines' access can be limited to it. The
# value here is a placeholder, never a credential: the primary's first start writes the real
# one as a new version, and nothing here changes it again.
resource "azurerm_key_vault_secret" "administrator" {
  name         = local.secret
  key_vault_id = var.key_vault_id
  value        = "set-by-the-primary-on-first-start"
  tags         = var.tags

  lifecycle {
    ignore_changes = [value]
  }
}

module "private_access" {
  source = "../../private-access-azure"
  count  = var.reuse_private_access ? 0 : 1

  name                = var.name
  location            = var.location
  resource_group_name = var.resource_group_name
  virtual_network_id  = var.virtual_network_id
  subnet_id           = var.subnet_ids[0]
  registry_id         = var.registry_id
  key_vault_id        = var.key_vault_id
  tags                = var.tags
}

module "boot" {
  source = "../../node-boot"
  count  = 3

  name            = local.names[count.index]
  cloud           = "azure"
  image           = var.image
  storage_gb      = var.storage_gb
  node_config     = local.node_config[count.index]
  azure_tenant_id = var.tenant_id
  tls_secret      = azurerm_key_vault_secret.tls[count.index].versionless_id
  tls_files       = count.index == 0 ? local.tls_files.primary : local.tls_files.standby
}

locals {
  tls_files = {
    primary = ["server.crt", "server.key", "ca.crt"]
    standby = ["server.crt", "server.key", "ca.crt", "replication.crt", "replication.key"]
  }
}

# One secret for each node's certificates and keys, as a JSON object with one entry for each
# file. OpenTofu makes the secret with an empty placeholder and never sets its contents:
# whoever issues the certificates puts the values in as a new version, so they are not in the
# state. The node reads its own secret at boot.
resource "azurerm_key_vault_secret" "tls" {
  count = 3

  name         = "${local.names[count.index]}-tls"
  key_vault_id = var.key_vault_id
  value        = "{}"
  tags         = var.tags

  lifecycle {
    ignore_changes = [value]
  }
}

resource "azurerm_network_interface" "node" {
  count = 3

  name                = local.names[count.index]
  location            = var.location
  resource_group_name = var.resource_group_name
  tags                = var.tags

  ip_configuration {
    name                          = "primary"
    subnet_id                     = var.subnet_ids[count.index]
    private_ip_address_allocation = "Static" # uk-guard: ignore not a region, the IP allocation method
    private_ip_address            = local.addresses[count.index]
  }
}

resource "azurerm_linux_virtual_machine" "node" {
  count = 3

  name                            = local.names[count.index]
  location                        = var.location
  resource_group_name             = var.resource_group_name
  size                            = var.vm_size
  zone                            = tostring(count.index + 1)
  source_image_id                 = var.operating_system_image_id
  network_interface_ids           = [azurerm_network_interface.node[count.index].id]
  admin_username                  = "ledger"
  disable_password_authentication = true
  custom_data                     = base64encode(module.boot[count.index].ignition)
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
  count = 3

  name                   = "${local.names[count.index]}-data"
  location               = var.location
  resource_group_name    = var.resource_group_name
  zone                   = tostring(count.index + 1)
  storage_account_type   = "Premium_LRS"
  create_option          = "Empty"
  disk_size_gb           = var.storage_gb
  disk_encryption_set_id = var.disk_encryption_set_id
  tags                   = var.tags
}

resource "azurerm_virtual_machine_data_disk_attachment" "data" {
  count = 3

  managed_disk_id    = azurerm_managed_disk.data[count.index].id
  virtual_machine_id = azurerm_linux_virtual_machine.node[count.index].id
  lun                = 0
  caching            = "ReadWrite"
}

# The nodes may read and write the administrator's secret, and nothing else. The role is
# custom, with only those two actions, and it is assigned on the one secret.
resource "azurerm_role_definition" "secret" {
  name        = "${var.name}-administrator-secret"
  scope       = var.key_vault_id
  description = "Read and write one secret. Nothing else."

  permissions {
    data_actions = [
      "Microsoft.KeyVault/vaults/secrets/getSecret/action",
      "Microsoft.KeyVault/vaults/secrets/setSecret/action",
    ]
  }

  assignable_scopes = [var.key_vault_id]
}

resource "azurerm_role_assignment" "secret" {
  count = 3

  scope              = azurerm_key_vault_secret.administrator.resource_versionless_id
  role_definition_id = azurerm_role_definition.secret.role_definition_resource_id
  principal_id       = azurerm_linux_virtual_machine.node[count.index].identity[0].principal_id
}

# The nodes may pull images from the registry, and nothing else there.
resource "azurerm_role_assignment" "registry" {
  count = 3

  scope                = var.registry_id
  role_definition_name = "AcrPull"
  principal_id         = azurerm_linux_virtual_machine.node[count.index].identity[0].principal_id
}

# Each node may read its own certificates, and no other node's: a custom role with one action,
# assigned on the one secret.
resource "azurerm_role_definition" "certificates" {
  name        = "${var.name}-node-certificates"
  scope       = var.key_vault_id
  description = "Read one secret. Nothing else."

  permissions {
    data_actions = ["Microsoft.KeyVault/vaults/secrets/getSecret/action"]
  }

  assignable_scopes = [var.key_vault_id]
}

resource "azurerm_role_assignment" "certificates" {
  count = 3

  scope              = azurerm_key_vault_secret.tls[count.index].resource_versionless_id
  role_definition_id = azurerm_role_definition.certificates.role_definition_resource_id
  principal_id       = azurerm_linux_virtual_machine.node[count.index].identity[0].principal_id
}
