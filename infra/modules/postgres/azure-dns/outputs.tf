output "private_dns_zone_id" {
  description = "The zone's id, for the private_dns_zone_id input of Azure managed cells."
  value       = azurerm_private_dns_zone.postgres.id
}
