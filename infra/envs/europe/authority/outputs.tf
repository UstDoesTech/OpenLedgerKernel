output "key_id" {
  description = "The KMS key that signs. The rotate-credentials workflow reads it from here and never prints it."
  value       = aws_kms_key.authority.arn
}

output "certificate_secret" {
  description = "The secret that holds the authority's public certificate."
  value       = aws_secretsmanager_secret.authority_certificate.arn
}
