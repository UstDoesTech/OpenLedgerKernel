output "endpoint_ids" {
  description = "The ids of the private endpoints, by target."
  value       = { for k, e in azurerm_private_endpoint.this : k => e.id }
}
