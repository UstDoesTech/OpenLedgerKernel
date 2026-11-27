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

output "aws_kms_key_arn" {
  description = "The AWS key that encrypts the members' disks and certificate secrets. An identifier, not a secret: the rotation identity is given use of it to write the members' certificates."
  value       = aws_kms_key.control_plane.arn
}
