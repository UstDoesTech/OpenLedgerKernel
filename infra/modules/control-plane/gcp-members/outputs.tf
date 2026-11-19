output "addresses" {
  description = "The members' private addresses, by member name."
  value       = { for k, i in google_compute_instance.member : k => i.network_interface[0].network_ip }
}

output "secret_refs" {
  description = "The Secret Manager secret of each member's certificates, by member name. Whoever issues the certificates adds a version here."
  value       = { for k, s in google_secret_manager_secret.tls : k => s.id }
}
