output "host" {
  description = "The instance's DNS name. Inside the network it resolves to the private endpoint."
  value       = trimsuffix(google_sql_database_instance.this.dns_name, ".")
}

output "port" {
  description = "The instance's port."
  value       = 5432
}

output "secret_ref" {
  description = "The service account that administers the instance. There is no password: it signs in with an IAM token."
  value       = "gcp-iam:${var.administrator_email}"
}
