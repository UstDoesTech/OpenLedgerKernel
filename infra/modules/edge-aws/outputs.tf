output "internet_gateway_id" {
  description = "The internet gateway's id."
  value       = aws_internet_gateway.this.id
}

output "route_table_id" {
  description = "The edge route table's id. Only the edge subnets are on it."
  value       = aws_route_table.edge.id
}
