output "ignition" {
  description = "The Ignition configuration, as JSON, for the machine's user data (custom data on Azure). It holds no credential."
  value       = jsonencode(local.ignition)
}

output "registry" {
  description = "The registry host the image comes from."
  value       = local.registry
}
