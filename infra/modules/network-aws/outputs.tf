output "vpc_id" {
  description = "The VPC's id."
  value       = aws_vpc.this.id
}

output "vpc_cidr" {
  description = "The VPC's address range."
  value       = aws_vpc.this.cidr_block
}

output "subnet_ids" {
  description = "Subnet ids by tier: a list of three, one per availability zone, in zone order."
  value       = { for tier in keys(var.plan.subnets) : tier => [for zone in range(3) : aws_subnet.this["${tier}-${zone}"].id] }
}

output "route_table_id" {
  description = "The private route table every subnet uses. Private links add routes here."
  value       = aws_route_table.private.id
}

output "security_group_ids" {
  description = "The default-deny security group for each tier."
  value       = { for tier, group in aws_security_group.tier : tier => group.id }
}

output "flow_log_id" {
  description = "The VPC flow log's id."
  value       = aws_flow_log.this.id
}
