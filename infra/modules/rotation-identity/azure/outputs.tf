output "client_id" {
  description = "The identity's client id. It is the AZURE_CLIENT_ID secret of the rotate-credentials environment."
  value       = azurerm_user_assigned_identity.this.client_id
}

output "tenant_id" {
  description = "The directory the identity is in. It is the AZURE_TENANT_ID secret."
  value       = azurerm_user_assigned_identity.this.tenant_id
}
