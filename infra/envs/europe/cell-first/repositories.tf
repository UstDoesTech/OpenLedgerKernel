# A repository of the tenant's own for the PostgreSQL node image, in each cloud that makes one.
# Tags cannot change. Azure needs no resource: its registry makes the repository on first push, at
# the address given in the outputs.

resource "aws_ecr_repository" "postgres" {
  name                 = "${local.tenant}/postgres"
  image_tag_mutability = "IMMUTABLE"

  image_scanning_configuration {
    scan_on_push = true
  }

  encryption_configuration {
    encryption_type = "KMS"
    kms_key         = aws_kms_key.cell.arn
  }

  tags = local.tags
}

resource "google_artifact_registry_repository" "postgres" {
  location      = module.regions.gcp[0]
  repository_id = "${local.hyphen}-postgres"
  format        = "DOCKER"
  description   = "The PostgreSQL node image for ${local.tenant}"
  labels        = local.tags

  docker_config {
    immutable_tags = true
  }
}
