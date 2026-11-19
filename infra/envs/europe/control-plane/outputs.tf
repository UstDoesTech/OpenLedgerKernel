output "endpoints" {
  description = "The etcd client endpoints."
  value       = module.control_plane.endpoints
}

output "members" {
  description = "The etcd members and where they are placed."
  value       = module.control_plane.members
}

output "quorum" {
  description = "How many members make a majority."
  value       = module.control_plane.quorum
}

output "survives_any_single_loss" {
  description = "Whether the cluster keeps a majority when any one cloud is lost."
  value       = module.control_plane.survives_any_single_loss
}

output "certificate_secrets" {
  description = "Where each member's certificates go, for whoever runs the certificate authority."
  value       = module.control_plane.certificate_secrets
}
