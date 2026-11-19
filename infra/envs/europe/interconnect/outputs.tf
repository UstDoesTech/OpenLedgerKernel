output "tunnels" {
  description = "How many tunnels join each pair of networks."
  value       = module.interconnect.tunnels
}

output "required_preshared_key_names" {
  description = "The name of every tunnel's pre-shared key."
  value       = module.interconnect.required_preshared_key_names
}

output "vpn_endpoints" {
  description = "The public addresses of the platform's VPN gateways. Nothing else in the platform has one."
  value       = module.interconnect.vpn_endpoints
}

output "allowed_flows" {
  description = "The flows allowed between tiers, with their ports."
  value       = module.interconnect.allowed_flows
}
