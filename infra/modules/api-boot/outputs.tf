output "ignition" {
  description = "The Ignition configuration, as JSON, for the machine's user data (custom data on Azure). It holds no credential."
  value       = jsonencode(local.ignition)
}

output "registry" {
  description = "The registry host the image comes from."
  value       = local.registry
}

output "secret_files" {
  description = "The files the API secret must hold, as entries of one JSON object. Whoever runs the certificate authority and issues the tokens puts them in; they are never in OpenTofu's state."
  value       = local.secret_files
}

output "tls_terminates" {
  description = "Where TLS ends: always \"service\". The API serves TLS itself, and there is no load balancer in front of it."
  value       = "service"
}
