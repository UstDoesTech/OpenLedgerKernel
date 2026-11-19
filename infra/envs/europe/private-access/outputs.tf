output "azure_registry_id" {
  description = "The Azure container registry, for the layers that read this one."
  value       = azurerm_container_registry.this.id
}

output "azure_registry_login_server" {
  description = "The registry's name for pulling and pushing images."
  value       = azurerm_container_registry.this.login_server
}

output "azure_key_vault_id" {
  description = "The Azure Key Vault, for the layers that read this one."
  value       = azurerm_key_vault.this.id
}

output "aws_client_security_group_id" {
  description = "Add this group to every AWS machine that needs the registry or the secret store."
  value       = module.private_access_aws.client_security_group_id
}

output "gcp_endpoint_address" {
  description = "The private address Google's APIs answer on inside the network."
  value       = module.private_access_gcp.endpoint_address
}

output "azure_key_vault_name" {
  description = "The Key Vault's name: the AZURE_KEY_VAULT_NAME variable of the GitHub environments."
  value       = azurerm_key_vault.this.name
}

output "azure_key_vault_resource_group_name" {
  description = "The Key Vault's resource group: the AZURE_KEY_VAULT_RESOURCE_GROUP variable of the GitHub environments."
  value       = azurerm_key_vault.this.resource_group_name
}
