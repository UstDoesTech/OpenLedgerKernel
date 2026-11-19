output "addresses" {
  description = "The members' private addresses, by member name."
  value       = { for k, n in azurerm_network_interface.member : k => n.private_ip_address }
}

output "secret_refs" {
  description = "The Key Vault secret of each member's certificates, by member name. Whoever issues the certificates writes them here."
  value       = { for k, s in azurerm_key_vault_secret.tls : k => s.versionless_id }
}
