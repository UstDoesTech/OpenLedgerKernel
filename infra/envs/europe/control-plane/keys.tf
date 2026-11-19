# What the control plane module takes as inputs but does not make: for each cloud, an encryption
# key for the members' disks and certificate secrets. The etcd image repositories are made by the
# registries layer, which comes first.

# AWS
resource "aws_kms_key" "control_plane" {
  description             = "Control plane disks and certificate secrets"
  enable_key_rotation     = true
  deletion_window_in_days = 30
  tags                    = { managed_by = "opentofu", component = "control-plane" }
}

resource "aws_kms_alias" "control_plane" {
  name          = "alias/${local.name}"
  target_key_id = aws_kms_key.control_plane.key_id
}

# Azure: a key in the platform's Key Vault, and a disk encryption set that uses it. The set has
# an identity of its own, and may use the key and nothing else in the vault.
resource "azurerm_key_vault_key" "disk" {
  name         = "${local.name}-disks"
  key_vault_id = local.private_access.azure_key_vault_id
  key_type     = "RSA-HSM"
  key_size     = 3072
  key_opts     = ["decrypt", "encrypt", "sign", "unwrapKey", "verify", "wrapKey"]
}

resource "azurerm_disk_encryption_set" "control_plane" {
  name                      = "${local.name}-disks"
  resource_group_name       = local.network.azure.resource_group_name
  location                  = module.regions.azure[0]
  key_vault_key_id          = azurerm_key_vault_key.disk.versionless_id
  auto_key_rotation_enabled = true
  tags                      = { managed_by = "opentofu", component = "control-plane" }

  identity {
    type = "SystemAssigned"
  }
}

resource "azurerm_role_assignment" "disk_key" {
  scope                = azurerm_key_vault_key.disk.resource_versionless_id
  role_definition_name = "Key Vault Crypto Service Encryption User"
  principal_id         = azurerm_disk_encryption_set.control_plane.identity[0].principal_id
}

# Google Cloud: a key ring and key in the region, which the Compute Engine and Secret Manager
# service agents may use.
resource "google_kms_key_ring" "control_plane" {
  name     = local.name
  location = module.regions.gcp[0]
}

resource "google_kms_crypto_key" "control_plane" {
  name            = "control-plane"
  key_ring        = google_kms_key_ring.control_plane.id
  rotation_period = "7776000s"

  lifecycle {
    prevent_destroy = true
  }
}

resource "google_kms_crypto_key_iam_member" "agents" {
  for_each = {
    compute        = "serviceAccount:service-${data.google_project.current.number}@compute-system.iam.gserviceaccount.com"
    secret_manager = "serviceAccount:service-${data.google_project.current.number}@gcp-sa-secretmanager.iam.gserviceaccount.com"
  }

  crypto_key_id = google_kms_crypto_key.control_plane.id
  role          = "roles/cloudkms.cryptoKeyEncrypterDecrypter"
  member        = each.value
}
