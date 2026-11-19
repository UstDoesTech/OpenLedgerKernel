output "client_security_group_id" {
  description = "Add this group to every machine that needs the registry or the secret store."
  value       = aws_security_group.clients.id
}

output "endpoint_ids" {
  description = "The ids of the interface and gateway endpoints."
  value       = concat([for e in aws_vpc_endpoint.interface : e.id], [aws_vpc_endpoint.s3.id])
}
