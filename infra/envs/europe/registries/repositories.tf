# AWS: a key for the repository, and the repository. Tags cannot change, and images are
# scanned when they are pushed.
resource "aws_kms_key" "registries" {
  description             = "Container image repositories"
  enable_key_rotation     = true
  deletion_window_in_days = 30
  tags                    = { managed_by = "opentofu", component = "registries" }
}

resource "aws_kms_alias" "registries" {
  name          = "alias/ledger-${local.geo}-registries"
  target_key_id = aws_kms_key.registries.key_id
}

resource "aws_ecr_repository" "etcd" {
  name                 = "etcd"
  image_tag_mutability = "IMMUTABLE"

  image_scanning_configuration {
    scan_on_push = true
  }

  encryption_configuration {
    encryption_type = "KMS"
    kms_key         = aws_kms_key.registries.arn
  }

  tags = { managed_by = "opentofu", component = "registries" }
}

# Google Cloud: an Artifact Registry repository whose tags cannot change.
resource "google_artifact_registry_repository" "etcd" {
  location      = module.regions.gcp[0]
  repository_id = "etcd"
  format        = "DOCKER"
  description   = "The etcd image for the control plane"
  labels        = { managed_by = "opentofu", component = "registries" }

  docker_config {
    immutable_tags = true
  }
}
