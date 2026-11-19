output "resource_group_name" {
  description = "The resource group that holds the network."
  value       = azurerm_resource_group.this.name
}

output "network_id" {
  description = "The virtual network's id."
  value       = azurerm_virtual_network.this.id
}

output "network_name" {
  description = "The virtual network's name."
  value       = azurerm_virtual_network.this.name
}

output "network_cidr" {
  description = "The virtual network's address range."
  value       = var.plan.cidr
}

output "subnet_ids" {
  description = "Subnet ids by tier: a list of three, one per availability zone, in zone order."
  value       = { for tier in keys(var.plan.subnets) : tier => [for zone in range(3) : azurerm_subnet.this["${tier}-${zone}"].id] }
}

output "network_security_group_ids" {
  description = "The default-deny network security group for each tier."
  value       = { for tier, group in azurerm_network_security_group.tier : tier => group.id }
}

output "network_security_group_names" {
  description = "The name of the default-deny network security group for each tier. Rules added to them must be separate rule resources."
  value       = { for tier, group in azurerm_network_security_group.tier : tier => group.name }
}

output "flow_log_id" {
  description = "The virtual network flow log's id."
  value       = azurerm_network_watcher_flow_log.this.id
}
