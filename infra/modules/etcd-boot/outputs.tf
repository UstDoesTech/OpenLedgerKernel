output "ignition" {
  description = "The Ignition configuration, as JSON, for the machine's user data (custom data on Azure). It holds no credential."
  value       = jsonencode(local.ignition)
}

output "tls_files" {
  description = "The files the member reads from its certificate secret."
  value       = local.tls_files
}
