output "service_account" {
  description = "The service account's email. It is the GCP_SERVICE_ACCOUNT secret of the rotate-credentials environment."
  value       = google_service_account.this.email
}
