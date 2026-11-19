output "host" {
  description = "The primary node's private address."
  value       = azurerm_network_interface.node[0].private_ip_address
}

output "port" {
  description = "The PostgreSQL port."
  value       = 5432
}

output "secret_ref" {
  description = "The Key Vault secret that holds the administrator's credential. The primary's first start writes it."
  value       = local.secret_id
}
