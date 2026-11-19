output "mechanism" {
  description = "How the networks are joined."
  value       = var.mechanism
}

output "tunnels" {
  description = "How many tunnels join each pair of networks. Every pair has at least two."
  value = {
    "aws-azure" = length(azurerm_virtual_network_gateway_connection.aws)
    "aws-gcp"   = length(google_compute_vpn_tunnel.aws)
    "azure-gcp" = length(google_compute_vpn_tunnel.azure)
  }
}

output "required_preshared_key_names" {
  description = "The name of every tunnel's pre-shared key. The preshared_keys input must hold one key for each."
  value       = local.preshared_key_names
}

output "vpn_endpoints" {
  description = "The public addresses of the platform's VPN gateways. Nothing else in the platform has one."
  value = {
    azure = azurerm_public_ip.vpn.ip_address
    gcp   = [for interface in google_compute_ha_vpn_gateway.this.vpn_interfaces : interface.ip_address]
  }
}

output "allowed_flows" {
  description = "The flows allowed between tiers, with their ports. Everything else is denied."
  value       = { for name, flow in local.flows : name => "${flow.source} to ${flow.destination} on ${local.flow_file.protocol}/${flow.port}" }
}
