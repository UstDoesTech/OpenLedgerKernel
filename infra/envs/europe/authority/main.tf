# The authority layer of the Europe environment: the one central certificate authority (decision
# D-17). Its key is a signing key in AWS KMS. The key can sign and cannot be exported, so nobody
# holds it: the rotate-credentials workflow asks KMS to sign each certificate. The authority's own
# certificate is public, and is kept in a secret that the first run fills. Nothing here is a secret
# value, and nothing here is stored in a variable.
locals {
  geo = jsondecode(file("${path.module}/../deployment.json")).geo
}

module "regions" {
  source = "../../../modules/regions"
  geo    = local.geo
}

provider "aws" {
  region = module.regions.aws[0]
}

resource "aws_kms_key" "authority" {
  description              = "Signs the certificates of the ledger's cells"
  key_usage                = "SIGN_VERIFY"
  customer_master_key_spec = "ECC_NIST_P256"

  # A key that is deleted takes every certificate it signed with it. The longest wait, so a mistake
  # can be undone.
  deletion_window_in_days = 30

  # No policy is written here, so the account's own policy applies: access comes from the roles that
  # are allowed kms:Sign and kms:GetPublicKey on this key, and from nothing else. Asymmetric keys
  # cannot be rotated by KMS, and this one is replaced on purpose if it ever has to be.
}

resource "aws_kms_alias" "authority" {
  name          = "alias/ledger-authority-${local.geo}"
  target_key_id = aws_kms_key.authority.key_id
}

# The authority's public certificate. It is empty until the rotate-credentials workflow first runs,
# which makes the certificate and writes it. It is public by nature, and kept here so that every run
# trusts the same one.
resource "aws_secretsmanager_secret" "authority_certificate" {
  name        = "ledger-authority-certificate-${local.geo}"
  description = "The public certificate of the ledger's authority. Filled by the rotate-credentials workflow."

  # Terraform never owns a value, so a later apply cannot overwrite what the workflow wrote.
  recovery_window_in_days = 30
}
