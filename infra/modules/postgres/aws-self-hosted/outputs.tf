output "host" {
  description = "The primary node's private address."
  value       = aws_instance.node[0].private_ip
}

output "port" {
  description = "The PostgreSQL port."
  value       = 5432
}

output "secret_ref" {
  description = "The ARN of the Secrets Manager secret that holds the administrator's credential. The primary's first start writes it."
  value       = aws_secretsmanager_secret.administrator.arn
}
