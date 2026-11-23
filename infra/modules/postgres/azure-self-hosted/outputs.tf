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

output "nodes" {
  description = "Each node's name, role and private address, and the identifier of the secret that holds its certificates. Never a certificate or a key. Whoever issues the certificates writes them there."
  value = [for i in range(3) : {
    name       = local.names[i]
    role       = i == 0 ? "primary" : "standby"
    address    = local.addresses[i]
    secret_ref = azurerm_key_vault_secret.tls[i].versionless_id
  }]
}
