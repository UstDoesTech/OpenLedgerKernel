output "address" {
  description = "The machine's private address. Other cells and the customers' network call it here."
  value       = aws_instance.api.private_ip
}

output "secret_ref" {
  description = "The ARN of the secret that holds this cell's API secrets. It is empty until whoever issues the certificates and tokens fills it."
  value       = aws_secretsmanager_secret.api.arn
}

output "secret_files" {
  description = "The files the secret must hold, as entries of one JSON object."
  value       = module.boot.secret_files
}

output "tls_terminates" {
  description = "Where TLS ends: the service."
  value       = module.boot.tls_terminates
}
