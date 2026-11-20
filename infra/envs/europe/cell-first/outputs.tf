output "tenant" {
  description = "The tenant this layer is for."
  value       = local.tenant
}

output "aws" {
  description = "What the AWS cells of this tenant take: its key and its repository."
  value = {
    kms_key_arn             = aws_kms_key.cell.arn
    registry_repository_arn = aws_ecr_repository.postgres.arn
    image                   = "${aws_ecr_repository.postgres.repository_url}@${var.postgres_image_digest}"
  }
}

output "azure" {
  description = "What the Azure cells of this tenant take: its key, identity, disk encryption set, administrator group and image address."
  value = {
    key_vault_key_id       = azurerm_key_vault_key.cell.versionless_id
    identity_id            = azurerm_user_assigned_identity.cell.id
    disk_encryption_set_id = azurerm_disk_encryption_set.cell.id
    administrator = {
      principal_name = var.azure_administrator_name
      object_id      = var.azure_administrator_object_id
      principal_type = "Group"
    }
    image = "${data.terraform_remote_state.private_access.outputs.azure_registry_login_server}/${local.tenant}/postgres@${var.postgres_image_digest}"
  }
}

output "gcp" {
  description = "What the Google Cloud cells of this tenant take: its key, administrator and repository."
  value = {
    kms_key_id             = google_kms_crypto_key.cell.id
    administrator_email    = google_service_account.administrator.email
    registry_repository_id = google_artifact_registry_repository.postgres.id
    image                  = "${module.regions.gcp[0]}-docker.pkg.dev/${data.google_project.current.project_id}/${google_artifact_registry_repository.postgres.repository_id}/postgres@${var.postgres_image_digest}"
  }
}

output "postgres_repositories" {
  description = "Where to copy the PostgreSQL node image to, in each cloud (no tag or digest)."
  value = {
    aws   = aws_ecr_repository.postgres.repository_url
    azure = "${data.terraform_remote_state.private_access.outputs.azure_registry_login_server}/${local.tenant}/postgres"
    gcp   = "${module.regions.gcp[0]}-docker.pkg.dev/${data.google_project.current.project_id}/${google_artifact_registry_repository.postgres.repository_id}/postgres"
  }
}

output "api" {
  description = "What the API machines next to this tenant's cells take in each cloud: the repository they may read and the image address, by the digest in infra/images.json."
  value = {
    "aws" = {
      registry_repository_arn = aws_ecr_repository.api.arn
      image                   = "${aws_ecr_repository.api.repository_url}@${local.api_digest}"
    }
    "azure" = {
      image = "${data.terraform_remote_state.private_access.outputs.azure_registry_login_server}/${local.tenant}/api@${local.api_digest}"
    }
    "gcp" = {
      registry_repository_id = google_artifact_registry_repository.api.id
      image                  = "${module.regions.gcp[0]}-docker.pkg.dev/${data.google_project.current.project_id}/${google_artifact_registry_repository.api.repository_id}/api@${local.api_digest}"
    }
  }
}

output "api_repositories" {
  description = "Where to copy the API image to, in each cloud (no tag or digest)."
  value = {
    aws   = aws_ecr_repository.api.repository_url
    azure = "${data.terraform_remote_state.private_access.outputs.azure_registry_login_server}/${local.tenant}/api"
    gcp   = "${module.regions.gcp[0]}-docker.pkg.dev/${data.google_project.current.project_id}/${google_artifact_registry_repository.api.repository_id}/api"
  }
}
