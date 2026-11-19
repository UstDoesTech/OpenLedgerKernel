output "host" {
  description = "The primary node's private address."
  value       = google_compute_instance.node[0].network_interface[0].network_ip
}

output "port" {
  description = "The PostgreSQL port."
  value       = 5432
}

output "secret_ref" {
  description = "The Secret Manager secret that holds the administrator's credential. The primary's first start adds the version."
  value       = google_secret_manager_secret.administrator.id
}
