output "members" {
  description = "Every member: its index, name, failure domain, cloud, zone number (0 to 2) and slot in that zone."
  value       = local.members
}

output "per_cloud" {
  description = "How many members each cloud has."
  value       = local.per_cloud
}

output "per_domain" {
  description = "How many members each failure domain has."
  value       = local.per_domain
}

output "quorum" {
  description = "The majority etcd needs."
  value       = local.quorum
}

output "survivors_after_loss" {
  description = "For each failure domain, how many members remain if it is lost."
  value       = local.survivors_after_loss
}

output "survives_any_single_loss" {
  description = "True when losing any one failure domain leaves a majority."
  value       = alltrue([for d, left in local.survivors_after_loss : left >= local.quorum])
}

output "placed" {
  description = "Every member with its full name and address. The address is null when the cloud's ranges were not given."
  value       = local.placed
}

output "initial_cluster" {
  description = "Every member as name=https://address:2380, joined by commas. It is the same on every member."
  value       = local.initial_cluster
}
