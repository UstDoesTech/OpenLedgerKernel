# The tenant's own encryption key in each cloud. No other tenant, and not the control plane,
# may use these keys, and this tenant's cells use no other key.

# AWS
resource "aws_kms_key" "cell" {
  description             = "Data and repository for tenant ${local.tenant}"
  enable_key_rotation     = true
  deletion_window_in_days = 30
  tags                    = local.tags
}

resource "aws_kms_alias" "cell" {
  name          = "alias/ledger-${local.hyphen}"
  target_key_id = aws_kms_key.cell.key_id
}

# Azure: a key in the platform's Key Vault, an identity that may use that key and nothing else in
# the vault, and a disk encryption set for self-hosted nodes.
resource "azurerm_key_vault_key" "cell" {
  name         = "${local.name}-data"
  key_vault_id = data.terraform_remote_state.private_access.outputs.azure_key_vault_id
  key_type     = "RSA-HSM"
  key_size     = 3072
  key_opts     = ["decrypt", "encrypt", "sign", "unwrapKey", "verify", "wrapKey"]
  tags         = local.tags
}

resource "azurerm_user_assigned_identity" "cell" {
  name                = "${local.name}-data"
  resource_group_name = data.terraform_remote_state.network.outputs.azure.resource_group_name
  location            = module.regions.azure[0]
  tags                = local.tags
}

resource "azurerm_role_assignment" "cell_identity_key" {
  scope                = azurerm_key_vault_key.cell.resource_versionless_id
  role_definition_name = "Key Vault Crypto Service Encryption User"
  principal_id         = azurerm_user_assigned_identity.cell.principal_id
}

resource "azurerm_disk_encryption_set" "cell" {
  name                      = "${local.name}-disks"
  resource_group_name       = data.terraform_remote_state.network.outputs.azure.resource_group_name
  location                  = module.regions.azure[0]
  key_vault_key_id          = azurerm_key_vault_key.cell.versionless_id
  auto_key_rotation_enabled = true
  tags                      = local.tags

  identity {
    type = "SystemAssigned"
  }
}

resource "azurerm_role_assignment" "cell_disk_key" {
  scope                = azurerm_key_vault_key.cell.resource_versionless_id
  role_definition_name = "Key Vault Crypto Service Encryption User"
  principal_id         = azurerm_disk_encryption_set.cell.identity[0].principal_id
}

# Google Cloud: a key ring and key in the region, which the Compute Engine and Secret Manager
# service agents may use, for the tenant's disks and its certificate and administrator secrets.
resource "google_kms_key_ring" "cell" {
  name     = local.hyphen
  location = module.regions.gcp[0]
}

resource "google_kms_crypto_key" "cell" {
  name            = "cell"
  key_ring        = google_kms_key_ring.cell.id
  rotation_period = "7776000s"
  labels          = local.tags

  lifecycle {
    prevent_destroy = true
  }
}

resource "google_kms_crypto_key_iam_member" "agents" {
  for_each = {
    compute        = "serviceAccount:service-${data.google_project.current.number}@compute-system.iam.gserviceaccount.com"
    secret_manager = "serviceAccount:service-${data.google_project.current.number}@gcp-sa-secretmanager.iam.gserviceaccount.com"
  }

  crypto_key_id = google_kms_crypto_key.cell.id
  role          = "roles/cloudkms.cryptoKeyEncrypterDecrypter"
  member        = each.value
}
