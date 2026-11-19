output "network_id" {
  description = "The VPC network's id."
  value       = google_compute_network.this.id
}

output "network_name" {
  description = "The VPC network's name."
  value       = google_compute_network.this.name
}

output "network_cidr" {
  description = "The VPC network's address range. Google networks have none of their own, so this is the plan's range that the subnets sit inside."
  value       = var.plan.cidr
}

output "subnet_ids" {
  description = "Subnet ids by tier: a list of three, one per availability zone, in zone order."
  value       = { for tier in keys(var.plan.subnets) : tier => [for zone in range(3) : google_compute_subnetwork.this["${tier}-${zone}"].id] }
}
