output "host" {
  description = "The cluster's writer endpoint. It resolves to a private address."
  value       = aws_rds_cluster.this.endpoint
}

output "port" {
  description = "The cluster's port."
  value       = aws_rds_cluster.this.port
}

output "secret_ref" {
  description = "The ARN of the Secrets Manager secret that holds the administrator's credential."
  value       = aws_rds_cluster.this.master_user_secret[0].secret_arn
}
