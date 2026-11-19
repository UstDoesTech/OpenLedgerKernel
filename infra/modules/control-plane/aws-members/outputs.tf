output "addresses" {
  description = "The members' private addresses, by member name."
  value       = { for k, i in aws_instance.member : k => i.private_ip }
}

output "secret_refs" {
  description = "The ARN of each member's certificate secret, by member name. Whoever issues the certificates writes them here."
  value       = { for k, s in aws_secretsmanager_secret.tls : k => s.arn }
}
