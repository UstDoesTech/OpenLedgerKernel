output "endpoint_address" {
  description = "The private address Google's APIs answer on inside the network."
  value       = google_compute_global_address.apis.address
}
