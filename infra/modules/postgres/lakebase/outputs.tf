output "host" {
  description = "The primary endpoint's host. The workspace's Private Link makes it private."
  value       = databricks_postgres_endpoint.primary.status.hosts.host
}

output "port" {
  description = "The endpoint's port."
  value       = 5432
}

output "secret_ref" {
  description = "The service principal that administers the database. There is no password: it signs in with an OAuth token."
  value       = "databricks-oauth:${var.administrator_application_id}"
}
