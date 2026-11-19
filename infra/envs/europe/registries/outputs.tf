output "aws_repository_url" {
  description = "The AWS repository's address, without a tag or digest."
  value       = aws_ecr_repository.etcd.repository_url
}

output "aws_repository_arn" {
  description = "The AWS repository, for the control plane layer's pull rights."
  value       = aws_ecr_repository.etcd.arn
}

output "gcp_repository_id" {
  description = "The Google Cloud repository's full id, for the control plane layer's pull rights."
  value       = google_artifact_registry_repository.etcd.id
}

output "gcp_repository_name" {
  description = "The Google Cloud repository's short name, used in image addresses."
  value       = google_artifact_registry_repository.etcd.repository_id
}

output "etcd_repositories" {
  description = "Where to copy the etcd image to, in each cloud (no tag or digest)."
  value = {
    aws   = aws_ecr_repository.etcd.repository_url
    azure = "${data.terraform_remote_state.private_access.outputs.azure_registry_login_server}/etcd"
    gcp   = "${module.regions.gcp[0]}-docker.pkg.dev/${data.google_project.current.project_id}/${google_artifact_registry_repository.etcd.repository_id}/etcd"
  }
}
