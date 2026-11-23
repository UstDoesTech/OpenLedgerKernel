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

output "nodes" {
  description = "Each node's name, role and private address, and the identifier of the secret that holds its certificates. Never a certificate or a key. Whoever issues the certificates writes them there."
  value = [for i in range(3) : {
    name       = local.names[i]
    role       = i == 0 ? "primary" : "standby"
    address    = local.addresses[i]
    secret_ref = google_secret_manager_secret.tls[i].id
  }]
}
