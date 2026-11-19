output "host" {
  description = "The server's name. Inside the network it resolves to the private endpoint."
  value       = azurerm_postgresql_flexible_server.this.fqdn
}

output "port" {
  description = "The server's port."
  value       = 5432
}

output "secret_ref" {
  description = "The Entra principal that administers the server. There is no password: it signs in with an access token."
  value       = "entra:${var.administrator.principal_name}"
}
