output "address" {
  description = "The machine's private address. Other cells and the customers' network call it here."
  value       = google_compute_instance.api.network_interface[0].network_ip
}

output "secret_ref" {
  description = "The id of the secret that holds this cell's API secrets. It has no version until whoever issues the certificates and tokens adds one."
  value       = google_secret_manager_secret.api.id
}

output "secret_files" {
  description = "The files the secret must hold, as entries of one JSON object."
  value       = module.boot.secret_files
}

output "tls_terminates" {
  description = "Where TLS ends: the service."
  value       = module.boot.tls_terminates
}
